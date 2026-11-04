//! The C interface for app hosts: `platform/ios/include/sena_mobile.h`.
//!
//! A session is a [`session::Session`]: the PAL engine with wgpu presentation
//! into the host's view, its input, and sound pulled by the host. The host
//! steps it once per display frame. Every entry point catches panics and
//! reports them as errors; all but the `sena_mobile_audio_*` calls belong on
//! the thread that opened the session.
#![allow(clippy::missing_safety_doc)]

pub mod audio;
pub mod log;
pub mod patch;
pub mod saves;
pub mod session;

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use pal_asset::Nls;
use pal_vm::config::TextLang;
use pal_vm::event::MouseButton;
use session::{Config, Session, Status};

/// `SENA_MOBILE_ABI_VERSION`.
pub const ABI_VERSION: u32 = 1;

/// `SenaMobileConfig`.
#[repr(C)]
pub struct SenaMobileConfig {
    pub struct_size: u32,
    pub game_dir: *const c_char,
    pub save_dir: *const c_char,
    pub encoding: u32,
    pub language: u32,
}

pub struct SenaMobileSession(Session);
pub struct SenaMobileAudio(audio::AudioOutput);

const ENCODING_AUTOMATIC: u32 = 0;
const ENCODING_SHIFT_JIS: u32 = 1;
const ENCODING_GBK: u32 = 2;
const ENCODING_UTF8: u32 = 3;

const LANGUAGE_AUTOMATIC: u32 = 0;
const LANGUAGE_JAPANESE: u32 = 1;
const LANGUAGE_SIMPLIFIED_CHINESE: u32 = 2;
const LANGUAGE_TRADITIONAL_CHINESE: u32 = 3;

const RUNNING: i32 = 0;
const ENDED: i32 = 1;
const FAILED: i32 = 2;

const POINTER_MOVE: i32 = 0;
const POINTER_LEFT_DOWN: i32 = 1;
const POINTER_LEFT_UP: i32 = 2;
const POINTER_RIGHT_DOWN: i32 = 3;
const POINTER_RIGHT_UP: i32 = 4;
const POINTER_MIDDLE_DOWN: i32 = 5;
const POINTER_MIDDLE_UP: i32 = 6;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(message: impl Into<String>) {
    let message = message.into().replace('\0', " ");
    log::emit(log::ERROR, &message);
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(message).unwrap_or_default());
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::default());
}

/// Runs `f`, turning a panic into an error and `fallback`.
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            set_error(format!("the engine stopped unexpectedly: {message}"));
            fallback
        }
    }
}

unsafe fn str_arg<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

fn status_code(status: &Status) -> i32 {
    match status {
        Status::Running => RUNNING,
        Status::Ended => ENDED,
        Status::Failed(message) => {
            set_error(message.clone());
            FAILED
        }
    }
}

#[no_mangle]
pub extern "C" fn sena_mobile_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn sena_mobile_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn sena_mobile_set_log_callback(
    callback: Option<log::Callback>,
    context: *mut c_void,
    max_level: i32,
) {
    guard((), || log::set(callback, context, max_level))
}

/// The index in `bytes` (a font or a collection) of `face`: a collection
/// index ("2"), a PostScript or family name, or "" for the first face.
fn face_index(bytes: &[u8], face: &str) -> Option<u32> {
    let face = face.trim();
    if face.is_empty() {
        return Some(0);
    }
    if let Ok(index) = face.parse::<u32>() {
        return Some(index);
    }
    let count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    (0..count).find(|&index| {
        ttf_parser::Face::parse(bytes, index).is_ok_and(|parsed| {
            parsed.names().into_iter().any(|name| {
                matches!(
                    name.name_id,
                    ttf_parser::name_id::POST_SCRIPT_NAME
                        | ttf_parser::name_id::FAMILY
                        | ttf_parser::name_id::FULL_NAME
                ) && name.to_string().is_some_and(|text| text == face)
            })
        })
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_register_font(path: *const c_char, face: *const c_char) -> i32 {
    guard(0, || {
        clear_error();
        let Some(path) = (unsafe { str_arg(path) }) else {
            set_error("no font path");
            return 0;
        };
        let face = unsafe { str_arg(face) }.unwrap_or("");
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                set_error(format!("{path}: {err}"));
                return 0;
            }
        };
        let Some(index) = face_index(&bytes, face) else {
            set_error(format!("{path} has no face {face}"));
            return 0;
        };
        if pal_vm::font::set_host_fallback_font(bytes, index) {
            log::emit(log::INFO, &format!("text face {path} #{index} {face}"));
            1
        } else {
            set_error(format!("{path} is not a usable font"));
            0
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_open(config: *const SenaMobileConfig) -> *mut SenaMobileSession {
    guard(std::ptr::null_mut(), || {
        clear_error();
        let Some(config) = (unsafe { config.as_ref() }) else {
            set_error("no configuration");
            return std::ptr::null_mut();
        };
        if (config.struct_size as usize) < std::mem::size_of::<SenaMobileConfig>() {
            set_error("configuration from an older header");
            return std::ptr::null_mut();
        }
        let Some(game_dir) = (unsafe { str_arg(config.game_dir) }) else {
            set_error("no game folder");
            return std::ptr::null_mut();
        };
        let game_dir = PathBuf::from(game_dir);
        let save_dir = unsafe { str_arg(config.save_dir) }
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from);
        if let Some(save_dir) = &save_dir {
            if let Err(err) = saves::adopt_original(&game_dir, save_dir) {
                log::emit(log::WARN, &format!("original saves not copied: {err:#}"));
            }
        }
        pal_vm::set_save_directory(save_dir);
        let found = patch::detect(&game_dir);
        if let Some(container) = &found.moe_container {
            log::emit(
                log::INFO,
                &format!(
                    "encrypted translation container {} registered; no host cipher configured",
                    container.display()
                ),
            );
        }
        let nls = match config.encoding {
            ENCODING_SHIFT_JIS => Nls::ShiftJis,
            ENCODING_GBK => Nls::Gbk,
            ENCODING_UTF8 => Nls::Utf8,
            ENCODING_AUTOMATIC | _ => found.nls.unwrap_or(Nls::ShiftJis),
        };
        let lang = match config.language {
            LANGUAGE_JAPANESE => TextLang::Jp,
            LANGUAGE_SIMPLIFIED_CHINESE => TextLang::Cn,
            LANGUAGE_TRADITIONAL_CHINESE => TextLang::Tc,
            LANGUAGE_AUTOMATIC | _ => {
                if patch::has_chinese_archives(&game_dir) {
                    TextLang::Cn
                } else {
                    TextLang::Jp
                }
            }
        };
        match Session::open(Config { game_dir, nls, lang, patch: found.patch }) {
            Ok(session) => Box::into_raw(Box::new(SenaMobileSession(session))),
            Err(err) => {
                set_error(format!("{err:#}"));
                std::ptr::null_mut()
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_screen_size(
    session: *const SenaMobileSession,
    width: *mut u32,
    height: *mut u32,
) {
    guard((), || {
        let size = unsafe { session.as_ref() }
            .map(|SenaMobileSession(s)| s.screen_size())
            .unwrap_or([1280, 720]);
        unsafe {
            if let Some(w) = width.as_mut() {
                *w = size[0];
            }
            if let Some(h) = height.as_mut() {
                *h = size[1];
            }
        }
    })
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
fn surface_target(view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    use raw_window_handle::{RawDisplayHandle, RawWindowHandle, UiKitDisplayHandle, UiKitWindowHandle};
    let view = std::ptr::NonNull::new(view)?;
    Some(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::UiKit(UiKitDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::UiKit(UiKitWindowHandle::new(view)),
    })
}

#[cfg(target_os = "macos")]
fn surface_target(view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};
    let view = std::ptr::NonNull::new(view)?;
    Some(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::AppKit(AppKitWindowHandle::new(view)),
    })
}

#[cfg(not(target_vendor = "apple"))]
fn surface_target(_view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    None
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_set_surface(
    session: *mut SenaMobileSession,
    ui_view: *mut c_void,
    width_px: u32,
    height_px: u32,
) -> i32 {
    guard(-1, || {
        clear_error();
        let Some(SenaMobileSession(s)) = (unsafe { session.as_mut() }) else {
            set_error("no session");
            return -1;
        };
        let result = s.attach([width_px, height_px], |instance| {
            let target = surface_target(ui_view)
                .ok_or_else(|| anyhow::anyhow!("views are not supported on this platform"))?;
            // SAFETY: the host keeps the view alive until it detaches or closes.
            Ok(unsafe { instance.create_surface_unsafe(target) }?)
        });
        match result {
            Ok(()) => 0,
            Err(err) => {
                set_error(format!("{err:#}"));
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_detach_surface(session: *mut SenaMobileSession) {
    guard((), || {
        if let Some(SenaMobileSession(s)) = unsafe { session.as_mut() } {
            s.detach();
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_step(session: *mut SenaMobileSession, dt_ms: u32) -> i32 {
    guard(FAILED, || {
        let Some(SenaMobileSession(s)) = (unsafe { session.as_mut() }) else {
            set_error("no session");
            return FAILED;
        };
        status_code(s.step(dt_ms))
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_pointer(
    session: *mut SenaMobileSession,
    phase: i32,
    x: f32,
    y: f32,
) {
    guard((), || {
        let Some(SenaMobileSession(s)) = (unsafe { session.as_mut() }) else {
            return;
        };
        let button = match phase {
            POINTER_LEFT_DOWN => Some((MouseButton::Left, true)),
            POINTER_LEFT_UP => Some((MouseButton::Left, false)),
            POINTER_RIGHT_DOWN => Some((MouseButton::Right, true)),
            POINTER_RIGHT_UP => Some((MouseButton::Right, false)),
            POINTER_MIDDLE_DOWN => Some((MouseButton::Middle, true)),
            POINTER_MIDDLE_UP => Some((MouseButton::Middle, false)),
            POINTER_MOVE | _ => None,
        };
        s.pointer(x, y, button);
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_wheel(session: *mut SenaMobileSession, notches: i32) {
    guard((), || {
        if let Some(SenaMobileSession(s)) = unsafe { session.as_mut() } {
            s.wheel(notches);
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_key(session: *mut SenaMobileSession, vk: i32, down: i32) {
    guard((), || {
        if let Some(SenaMobileSession(s)) = unsafe { session.as_mut() } {
            s.key(vk, down != 0);
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_set_focused(session: *mut SenaMobileSession, focused: i32) {
    guard((), || {
        if let Some(SenaMobileSession(s)) = unsafe { session.as_mut() } {
            s.set_focused(focused != 0);
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_snapshot(
    session: *mut SenaMobileSession,
    width: *mut u32,
    height: *mut u32,
) -> *const u8 {
    guard(std::ptr::null(), || {
        let Some(SenaMobileSession(s)) = (unsafe { session.as_mut() }) else {
            return std::ptr::null();
        };
        let Some(([w, h], pixels)) = s.snapshot() else {
            return std::ptr::null();
        };
        unsafe {
            if let Some(out) = width.as_mut() {
                *out = w;
            }
            if let Some(out) = height.as_mut() {
                *out = h;
            }
        }
        pixels.as_ptr()
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_audio(session: *mut SenaMobileSession) -> *mut SenaMobileAudio {
    guard(std::ptr::null_mut(), || {
        clear_error();
        let Some(SenaMobileSession(s)) = (unsafe { session.as_mut() }) else {
            set_error("no session");
            return std::ptr::null_mut();
        };
        match audio::AudioOutput::new(s.audio_sink()) {
            Ok(output) => Box::into_raw(Box::new(SenaMobileAudio(output))),
            Err(err) => {
                set_error(format!("audio: {err}"));
                std::ptr::null_mut()
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn sena_mobile_audio_sample_rate() -> u32 {
    audio::SAMPLE_RATE
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_audio_render(audio: *mut SenaMobileAudio, out: *mut f32, frames: u32) {
    if out.is_null() || frames == 0 {
        return;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * 2) };
    match unsafe { audio.as_mut() } {
        Some(SenaMobileAudio(output)) => output.render(out),
        None => out.fill(0.0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_audio_release(audio: *mut SenaMobileAudio) {
    guard((), || {
        if !audio.is_null() {
            drop(unsafe { Box::from_raw(audio) });
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn sena_mobile_close(session: *mut SenaMobileSession) {
    guard((), || {
        if !session.is_null() {
            drop(unsafe { Box::from_raw(session) });
        }
    })
}

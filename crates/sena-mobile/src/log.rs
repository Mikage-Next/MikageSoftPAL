//! Engine log records for the host: the `log` facade forwards to a C callback.
use std::ffi::{c_char, c_void, CString};
use std::sync::{Once, RwLock};

pub const ERROR: i32 = 1;
pub const WARN: i32 = 2;
pub const INFO: i32 = 3;
pub const DEBUG: i32 = 4;
pub const TRACE: i32 = 5;

pub type Callback =
    extern "C" fn(level: i32, target: *const c_char, message: *const c_char, context: *mut c_void);

struct Sink {
    callback: Callback,
    context: usize,
    max_level: i32,
}

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

struct Forwarder;

fn level_code(level: log::Level) -> i32 {
    match level {
        log::Level::Error => ERROR,
        log::Level::Warn => WARN,
        log::Level::Info => INFO,
        log::Level::Debug => DEBUG,
        log::Level::Trace => TRACE,
    }
}

impl log::Log for Forwarder {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let Ok(sink) = SINK.read() else { return false };
        sink.as_ref()
            .is_some_and(|sink| level_code(metadata.level()) <= sink.max_level)
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            emit_target(level_code(record.level()), record.target(), &record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// Installs the forwarder (once) and sets or clears the callback.
pub fn set(callback: Option<Callback>, context: *mut c_void, max_level: i32) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = log::set_logger(&Forwarder);
    });
    let max_level = max_level.clamp(0, TRACE);
    let sink = callback.map(|callback| Sink {
        callback,
        context: context as usize,
        max_level,
    });
    log::set_max_level(match (sink.is_some(), max_level) {
        (false, _) | (_, 0) => log::LevelFilter::Off,
        (_, ERROR) => log::LevelFilter::Error,
        (_, WARN) => log::LevelFilter::Warn,
        (_, INFO) => log::LevelFilter::Info,
        (_, DEBUG) => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    });
    match SINK.write() {
        Ok(mut s) => *s = sink,
        Err(p) => *p.into_inner() = sink,
    }
}

/// A record from this crate itself.
pub fn emit(level: i32, message: &str) {
    emit_target(level, "sena_mobile", message);
}

fn emit_target(level: i32, target: &str, message: &str) {
    let Ok(sink) = SINK.read() else { return };
    let Some(sink) = sink.as_ref() else { return };
    if level > sink.max_level {
        return;
    }
    let target = CString::new(target.replace('\0', " ")).unwrap_or_default();
    let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
    (sink.callback)(level, target.as_ptr(), message.as_ptr(), sink.context as *mut c_void);
}

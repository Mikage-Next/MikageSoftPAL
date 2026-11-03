//! Runs a game through the mobile session without a view, the way the app
//! drives it: fixed 16 ms steps, taps in logical pixels, PNG snapshots.
//!
//! sena-mobile-run <game> [--frames N] [--encoding auto|sjis|gbk] [--lang auto|jp|cn|tc]
//!     [--font PATH] [--tap FRAME:X:Y] [--right FRAME:X:Y] [--key FRAME:VK]
//!     [--wheel FRAME:NOTCHES] [--auto-click EVERY] [--png FRAME:PATH] [--log LEVEL]
use std::ffi::{c_char, c_void, CStr, CString};

use sena_mobile::*;

extern "C" fn print_log(level: i32, target: *const c_char, message: *const c_char, _: *mut c_void) {
    let target = unsafe { CStr::from_ptr(target) }.to_string_lossy();
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    eprintln!("[{level}] {target}: {message}");
}

fn parse3(v: &str) -> (u32, f32, f32) {
    let p: Vec<&str> = v.split(':').collect();
    (p[0].parse().unwrap(), p[1].parse().unwrap(), p[2].parse().unwrap())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let game = CString::new(args[1].clone()).unwrap();
    let (mut frames, mut encoding, mut language, mut auto_click, mut level) = (600u32, 0u32, 0u32, 0u32, 2i32);
    let (mut taps, mut rights, mut keys, mut wheels, mut pngs) = (vec![], vec![], vec![], vec![], vec![]);
    let mut font = None;
    let mut i = 2;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--frames" => frames = v.parse().unwrap(),
            "--encoding" => encoding = match v.as_str() { "sjis" => 1, "gbk" => 2, "utf8" => 3, _ => 0 },
            "--lang" => language = match v.as_str() { "jp" => 1, "cn" => 2, "tc" => 3, _ => 0 },
            "--font" => font = Some(v),
            "--tap" => taps.push(parse3(&v)),
            "--right" => rights.push(parse3(&v)),
            "--key" => { let p: Vec<&str> = v.split(':').collect(); keys.push((p[0].parse::<u32>().unwrap(), i32::from_str_radix(p[1].trim_start_matches("0x"), 16).unwrap())) }
            "--wheel" => { let p: Vec<&str> = v.split(':').collect(); wheels.push((p[0].parse::<u32>().unwrap(), p[1].parse::<i32>().unwrap())) }
            "--auto-click" => auto_click = v.parse().unwrap(),
            "--png" => { let (f, p) = v.split_once(':').unwrap(); pngs.push((f.parse::<u32>().unwrap(), p.to_owned())) }
            "--log" => level = v.parse().unwrap(),
            other => panic!("unknown option {other}"),
        }
        i += 2;
    }
    unsafe {
        sena_mobile_set_log_callback(Some(print_log), std::ptr::null_mut(), level);
        if let Some(font) = font {
            let path = CString::new(font).unwrap();
            assert_eq!(sena_mobile_register_font(path.as_ptr(), 0), 1);
        }
        let config = SenaMobileConfig {
            struct_size: std::mem::size_of::<SenaMobileConfig>() as u32,
            game_dir: game.as_ptr(),
            encoding,
            language,
        };
        let session = sena_mobile_open(&config);
        if session.is_null() {
            eprintln!("open failed: {}", CStr::from_ptr(sena_mobile_last_error()).to_string_lossy());
            std::process::exit(1);
        }
        let (mut w, mut h) = (0, 0);
        sena_mobile_screen_size(session, &mut w, &mut h);
        eprintln!("screen {w}x{h}");
        // Sound is pulled as the app's audio unit would (16 ms per frame):
        // button reactions and voices finish only as their sound plays.
        let audio = sena_mobile_audio(session);
        let mut pcm = vec![0f32; 768 * 2];
        // Without a surface the viewport is the logical screen.
        for frame in 0..frames {
            for &(f, x, y) in &taps {
                if f == frame + 3 { sena_mobile_pointer(session, 0, x, y); }
                if f == frame { sena_mobile_pointer(session, 1, x, y); }
                if f + 3 == frame { sena_mobile_pointer(session, 2, x, y); }
            }
            for &(f, x, y) in &rights {
                if f == frame { sena_mobile_pointer(session, 0, x, y); sena_mobile_pointer(session, 3, x, y); }
                if f + 2 == frame { sena_mobile_pointer(session, 4, x, y); }
            }
            for &(f, vk) in &keys {
                if f == frame { sena_mobile_key(session, vk, 1); }
                if f + 2 == frame { sena_mobile_key(session, vk, 0); }
            }
            for &(f, n) in &wheels { if f == frame { sena_mobile_wheel(session, n); } }
            if auto_click > 0 && frame % auto_click == 0 { sena_mobile_pointer(session, 1, w as f32 / 2.0, h as f32 - 80.0); }
            if auto_click > 0 && frame % auto_click == 2 { sena_mobile_pointer(session, 2, w as f32 / 2.0, h as f32 - 80.0); }
            let status = sena_mobile_step(session, 16);
            std::thread::sleep(std::time::Duration::from_millis(2));
            sena_mobile_audio_render(audio, pcm.as_mut_ptr(), 768);
            for (f, path) in &pngs {
                if *f == frame {
                    let (mut sw, mut sh) = (0, 0);
                    let px = sena_mobile_snapshot(session, &mut sw, &mut sh);
                    if !px.is_null() {
                        let data = std::slice::from_raw_parts(px, (sw * sh * 4) as usize);
                        let file = std::fs::File::create(path).unwrap();
                        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), sw, sh);
                        enc.set_color(png::ColorType::Rgba);
                        enc.set_depth(png::BitDepth::Eight);
                        enc.write_header().unwrap().write_image_data(data).unwrap();
                        eprintln!("frame {frame}: wrote {path}");
                    }
                }
            }
            if status != 0 {
                eprintln!("frame {frame}: status {status} {}", CStr::from_ptr(sena_mobile_last_error()).to_string_lossy());
                break;
            }
        }
        sena_mobile_close(session);
        sena_mobile_audio_release(audio);
    }
}

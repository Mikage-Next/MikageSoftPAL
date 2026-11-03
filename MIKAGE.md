# MikageSoftPAL

Mikage's fork of [sena-rs](https://github.com/xmoezzz/sena-rs), the Rust
SoftPAL engine, as the SoftPAL runtime of the Mikage iOS visual novel player.
Work happens on the `mikage` branch; `main` mirrors upstream.

## What the fork adds

- `crates/sena-mobile`: the C interface for the app
  (`platform/ios/include/sena_mobile.h`). A session runs one game: the PAL
  engine draws with the existing wgpu backend into the host's `CAMetalLayer`,
  letterboxed; sound is pulled by the host through a lock-free ring fed from
  the engine's Kira mixer; touches arrive in drawable pixels and are mapped
  to the game's logical screen; Windows virtual-key codes become the engine's
  key names; log records go to a host callback; every entry point catches
  panics. `sena-mobile-run` (feature `runner`) drives the same session
  headless for tests: taps, keys, wheel, PNG snapshots, audio pulled at 16 ms
  per frame.
- `pal-vm` feature `gpu`: the wgpu renderer without winit (`desktop` still
  includes it). `WgpuBackend::with_surface` takes a surface the host made,
  `resize_to` and `set_viewport` fit the scene into a letterbox.
- No bundled font. Upstream embedded a Microsoft font (`default.ttf`), which
  can't be redistributed; it is removed. The host registers its face with
  `sena_mobile_register_font` (Mikage: PingFang, Hiragino Sans fallback);
  desktop builds use the system CJK font paths. Tests need a system CJK font
  (Linux: `apt install fonts-noto-cjk`).
- `AudioSink::render_f32` for the pulled output.
- `pal_vm::set_save_directory`: saves (the original's files and the
  portable `sena_rs/` ones) can go to a folder of the host's instead of the
  game folder's `save/`. `SenaMobileConfig.save_dir` sets it; a new folder
  first gets a copy of the original's `save/`, so Windows saves still load.
- Older engine builds (魔女こいにっき, 2014): extcall 18:17 pops nothing and
  returns a value the game executable hides after a `/**/` marker, which the
  script's start-up check compares with a constant
  (`native_profile::read_game_token`). se_load, se_play and se_unload pop
  three, three and one values, as koikake.exe and MAJOKOI.exe do.
  `SENA_ARITY_AUDIT=1` prints extcalls whose handler popped a different
  count than the native one (`runtime/native_arity.rs`).
- Upstream's GitHub workflows are removed on this branch, so pushes do not
  start the console builds.

## Testing

```bash
cargo build --release -p sena-mobile --features runner --bin sena-mobile-run
target/release/sena-mobile-run <game> --frames 2600 --tap 1505:150:260 --png 2599:out.png
cargo check -p sena-mobile --target aarch64-apple-ios
```

Without `--saves DIR`, saves stay where the original engine keeps them: the
game folder's `save/`. Mikage passes the game folder's `savedata/`.

## Games

- 恋×シンアイ彼女 (Koikake, 2015, Japanese): title, GAME START and the first
  story lines headless through `sena-mobile`.
- 魔女こいにっき (2014, Japanese): logo, title, the prologue's narration.

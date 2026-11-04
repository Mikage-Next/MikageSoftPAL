use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pal_asset::{AssetSource, LoadedAsset, Nls, ResourceManager};
use pal_script::PointTable;
use pal_vm::{CoreAssets, FileDatLayout, RuntimeStatus, ScriptRuntime, ScriptRuntimeConfig};

struct SaveRoot(PathBuf);

impl SaveRoot {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sena-save-semantics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("save/sena_rs")).unwrap();
        Self(path)
    }

    fn write(&self, name: &str) {
        std::fs::write(self.0.join("save").join(name), []).unwrap();
    }
}

impl Drop for SaveRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn words(script: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        script.extend_from_slice(&value.to_le_bytes());
    }
}

fn call(script: &mut Vec<u8>, index: u16, args: &[i32], destination: u32) {
    for &value in args.iter().rev() {
        if value < 0 {
            words(script, &[0x10001, 0x4000_001F, value.unsigned_abs()]);
            words(script, &[0x1001D, 0x4000_001F]);
            words(script, &[0x1001F, 0x4000_001F]);
        } else {
            words(script, &[0x1001F, value as u32]);
        }
    }
    words(script, &[0x10017, 0x000A_0000 | index as u32, destination]);
}

fn assets(mut script: Vec<u8>) -> CoreAssets {
    let mut header = b"Sv20".to_vec();
    words(&mut header, &[0, 12]);
    header.append(&mut script);
    words(&mut header, &[0x10015]);
    let asset = |name: &str, bytes| LoadedAsset {
        name: name.to_owned(),
        bytes,
        source: AssetSource::Loose {
            path: PathBuf::from(name),
        },
    };
    CoreAssets {
        script: asset("Script.src", header),
        file_dat: asset("File.dat", vec![]),
        text_dat: asset("Text.dat", vec![]),
        mem_dat: asset("Mem.dat", vec![]),
        point_dat: asset("Point.dat", vec![]),
        graphic_dat: None,
        text_dat_multilingual: false,
        file_dat_layout: FileDatLayout::default(),
        extended_softpal: false,
            native_build: Default::default(),
            registered_softpal: false,
        script_check_value: 0,
        script_entry_pc: 12,
        point_table: PointTable::parse(&[]).unwrap(),
        graphic_index: None,
        game_token: None,
    }
}

fn run(root: &SaveRoot, script: Vec<u8>) -> ScriptRuntime {
    let assets = assets(script);
    let config = ScriptRuntimeConfig::default();
    let mut runtime = ScriptRuntime::boot(12, config.clone());
    let mut manager = ResourceManager::new(&root.0, Nls::ShiftJis);
    let tick = runtime
        .run_frame_with_resources(&assets, Some(&mut manager), None, None, None, None, &config)
        .unwrap();
    assert!(matches!(tick.status, RuntimeStatus::Halted { .. }));
    assert_eq!(runtime.stack_depth(), 0);
    runtime
}

#[test]
fn title_save_queries_distinguish_continue_slot_and_any_numbered_save() {
    // Koikake title calls is_save(998) and is_save(-1) separately. A normal
    // numbered save enables DATA LOAD without requiring the continue slot.
    let root = SaveRoot::new();
    root.write("save007.dat");
    let mut script = vec![];
    call(&mut script, 9, &[998], 0);
    call(&mut script, 9, &[-1], 1);
    call(&mut script, 9, &[7], 2);
    call(&mut script, 9, &[8], 3);
    let runtime = run(&root, script);
    assert_eq!(&runtime.vars()[..4], &[0, 1, 1, 0]);
}

#[test]
fn wildcard_save_query_ignores_continue_and_slots_outside_native_scan() {
    let root = SaveRoot::new();
    root.write("continue.dat");
    root.write("sena_rs/continue.sav");
    root.write("save1000.dat");
    let mut script = vec![];
    call(&mut script, 9, &[-1], 0);
    call(&mut script, 9, &[-2], 1);
    assert_eq!(&run(&root, script).vars()[..2], &[0, 0]);
}

#[test]
fn only_minus_one_is_the_wildcard_save_query() {
    let root = SaveRoot::new();
    root.write("save999.dat");
    let mut script = vec![];
    call(&mut script, 9, &[-1], 0);
    call(&mut script, 9, &[-2], 1);
    assert_eq!(&run(&root, script).vars()[..2], &[1, 0]);
    root.write("save-02.dat");
    let mut script = vec![];
    call(&mut script, 9, &[-2], 0);
    assert_eq!(run(&root, script).vars()[0], 1);
}

#[test]
fn wildcard_save_query_accepts_legacy_portable_saves() {
    let root = SaveRoot::new();
    root.write("sena_rs/save021.sav");
    let mut script = vec![];
    call(&mut script, 9, &[-1], 0);
    call(&mut script, 9, &[21], 1);
    assert_eq!(&run(&root, script).vars()[..2], &[1, 1]);
}

#[test]
fn remembered_save_slot_keeps_native_no_save_sentinel() {
    let root = SaveRoot::new();
    let mut script = vec![];
    call(&mut script, 17, &[], 0);
    call(&mut script, 11, &[0], 4);
    call(&mut script, 0, &[17, 1], 4);
    call(&mut script, 17, &[], 1);
    call(&mut script, 0, &[18, 0], 4);
    call(&mut script, 17, &[], 2);
    call(&mut script, 28, &[17], 4);
    call(&mut script, 17, &[], 3);
    let runtime = run(&root, script);
    assert_eq!(&runtime.vars()[..4], &[-1, 17, 17, -1]);
    assert!(!root.0.join("save/save017.dat").exists());
    assert!(root.0.join("save/save018.dat").exists());
}

#[test]
fn deleting_an_already_missing_save_still_clears_the_remembered_slot() {
    let root = SaveRoot::new();
    let mut script = vec![];
    call(&mut script, 11, &[0], 4);
    call(&mut script, 0, &[17, 1], 4);
    call(&mut script, 28, &[17], 0);
    call(&mut script, 17, &[], 1);
    let assets = assets(script);
    let config = ScriptRuntimeConfig {
        instructions_per_frame: 5,
        ..ScriptRuntimeConfig::default()
    };
    let mut runtime = ScriptRuntime::boot(12, config.clone());
    let mut manager = ResourceManager::new(&root.0, Nls::ShiftJis);
    runtime
        .run_frame_with_resources(&assets, Some(&mut manager), None, None, None, None, &config)
        .unwrap();
    std::fs::remove_file(root.0.join("save/save017.dat")).unwrap();
    runtime
        .run_frame_with_resources(&assets, Some(&mut manager), None, None, None, None, &config)
        .unwrap();
    assert_eq!(&runtime.vars()[..2], &[0, -1]);
}

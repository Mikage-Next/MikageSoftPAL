use std::path::PathBuf;

use pal_asset::{AssetSource, LoadedAsset};
use pal_script::PointTable;
use pal_vm::{CoreAssets, FileDatLayout, RuntimeStatus, ScriptRuntime, ScriptRuntimeConfig};

fn asset(name: &str, bytes: Vec<u8>) -> LoadedAsset {
    LoadedAsset {
        name: name.to_owned(),
        bytes,
        source: AssetSource::Loose {
            path: PathBuf::from(name),
        },
    }
}

fn opcode(op: u16) -> [u8; 4] {
    (0x0001_0000u32 | op as u32).to_le_bytes()
}

fn word(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

fn var(slot: u16) -> u32 {
    0x4000_0000u32 | slot as u32
}

#[test]
fn runtime_executes_basic_variable_arithmetic() {
    let mut script = Vec::new();
    script.extend_from_slice(b"Sv20");
    script.extend_from_slice(&word(0));
    script.extend_from_slice(&word(12));
    script.extend_from_slice(&opcode(1));
    script.extend_from_slice(&word(var(1)));
    script.extend_from_slice(&word(1));
    script.extend_from_slice(&opcode(2));
    script.extend_from_slice(&word(var(1)));
    script.extend_from_slice(&word(2));
    script.extend_from_slice(&opcode(21));

    let assets = CoreAssets {
        script: asset("Script.src", script),
        file_dat: asset("File.dat", Vec::new()),
        text_dat: asset("Text.dat", Vec::new()),
        mem_dat: asset("Mem.dat", Vec::new()),
        point_dat: asset("Point.dat", Vec::new()),
        graphic_dat: None,
        text_dat_multilingual: false,
        file_dat_layout: FileDatLayout::default(),
        extended_softpal: false,
            native_build: Default::default(),
            registered_softpal: false,
        script_check_value: 0,
        script_entry_pc: 12,
        point_table: PointTable::parse(&[]).expect("empty Point.dat should parse"),
        graphic_index: None,
        game_token: None,
    };

    let config = ScriptRuntimeConfig::default();
    let mut runtime = ScriptRuntime::boot(assets.script_entry_pc, config.clone());
    let tick = runtime
        .run_frame(&assets, &config)
        .expect("runtime should execute fixture");

    assert_eq!(tick.status, RuntimeStatus::Halted { pc: 40 });
    assert_eq!(runtime.vars()[1], 3);
}

#[test]
fn window_extcalls_reach_host_queue_and_keep_script_mode_cache() {
    let mut script = b"Sv20".to_vec();
    script.extend_from_slice(&word(0));
    script.extend_from_slice(&word(12));
    let ext = |script: &mut Vec<u8>, index: u32, args: &[u32], dst: u32| {
        for arg in args.iter().rev() {
            script.extend_from_slice(&opcode(31));
            script.extend_from_slice(&word(*arg));
        }
        script.extend_from_slice(&opcode(23));
        script.extend_from_slice(&word(0x90000 | index));
        script.extend_from_slice(&word(dst));
    };
    ext(&mut script, 36, &[800, 600], 1);
    ext(&mut script, 6, &[1], 2);
    ext(&mut script, 46, &[0], 3);
    ext(&mut script, 9, &[], 4);
    script.extend_from_slice(&opcode(21));
    let assets = CoreAssets {
        script: asset("Script.src", script),
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
    };
    let config = ScriptRuntimeConfig::default();
    let mut runtime = ScriptRuntime::boot(12, config.clone());
    runtime.observe_window_geometry(20, 30, 1280, 720, false);
    assert!(runtime.take_window_requests().is_empty());
    runtime.run_frame(&assets, &config).unwrap();
    assert_eq!(&runtime.vars()[1..5], &[1, 1, 1, 1]);
    assert_eq!(
        runtime.take_window_requests(),
        vec![pal_vm::PalWindowRequest::ChangeSize {
            width: 800,
            height: 450
        }],
        "disabled fullscreen request must not reach the host"
    );
    runtime.observe_window_geometry(20, 30, 800, 450, false);
    assert!(runtime.take_window_requests().is_empty());
}

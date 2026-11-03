//! Game.exe 0x42C460: history_get_text(record, name_dst, voice_dst, body_dst).
use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct FrozenHistoryText {
    pub(super) name: String,
    pub(super) voice: String,
    pub(super) body: String,
}

impl ScriptRuntime {
    pub(super) fn resolve_history_text(
        &self,
        args: [i32; 4],
        assets: &CoreAssets,
        nls: Nls,
    ) -> FrozenHistoryText {
        FrozenHistoryText {
            name: self
                .resolve_script_string(args[2], assets, nls)
                .unwrap_or_default(),
            body: self
                .resolve_script_string(args[1], assets, nls)
                .unwrap_or_default(),
            voice: if args[3] == 0 || args[3] == 0x0fff_ffff {
                String::new()
            } else {
                self.resolve_resource_string(args[3], assets, nls)
                    .unwrap_or_default()
            },
        }
    }

    pub(super) fn history_record_text(
        &self,
        index: usize,
        assets: &CoreAssets,
        nls: Nls,
    ) -> FrozenHistoryText {
        if let Some(Some(text)) = self.history_state.texts.get(index) {
            return text.clone();
        }
        // Versions 1–7 contain only source handles. Preserve their static
        // resource fallback; lost dynamic-buffer contents cannot be recovered.
        let record = self.history_state.records[index];
        let mut args = [record[0], record[1], record[2], record[3]];
        for value in &mut args[1..] {
            if is_dynamic_string_handle(*value) {
                *value = 0x0fff_ffff;
            }
        }
        self.resolve_history_text(args, assets, nls)
    }

    pub(super) fn replay_history_voice(
        &mut self,
        name: &str,
        mut resources: Option<&mut ResourceManager>,
        mut audio: Option<&mut AudioSystem>,
    ) {
        if !self.text_state.voice_enabled
            || name.is_empty()
            || is_resource_clear_sentinel(name)
            || name.to_ascii_lowercase().starts_with("http:")
            || name.to_ascii_lowercase().starts_with("https:")
        {
            return;
        }
        if let Some(audio) = audio.as_deref_mut() {
            self.release_adv_voice_slots(audio);
        }
        let percent = resources
            .as_deref_mut()
            .and_then(|rm| self.voice_unit_slot_for_name(rm, name))
            .map(|slot| self.voice_unit_channel_percent(slot))
            .unwrap_or(100);
        self.load_voice_audio(
            0,
            PalSoundGroup::GROUP1,
            name,
            0,
            percent,
            true,
            0,
            resources,
            audio,
        );
    }

    pub(super) fn dispatch_history_get_text(
        &mut self,
        assets: &CoreAssets,
        nls: Nls,
    ) -> ExtCallOutcome {
        let args = self.pop_ext_args(4);
        if args.len() != 4 {
            return ExtCallOutcome::Block;
        }
        // Native validates all three destinations before writing any of them.
        if !args[1..]
            .iter()
            .all(|value| is_dynamic_string_handle(*value))
        {
            return ExtCallOutcome::Value(0);
        }
        // Native shifts older entries up on insertion; our storage appends.
        let Some(index) = usize::try_from(args[0])
            .ok()
            .and_then(|index| self.history_state.records.len().checked_sub(index + 1))
        else {
            // Native reads outside its array for invalid indices. Fail safely.
            return ExtCallOutcome::Value(0);
        };
        let record = self.history_state.records[index];
        let text = self.history_record_text(index, assets, nls);
        self.replace_dynamic_string(args[1], text.name);
        self.replace_dynamic_string(args[2], text.voice);
        self.replace_dynamic_string(args[3], text.body);
        // ctx+92868 = task+805048 = ctx+804224 (saved instruction PC).
        ExtCallOutcome::Value(record[4])
    }
}

// A native body has 4096 bytes; UTF-8 can require three bytes per UTF-16
// code unit. Keep the portable cap large enough without unbounded allocations.
const HISTORY_STRING_CAP: usize = 16_384;

pub(super) fn write_history_texts(
    out: &mut Vec<u8>,
    snapshot: &RuntimeSaveSnapshot,
) -> std::io::Result<()> {
    write_u32(out, snapshot.history_records.len() as u32)?;
    for index in 0..snapshot.history_records.len() {
        let text = snapshot.history_texts.get(index).and_then(Option::as_ref);
        out.push(u8::from(text.is_some()));
        if let Some(text) = text {
            for value in [&text.name, &text.voice, &text.body] {
                if value.len() > HISTORY_STRING_CAP {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "history string too long",
                    ));
                }
                write_bytes(out, value.as_bytes(), HISTORY_STRING_CAP)?;
            }
        }
    }
    Ok(())
}

pub(super) fn read_history_texts(
    reader: &mut impl Read,
    expected: usize,
) -> std::io::Result<Vec<Option<FrozenHistoryText>>> {
    let count = read_u32_from(reader)? as usize;
    if count != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "history string count mismatch",
        ));
    }
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let mut present = [0];
        reader.read_exact(&mut present)?;
        values.push(match present[0] {
            0 => None,
            1 => {
                let mut read_string = || -> std::io::Result<String> {
                    String::from_utf8(read_bytes_capped(reader, HISTORY_STRING_CAP)?)
                        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
                };
                Some(FrozenHistoryText {
                    name: read_string()?,
                    voice: read_string()?,
                    body: read_string()?,
                })
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid history presence flag",
                ))
            }
        });
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> CoreAssets {
        let asset = |name: &str, bytes| LoadedAsset {
            name: name.into(),
            bytes,
            source: AssetSource::Loose {
                path: PathBuf::from(name),
            },
        };
        CoreAssets {
            script: asset("Script.src", Vec::new()),
            file_dat: asset("File.dat", Vec::new()),
            text_dat: asset("Text.dat", Vec::new()),
            mem_dat: asset("Mem.dat", Vec::new()),
            point_dat: asset("Point.dat", Vec::new()),
            graphic_dat: None,
            text_dat_multilingual: false,
            file_dat_layout: FileDatLayout::default(),
            script_check_value: 0,
            script_entry_pc: 12,
            extended_softpal: false,
            native_build: Default::default(),
            registered_softpal: false,
            game_token: None,
            point_table: PointTable::parse(&[]).unwrap(),
            graphic_index: None,
        }
    }

    fn query(vm: &mut ScriptRuntime, assets: &CoreAssets, args: [i32; 4]) -> i32 {
        vm.stack.extend(args.into_iter().rev());
        match vm.dispatch_history_get_text(assets, Nls::ShiftJis) {
            ExtCallOutcome::Value(value) => value,
            _ => panic!("expected history result"),
        }
    }

    #[test]
    fn history_query_returns_newest_record_strings_and_instruction_pc() {
        let assets = assets();
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.text_state.history_enabled = true;
        let old_body = vm.store_dynamic_string("old body".into());
        let body = vm.store_dynamic_string("Body \u{03BB}<r>Next line".into());
        let name = vm.store_dynamic_string("Speaker".into());
        let voice = vm.store_dynamic_string("vo01_001".into());
        vm.pc = 0x10c;
        vm.push_history_text_record(
            [5, old_body, 0x0fff_ffff, 0x0fff_ffff],
            &assets,
            Nls::ShiftJis,
        );
        vm.pc = 0x20c;
        // Exercise the text handler's source/pop-order conversion too.
        vm.stack.extend([9, body, name, voice]);
        vm.dispatch_text_stub(2, &assets, Nls::ShiftJis, None, None);
        let dst = [
            vm.store_dynamic_string(String::new()),
            vm.store_dynamic_string(String::new()),
            vm.store_dynamic_string(String::new()),
        ];
        assert_eq!(query(&mut vm, &assets, [0, dst[0], dst[1], dst[2]]), 0x200);
        assert_eq!(
            vm.resolve_script_string(dst[0], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("Speaker")
        );
        assert_eq!(
            vm.resolve_script_string(dst[1], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("vo01_001")
        );
        assert_eq!(
            vm.resolve_script_string(dst[2], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("Body \u{03BB}<r>Next line")
        );
        assert_eq!(query(&mut vm, &assets, [1, dst[0], dst[1], dst[2]]), 0x100);
        assert_eq!(
            vm.resolve_script_string(dst[0], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("")
        );
        assert_eq!(
            vm.resolve_script_string(dst[1], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("")
        );
        assert_eq!(
            vm.resolve_script_string(dst[2], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("old body")
        );
    }

    #[test]
    fn history_query_rejects_invalid_destinations_and_indices_atomically() {
        let assets = assets();
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.history_state
            .records
            .push([0, 0x0fff_ffff, 0x0fff_ffff, 0, 123, 0, 0, 0, 0]);
        let dst = vm.store_dynamic_string("unchanged".into());
        for args in [[0, dst, 7, dst], [-1, dst, dst, dst], [1, dst, dst, dst]] {
            assert_eq!(query(&mut vm, &assets, args), 0);
            assert_eq!(
                vm.resolve_script_string(dst, &assets, Nls::ShiftJis)
                    .as_deref(),
                Some("unchanged")
            );
            assert!(vm.stack.is_empty());
        }
    }

    #[test]
    fn history_query_sv20_extcall_returns_script_byte_offset() {
        let config = ScriptRuntimeConfig::default();
        let mut vm = ScriptRuntime::boot(12, config.clone());
        vm.text_state.history_enabled = true;
        let body = vm.store_dynamic_string("Body \u{03BB}".into());
        let name = vm.store_dynamic_string("Speaker".into());
        let voice = vm.store_dynamic_string("vo02_001".into());
        let dst = [
            vm.store_dynamic_string(String::new()),
            vm.store_dynamic_string(String::new()),
            vm.store_dynamic_string(String::new()),
        ];
        let mut script = b"Sv20".to_vec();
        let words = |script: &mut Vec<u8>, values: &[u32]| {
            for word in values {
                script.extend_from_slice(&word.to_le_bytes());
            }
        };
        words(&mut script, &[0, 12]);
        for (slot, arg) in [0, body, name, voice].into_iter().enumerate() {
            vm.vars[100 + slot] = arg;
            words(&mut script, &[0x1001f, 0x4000_0064 + slot as u32]);
        }
        let text_pc = script.len() as i32;
        words(&mut script, &[0x10017, 0x20002, 0]);
        for (slot, arg) in [dst[2], dst[1], dst[0], 0].into_iter().enumerate() {
            vm.vars[104 + slot] = arg;
            words(&mut script, &[0x1001f, 0x4000_0068 + slot as u32]);
        }
        words(&mut script, &[0x10017, 0xe0014, 7, 0x10015]);
        let mut assets = assets();
        assets.script.bytes = script;
        let first = vm.run_frame(&assets, &config).unwrap();
        vm.status = RuntimeStatus::Running { pc: vm.pc };
        let second = vm.run_frame(&assets, &config).unwrap();
        assert_eq!(
            vm.vars[7], text_pc,
            "first={first:?} second={second:?} records={:?} stack={:?}",
            vm.history_state.records, vm.stack
        );
        assert_eq!(
            vm.resolve_script_string(dst[0], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("Speaker")
        );
        assert_eq!(
            vm.resolve_script_string(dst[1], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("vo02_001")
        );
        assert_eq!(
            vm.resolve_script_string(dst[2], &assets, Nls::ShiftJis)
                .as_deref(),
            Some("Body \u{03BB}")
        );
    }

    #[test]
    fn history_frozen_strings_survive_buffer_reuse_and_save_restore() {
        let assets = assets();
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.text_state.history_enabled = true;
        let body = vm.store_dynamic_string("First body".into());
        let name = vm.store_dynamic_string("First speaker".into());
        let voice = vm.store_dynamic_string("vo01_first".into());
        vm.pc = 0x10c;
        vm.push_history_text_record([0, body, name, voice], &assets, Nls::ShiftJis);
        vm.replace_dynamic_string(body, "Second body".into());
        vm.replace_dynamic_string(name, "Second speaker".into());
        vm.replace_dynamic_string(voice, "vo02_second".into());
        vm.pc = 0x20c;
        // The same handles can carry a new line; it must not be deduplicated.
        vm.push_history_text_record([0, body, name, voice], &assets, Nls::ShiftJis);
        vm.replace_dynamic_string(body, "destroyed".into());
        vm.replace_dynamic_string(name, "destroyed".into());
        vm.replace_dynamic_string(voice, "destroyed".into());
        assert_eq!(
            vm.history_record_text(0, &assets, Nls::ShiftJis).body,
            "First body"
        );
        assert_eq!(
            vm.history_record_text(1, &assets, Nls::ShiftJis).voice,
            "vo02_second"
        );
        vm.history_state.rect = [0, 0, 1280, 720];
        vm.rebuild_history_layout(&assets, Nls::ShiftJis);
        let layout = vm.history_state.layout_cache.as_ref().unwrap();
        assert_eq!(layout.records[0].name.as_deref(), Some("First speaker"));
        assert_eq!(layout.records[0].body_lines.concat(), "First body");
        assert!(layout.records[0].has_voice);

        let bytes = encode_runtime_save_snapshot(&vm.capture_save_snapshot()).unwrap();
        let snapshot = decode_runtime_save_snapshot(&mut bytes.as_slice()).unwrap();
        assert_eq!(snapshot.version, PORTABLE_SAVE_VERSION);
        let mut restored = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        restored.restore_save_snapshot(snapshot);
        // The current format restores live buffers as well as frozen history.
        // Queries below must still return the earlier captured history strings.
        assert_eq!(restored.dynamic_strings, vm.dynamic_strings);
        let saved_buffer_count = restored.dynamic_strings.len();
        let dst = [
            restored.store_dynamic_string(String::new()),
            restored.store_dynamic_string(String::new()),
            restored.store_dynamic_string(String::new()),
        ];
        assert_eq!(
            query(&mut restored, &assets, [1, dst[0], dst[1], dst[2]]),
            0x100
        );
        assert_eq!(
            &restored.dynamic_strings[saved_buffer_count..],
            ["First speaker", "vo01_first", "First body"]
        );
        assert_eq!(
            query(&mut restored, &assets, [0, dst[0], dst[1], dst[2]]),
            0x200
        );
        assert_eq!(
            &restored.dynamic_strings[saved_buffer_count..],
            ["Second speaker", "vo02_second", "Second body"]
        );
    }

    #[test]
    fn history_frozen_storage_evicts_and_clears_with_metadata() {
        let assets = assets();
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.text_state.history_enabled = true;
        let body = vm.store_dynamic_string(String::new());
        for line in 0..130 {
            vm.replace_dynamic_string(body, format!("Line {line}"));
            vm.push_history_text_record(
                [0, body, 0x0fff_ffff, 0x0fff_ffff],
                &assets,
                Nls::ShiftJis,
            );
        }
        assert_eq!(vm.history_state.records.len(), 128);
        assert_eq!(vm.history_state.texts.len(), 128);
        assert_eq!(
            vm.history_record_text(0, &assets, Nls::ShiftJis).body,
            "Line 2"
        );
        assert_eq!(
            vm.history_record_text(127, &assets, Nls::ShiftJis).body,
            "Line 129"
        );
        vm.dispatch_history_stub(11, &assets, Nls::ShiftJis, None, None, None);
        assert!(vm.history_state.texts.is_empty());
        assert!(vm.history_state.records.is_empty());
    }

    #[test]
    fn history_version_seven_without_frozen_strings_is_still_readable() {
        let snapshot = RuntimeSaveSnapshot {
            history_records: vec![[0, 1, 2, 3, 4, 5, 6, 7, 8]],
            ..RuntimeSaveSnapshot::default()
        };
        let mut bytes = encode_runtime_save_snapshot(&snapshot).unwrap();
        bytes[8..12].copy_from_slice(&7u32.to_le_bytes());
        // Locate the old format boundary without depending on later suffixes.
        let mut cursor = std::io::Cursor::new(&bytes);
        decode_runtime_save_snapshot(&mut cursor).unwrap();
        let old_length = cursor.position() as usize;
        bytes.truncate(old_length);
        let legacy = decode_runtime_save_snapshot(&mut bytes.as_slice()).unwrap();
        assert_eq!(legacy.version, 7);
        assert_eq!(legacy.history_records, snapshot.history_records);
        assert!(legacy.history_texts.is_empty());
        let mut runtime = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        runtime.restore_save_snapshot(legacy);
        assert_eq!(runtime.history_state.texts, vec![None]);
    }

    #[test]
    fn legacy_dynamic_history_never_reads_unrelated_current_buffers() {
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        let handle = vm.store_dynamic_string("unrelated current scene".into());
        vm.history_state
            .records
            .push([0, handle, handle, handle, 0, 0, 0, 0, 0]);
        assert_eq!(
            vm.history_record_text(0, &assets(), Nls::ShiftJis),
            FrozenHistoryText::default()
        );
    }

    #[test]
    fn history_save_rejects_invalid_utf8_and_mismatched_record_count() {
        let mut bytes = Vec::new();
        write_u32(&mut bytes, 1).unwrap();
        bytes.push(1);
        write_bytes(&mut bytes, &[0xff], HISTORY_STRING_CAP).unwrap();
        assert!(read_history_texts(&mut bytes.as_slice(), 1).is_err());
        assert!(read_history_texts(&mut bytes.as_slice(), 2).is_err());
    }

    #[test]
    fn history_query_short_stack_is_blocked() {
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.stack.push(0);
        assert!(matches!(
            vm.dispatch_history_get_text(&assets(), Nls::ShiftJis),
            ExtCallOutcome::Block
        ));
    }
}

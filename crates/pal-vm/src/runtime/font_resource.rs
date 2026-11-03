use super::*;

impl ScriptRuntime {
    pub(super) fn dispatch_font_resource(
        &mut self,
        index: u16,
        assets: &CoreAssets,
        nls: Nls,
        resources: Option<&mut ResourceManager>,
    ) -> ExtCallOutcome {
        if index == 16 {
            self.font_state.unload_bitmap_font();
            self.text_state.dirty = true;
            self.history_state.layout_cache = None;
            return ExtCallOutcome::Value(1);
        }
        let args = self.pop_ext_args(1);
        if args.len() != 1 {
            return ExtCallOutcome::Block;
        }
        let name = self
            .resolve_resource_string(args[0], assets, nls)
            .unwrap_or_default();
        if index == 20 {
            // Native unloads the old extended sheet before attempting a load.
            self.text_state.wait_mark_sheet = None;
            self.text_state.wait_mark_missing = true;
            self.font_state.set_ex_font_loaded(false);
        }
        let Some(resources) = resources else {
            return ExtCallOutcome::Value(1);
        };
        let extensions = if index == 15 {
            FONT_DATA_EXTENSIONS
        } else {
            FONT_SHEET_EXTENSIONS
        };
        let result = (|| -> anyhow::Result<()> {
            let asset = open_resource_variant(resources, &name, extensions)?;
            if index == 15 {
                self.font_state
                    .load_named_bitmap_font(&asset.name, asset.bytes, nls)
                    .map_err(anyhow::Error::msg)
            } else {
                self.font_state
                    .load_extended_font(&asset.bytes)
                    .map_err(anyhow::Error::msg)?;
                let sheet = decode_image(&asset.bytes)?;
                self.text_state.wait_mark_sheet = Some(sheet);
                self.text_state.wait_mark_missing = false;
                self.font_state.set_ex_font_loaded(true);
                Ok(())
            }
        })();
        if let Err(error) = result {
            log::warn!("[trace-font] font resource {name:?} failed: {error}");
        }
        self.text_state.dirty = true;
        self.history_state.layout_cache = None;
        // Game handlers return status 1 even if the PAL resource loader fails.
        ExtCallOutcome::Value(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> CoreAssets {
        let empty = || LoadedAsset {
            name: String::new(),
            bytes: Vec::new(),
            source: AssetSource::Loose {
                path: PathBuf::new(),
            },
        };
        CoreAssets {
            script: empty(),
            file_dat: empty(),
            text_dat: empty(),
            mem_dat: empty(),
            point_dat: empty(),
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

    #[test]
    fn script_font_load_replaces_metrics_and_unload_preserves_extended_sheet() {
        let root = std::env::temp_dir().join(format!(
            "pal-font-resource-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut bitmap = vec![0; 0x108];
        bitmap[0x80..0x84].copy_from_slice(&0x108u32.to_le_bytes());
        bitmap[0x104..0x108].copy_from_slice(&0x108u32.to_le_bytes());
        for value in [2u32, 2, 1, 2, 4, 8] {
            bitmap.extend_from_slice(&value.to_le_bytes());
        }
        bitmap.extend_from_slice(&[64, 0, 0, 0, 0, 32, 0, 0]);
        bitmap.extend_from_slice(&28i32.to_le_bytes());
        bitmap.extend_from_slice(&24i32.to_le_bytes());
        bitmap.extend_from_slice(&[0; 48]);
        std::fs::write(root.join("custom.dat"), bitmap).unwrap();
        let mut tga = vec![0; 18];
        tga[2] = 3;
        tga[12..14].copy_from_slice(&4u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 8;
        tga.extend_from_slice(&[255; 8]);
        tga.extend_from_slice(&[0; 26]);
        std::fs::write(root.join("marks.tga"), tga).unwrap();
        let mut resources = ResourceManager::new(&root, Nls::ShiftJis);
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        let assets = assets();
        let name = vm.store_dynamic_string("custom".into());
        vm.stack.extend([0x1234, name]);
        assert!(matches!(
            vm.dispatch_font_resource(15, &assets, Nls::ShiftJis, Some(&mut resources)),
            ExtCallOutcome::Value(1)
        ));
        assert_eq!(vm.stack, [0x1234]);
        assert!(vm.font_state.set_type(4));
        vm.font_state.set_font_size(28);
        assert_eq!(vm.font_state.measure("A"), (4, 28));
        let name = vm.store_dynamic_string("marks".into());
        vm.stack.push(name);
        vm.dispatch_font_resource(20, &assets, Nls::ShiftJis, Some(&mut resources));
        assert_eq!(
            vm.text_state
                .wait_mark_sheet
                .as_ref()
                .map(|sheet| (sheet.width, sheet.height)),
            Some((4, 2))
        );
        vm.dispatch_font_resource(16, &assets, Nls::ShiftJis, None);
        assert_ne!(vm.font_state.measure("A"), (4, 28));
        assert!(vm.text_state.wait_mark_sheet.is_some());
        let name = vm.store_dynamic_string("missing".into());
        vm.stack.push(name);
        vm.dispatch_font_resource(20, &assets, Nls::ShiftJis, Some(&mut resources));
        assert!(vm.text_state.wait_mark_sheet.is_none());
        assert!(vm.text_state.wait_mark_missing);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_font_color_words_keep_their_alpha_bits() {
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        vm.stack.extend([0x0012_3456, 0x8076_5432_u32 as i32]);
        assert!(matches!(
            vm.dispatch_font_system_stub(19, None),
            ExtCallOutcome::Value(1)
        ));
        let (first, second) = vm.font_state.color();
        assert_eq!([first, second], [0x8076_5432, 0x0012_3456]);
        assert!(vm.stack.is_empty());
    }
}

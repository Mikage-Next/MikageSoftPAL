//! PAL input adapters. Clear wrapper: Koikake0x428090 and Totsulover0x465860.
use super::*;

#[derive(Clone, Debug, Default)]
pub(super) struct InputControlState {
    keys: u32,
    extended: u32,
    cleared_this_frame: bool,
    clear_pending: bool,
    repeat_clear_pending: bool,
    repeat_cleared_this_frame: bool,
    suppressed_keys: u32,
    suppressed_extended: u32,
}

impl ScriptRuntime {
    pub(super) fn filter_script_input(&self, input: &PalInputState) -> PalInputState {
        let mut input = input.clone();
        input.set_cancel_masks(
            self.input_control.keys | self.input_control.suppressed_keys,
            self.input_control.extended | self.input_control.suppressed_extended,
        );
        if self.input_control.cleared_this_frame {
            input.clear();
        }
        if self.input_control.repeat_cleared_this_frame {
            input.clear_repeat();
        }
        input
    }

    pub(crate) fn apply_input_control(&mut self, input: &mut PalInputState) {
        input.set_cancel_masks(
            self.input_control.keys | self.input_control.suppressed_keys,
            self.input_control.extended | self.input_control.suppressed_extended,
        );
        input.set_cancel_masks(self.input_control.keys, self.input_control.extended);
        if std::mem::take(&mut self.input_control.clear_pending) {
            input.clear();
        }
        if std::mem::take(&mut self.input_control.repeat_clear_pending) {
            input.clear_repeat();
        }
        self.input_control.suppressed_keys = 0;
        self.input_control.suppressed_extended = 0;
    }

    pub(super) fn begin_script_input_frame(&mut self) {
        self.input_control.cleared_this_frame = false;
        self.input_control.repeat_cleared_this_frame = false;
        self.input_control.suppressed_keys = self.input_control.keys;
        self.input_control.suppressed_extended = self.input_control.extended;
    }

    pub(super) fn dispatch_input_control(
        &mut self,
        index: u16,
        input: Option<&PalInputState>,
    ) -> ExtCallOutcome {
        let count = match index {
            18 => 2,
            54 => 1,
            32..=35 => 0,
            _ => return ExtCallOutcome::Skip,
        };
        let args = self.pop_ext_args(count);
        if args.len() != count {
            return ExtCallOutcome::Block;
        }
        match index {
            18 => {
                self.input_control.keys = args[0] as u32;
                self.input_control.extended = args[1] as u32;
                self.input_control.suppressed_keys |= args[0] as u32;
                self.input_control.suppressed_extended |= args[1] as u32;
                if let Some(input) = input {
                    self.last_skip_state = self.skip_state(&self.filter_script_input(input));
                }
                ExtCallOutcome::Value(1)
            }
            35 => {
                self.input_control.cleared_this_frame = true;
                self.input_control.clear_pending = true;
                self.input_control.repeat_clear_pending = true;
                self.input_control.repeat_cleared_this_frame = true;
                if let Some(input) = input {
                    self.last_skip_state = self.skip_state(&self.filter_script_input(input));
                }
                ExtCallOutcome::Continue
            }
            54 => ExtCallOutcome::Value(i32::from(
                input.is_some_and(|input| input.async_virtual_key(args[0] as u32)),
            )),
            _ => {
                let input = input.map(|input| self.filter_script_input(input));
                ExtCallOutcome::Value(input.as_ref().map_or(0, |input| match index {
                    32 => input.raw_key_pull(),
                    33 => input.raw_key_on(),
                    34 => input.raw_key_push(),
                    _ => 0,
                }) as i32)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(script: Vec<u8>) -> CoreAssets {
        let asset = |name: &str, bytes| LoadedAsset {
            name: name.into(),
            bytes,
            source: AssetSource::Loose {
                path: PathBuf::from(name),
            },
        };
        CoreAssets {
            script: asset("Script.src", script),
            file_dat: asset("File.dat", vec![]),
            text_dat: asset("Text.dat", vec![]),
            mem_dat: asset("Mem.dat", vec![]),
            point_dat: asset("Point.dat", vec![]),
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
    fn real_script_cancel_clear_and_getters_share_one_input_sample() {
        let mut bytes = b"Sv20".to_vec();
        let words = |bytes: &mut Vec<u8>, words: &[u32]| {
            for word in words {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        };
        words(&mut bytes, &[0, 12]);
        let ext = |bytes: &mut Vec<u8>, index: u32, args: &[u32], dst| {
            for arg in args.iter().rev() {
                words(bytes, &[0x1001f, *arg]);
            }
            words(bytes, &[0x10017, 0x90000 | index, dst]);
        };
        ext(&mut bytes, 34, &[], 1);
        ext(&mut bytes, 18, &[8 | (1 << 23), 0x10], 2);
        ext(&mut bytes, 34, &[], 3);
        ext(&mut bytes, 54, &[0x11], 4);
        ext(&mut bytes, 35, &[], 5);
        ext(&mut bytes, 34, &[], 6);
        ext(&mut bytes, 33, &[], 7);
        ext(&mut bytes, 18, &[0, 0], 8);
        ext(&mut bytes, 34, &[], 9);
        words(&mut bytes, &[0x10015]);
        let assets = assets(bytes);
        let config = ScriptRuntimeConfig::default();
        let mut vm = ScriptRuntime::boot(12, config.clone());
        vm.vars[5] = 1337;
        let mut input = PalInputState::new();
        input.handle_keyboard_event("Return", true);
        input.handle_keyboard_event("Ctrl", true);
        input.handle_mouse_button_event(crate::event::MouseButton::Left, true);
        input.handle_mouse_wheel(0.0, 1.0);
        vm.run_frame_with_resources(&assets, None, None, None, None, Some(&input), &config)
            .unwrap();
        assert_eq!(vm.vars[1], 4 | 8 | (1 << 23));
        assert_eq!(vm.vars[2], 1);
        assert_eq!(vm.vars[3], 4);
        assert_eq!(vm.vars[4], 1, "async polling bypasses PAL cancellation");
        assert_eq!(vm.vars[5], 1337, "input_clear preserves the destination");
        assert_eq!(&vm.vars[6..10], &[0, 0, 1, 0]);
        vm.apply_input_control(&mut input);
        assert!(!input.click_push());
        input.begin_frame();
        assert_eq!(input.raw_key_on(), 4 | 8 | (1 << 23));
        assert_eq!(input.raw_key_push(), 0);
        assert!(vm.stack.is_empty());
    }

    #[test]
    fn persistent_masks_reach_task_and_button_consumers_next_frame() {
        let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
        let mut input = PalInputState::new();
        vm.stack.extend([0x10, 8 | (1 << 23)]);
        assert_eq!(
            vm.dispatch_input_control(18, Some(&input)),
            ExtCallOutcome::Value(1)
        );
        vm.apply_input_control(&mut input);
        input.begin_frame();
        input.handle_keyboard_event("Ctrl", true);
        input.handle_mouse_button_event(crate::event::MouseButton::Left, true);
        input.handle_mouse_wheel(0.0, 1.0);
        vm.apply_input_control(&mut input);
        assert!(!input.mouse_push(PalMouseButton::Left));
        assert!(!input.mouse_on(PalMouseButton::Left));
        assert!(!input.fast_forward_held());
        assert!(!input.click_push());
        assert_eq!(vm.filter_script_input(&input).raw_key_push(), 0);
        let mut tasks = TaskSystem::new();
        let mut sprites = SpriteSystem::new();
        let wait = tasks.create_wait_click().unwrap();
        tasks.process(&mut sprites, &input, vm.skip_active(&input), false);
        assert!(
            tasks.is_alive(wait),
            "cancelled input must not satisfy a wait task"
        );
    }
}

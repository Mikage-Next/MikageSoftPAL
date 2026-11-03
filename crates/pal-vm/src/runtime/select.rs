//! Game.exe category 6 (table 0x4BB350). The option declarations between
//! select (0x436240) and select_commit (0x435C90) form a blocking branch,
//! not ordinary buttons and not a list that execution may fall through.
use super::*;

#[derive(Clone, Debug, Default)]
pub(super) struct SelectSubsystemState {
    background: i32,
    colors: [u32; 3],
    locked: bool,
    process: [u32; 3], // enter, leave, choose point ids
    options: Vec<SelectOption>,
    collecting: bool,
    active: bool,
    declarations: usize,
    key: u32, // current reaction in low word, previous reaction in high word
    keyboard_index: Option<usize>,
    navigation_index: i32, // ctx+661680, starts at -1; mouse hover never changes it
    mouse_position: Option<(i32, i32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(script: Vec<u8>, points: Vec<u8>) -> CoreAssets {
        let asset = |name: &str, bytes| LoadedAsset {
            name: name.into(),
            bytes,
            source: AssetSource::Loose {
                path: PathBuf::from(name),
            },
        };
        CoreAssets {
            script: asset("Script.src", script),
            file_dat: asset("File.dat", Vec::new()),
            text_dat: asset("Text.dat", b"first choice\0second choice\0".to_vec()),
            mem_dat: asset("Mem.dat", Vec::new()),
            point_dat: asset("Point.dat", points.clone()),
            graphic_dat: None,
            text_dat_multilingual: false,
            file_dat_layout: FileDatLayout::default(),
            script_check_value: 0,
            script_entry_pc: 12,
            extended_softpal: false,
            native_build: Default::default(),
            registered_softpal: false,
            game_token: None,
            point_table: PointTable::parse(&points).unwrap(),
            graphic_index: None,
        }
    }

    fn words(script: &mut Vec<u8>, words: &[u32]) {
        for word in words {
            script.extend_from_slice(&word.to_le_bytes());
        }
    }

    fn ext(script: &mut Vec<u8>, index: u32, args: &[u32], dst: u32) {
        for arg in args.iter().rev() {
            words(script, &[0x1001f, *arg]);
        }
        words(script, &[0x10017, 0x60000 | index, dst]);
    }

    fn call(
        runtime: &mut ScriptRuntime,
        assets: &CoreAssets,
        index: u16,
        args: &[i32],
        sprites: &mut SpriteSystem,
        input: Option<&PalInputState>,
    ) -> ExtCallOutcome {
        runtime.stack.extend(args.iter().rev().copied());
        runtime.dispatch_select_ext(index, assets, Nls::ShiftJis, None, Some(sprites), input)
    }

    fn mouse(x: i32, y: i32, push: bool) -> PalInputState {
        let mut input = PalInputState::new();
        input.set_coordinate_space(1280, 720, 1280, 720);
        input.handle_cursor_moved(x as f64, y as f64);
        if push {
            input.handle_mouse_button_event(crate::event::MouseButton::Left, true);
        }
        input
    }

    #[test]
    fn select_blocks_at_commit_then_jumps_without_falling_through() {
        let mut script = b"Sv20".to_vec();
        words(&mut script, &[0, 12]);
        ext(
            &mut script,
            0,
            &[0xfffffff, 0xffffff, 0xffffff, 0xffffff],
            0,
        );
        ext(&mut script, 1, &[1], 0);
        ext(&mut script, 2, &[0, 1, 10, 10, 0, 0], 0); // hidden
        ext(&mut script, 2, &[0, 1, 40, 40, 1, 0], 0); // disabled
        ext(&mut script, 2, &[13, 1, 50, 80, 3, 0], 0);
        let commit_pc = script.len() as u32;
        ext(&mut script, 3, &[], 0);
        words(&mut script, &[0x10001, 0x40000007, 999, 0x10015]);
        let target_pc = script.len() as u32;
        words(&mut script, &[0x10001, 0x40000007, 123]);
        ext(&mut script, 10, &[], 8);
        words(&mut script, &[0x10015]);
        let assets = assets(script, (target_pc - 12).to_le_bytes().to_vec());
        let config = ScriptRuntimeConfig::default();
        let mut runtime = ScriptRuntime::boot(12, config.clone());
        let mut sprites = SpriteSystem::new();
        let click = mouse(55, 85, true);
        let tick = runtime
            .run_frame_with_resources(
                &assets,
                None,
                Some(&mut sprites),
                None,
                None,
                Some(&click),
                &config,
            )
            .unwrap();
        assert_eq!(tick.wait_request, None);
        assert_eq!(runtime.pc, commit_pc, "arrival click must be ignored");
        assert_eq!(
            runtime.select_state.options.len(),
            2,
            "hidden entries do not consume indices"
        );
        assert_eq!(runtime.vars[7], 0);
        assert_eq!(sprites.len(), 2);
        assert!(sprites
            .iter_sprites()
            .all(|(_, sprite)| sprite.base_priority < 0));
        runtime.resolve_pending_wait();
        let disabled = mouse(45, 45, true);
        runtime
            .run_frame_with_resources(
                &assets,
                None,
                Some(&mut sprites),
                None,
                None,
                Some(&disabled),
                &config,
            )
            .unwrap();
        assert_eq!(
            runtime.pc, commit_pc,
            "disabled option must not choose a branch"
        );
        runtime.resolve_pending_wait();
        runtime
            .run_frame_with_resources(
                &assets,
                None,
                Some(&mut sprites),
                None,
                None,
                Some(&click),
                &config,
            )
            .unwrap();
        assert_eq!(runtime.pc, target_pc);
        assert_eq!(sprites.len(), 0, "both option surfaces must be released");
        assert_eq!(
            runtime.vars[7], 0,
            "branch starts next frame, after click consumption"
        );
        runtime.resolve_pending_wait();
        let tick = runtime
            .run_frame_with_resources(&assets, None, Some(&mut sprites), None, None, None, &config)
            .unwrap();
        assert!(matches!(tick.status, RuntimeStatus::Halted { .. }));
        assert_eq!(runtime.vars[7], 123);
        assert_eq!(runtime.vars[8], 1);
        assert!(runtime.stack.is_empty());
    }

    #[test]
    fn select_callback_order_and_reaction_queries_match_native_words() {
        // point1=100 target, point2=200 enter, point3=300 leave, point4=400 choose
        let points = [388_u32, 288, 188, 88]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let assets = assets(Vec::new(), points);
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(&mut runtime, &assets, 6, &[2, 3, 4], &mut sprites, None);
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        call(
            &mut runtime,
            &assets,
            2,
            &[0, 1, 40, 40, 3, 0],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 3, &[], &mut sprites, None);
        runtime.pc = 1000;
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 42, false)),
        );
        assert_eq!(runtime.pc, 200);
        assert_eq!(runtime.call_stack, [988]);
        assert!(matches!(
            call(&mut runtime, &assets, 8, &[], &mut sprites, None),
            ExtCallOutcome::Value(0)
        ));
        assert!(matches!(
            call(&mut runtime, &assets, 9, &[], &mut sprites, None),
            ExtCallOutcome::Value(-1)
        ));
        assert!(matches!(
            call(&mut runtime, &assets, 10, &[], &mut sprites, None),
            ExtCallOutcome::Value(-1)
        ));
        runtime.call_stack.clear();
        runtime.pc = 1000;
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 42, true)),
        );
        assert_eq!(
            runtime.pc, 300,
            "leave callback runs before choose callback"
        );
        assert_eq!(runtime.call_stack, [100, 400]);
        assert_eq!(runtime.select_state.key, 0x0001_0201);
        assert!(matches!(
            call(&mut runtime, &assets, 9, &[], &mut sprites, None),
            ExtCallOutcome::Value(0)
        ));
        assert!(matches!(
            call(&mut runtime, &assets, 10, &[], &mut sprites, None),
            ExtCallOutcome::Value(0)
        ));
    }

    #[test]
    fn select_lock_is_visual_and_clear_preserves_initialization() {
        let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(
            &mut runtime,
            &assets,
            0,
            &[12, 0x112233, 0x445566, 0x778899],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        call(
            &mut runtime,
            &assets,
            2,
            &[0, 1, 40, 40, 3, 6],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 5, &[0, 4], &mut sprites, None);
        assert_eq!(runtime.select_state.options[0].frame_offset, 12);
        call(&mut runtime, &assets, 7, &[1], &mut sprites, None);
        call(&mut runtime, &assets, 3, &[], &mut sprites, None);
        runtime.pc = 1000;
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 42, true)),
        );
        assert_eq!(runtime.pc, 100, "lock does not prohibit selecting");
        call(&mut runtime, &assets, 4, &[], &mut sprites, None);
        assert_eq!(runtime.select_state.background, 12);
        assert_eq!(
            runtime.select_state.colors,
            [0xff112233, 0xff445566, 0xff778899]
        );
    }

    #[test]
    fn select_keyboard_navigation_requires_confirmation_and_auto_never_selects() {
        let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        call(
            &mut runtime,
            &assets,
            2,
            &[0, 1, 40, 40, 3, 0],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 3, &[], &mut sprites, None);
        call(&mut runtime, &assets, 14, &[1], &mut sprites, None);
        assert!(runtime.text_auto_enabled);
        assert!(!runtime.select_state.locked);
        let mut input = mouse(1000, 600, false);
        input.handle_keyboard_event("ArrowDown", true);
        runtime.pc = 1000;
        call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
        assert_eq!(runtime.pc, 988);
        input.begin_frame();
        input.handle_keyboard_event("Enter", true);
        runtime.pc = 1000;
        call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
        assert_eq!(runtime.pc, 100);
    }

    #[test]
    fn overlapping_selects_highlight_all_but_click_first_native_declaration() {
        let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(
            &mut runtime,
            &assets,
            0,
            &[12, 0x112233, 0x445566, 0x778899],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        for _ in 0..2 {
            call(
                &mut runtime,
                &assets,
                2,
                &[0, 1, 40, 40, 3, 0],
                &mut sprites,
                None,
            );
        }
        call(&mut runtime, &assets, 3, &[], &mut sprites, None);
        runtime.pc = 1000;
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 42, false)),
        );
        assert_eq!(
            runtime.select_state.key & 0xffff,
            2,
            "last hit supplies hover reaction"
        );
        for option in &runtime.select_state.options {
            let label = sprites.get(option.label.unwrap()).unwrap();
            assert_eq!(label.color, PalColor::from_argb(0xff445566));
        }
        runtime.pc = 1000;
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 42, true)),
        );
        assert_eq!(
            runtime.select_state.key & 0xffff,
            0x201,
            "first hit consumes click"
        );
    }

    #[test]
    fn select_keyboard_cursor_is_independent_of_mouse_hover_and_clamps() {
        let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        for y in [40, 100, 160] {
            call(
                &mut runtime,
                &assets,
                2,
                &[0, 1, 40, y, 3, 0],
                &mut sprites,
                None,
            );
        }
        call(&mut runtime, &assets, 3, &[], &mut sprites, None);
        call(
            &mut runtime,
            &assets,
            3,
            &[],
            &mut sprites,
            Some(&mouse(42, 162, false)),
        );
        assert_eq!(runtime.select_state.key & 0xff, 3);
        for expected in [0, 1, 2, 2] {
            let mut input = mouse(42, 162, false);
            input.handle_keyboard_event("ArrowDown", true);
            call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
            assert_eq!(runtime.select_state.navigation_index, expected);
        }
        for expected in [1, 0, 0] {
            let mut input = mouse(42, 162, false);
            input.handle_keyboard_event("ArrowUp", true);
            call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
            assert_eq!(runtime.select_state.navigation_index, expected);
        }
    }

    #[test]
    fn scene_load_releases_choice_sprites_but_preserves_global_select_configuration() {
        let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
        let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
        let mut sprites = SpriteSystem::new();
        call(
            &mut runtime,
            &assets,
            0,
            &[12, 0x112233, 0x445566, 0x778899],
            &mut sprites,
            None,
        );
        call(&mut runtime, &assets, 6, &[2, 3, 4], &mut sprites, None);
        call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
        call(
            &mut runtime,
            &assets,
            2,
            &[0, 1, 40, 40, 3, 0],
            &mut sprites,
            None,
        );
        assert_eq!(sprites.len(), 1);
        runtime.release_scene_for_load(&mut sprites);
        assert_eq!(sprites.len(), 0);
        assert!(!runtime.select_state.active);
        assert!(!runtime.select_state.collecting);
        assert!(runtime.select_state.options.is_empty());
        assert_eq!(runtime.select_state.background, 12);
        assert_eq!(
            runtime.select_state.colors,
            [0xff112233, 0xff445566, 0xff778899]
        );
        assert_eq!(runtime.select_state.process, [2, 3, 4]);
    }

    #[test]
    fn btn_mode_select_accepts_mouse_return_and_space_release_without_capture() {
        for key in [None, Some("Enter"), Some("Space")] {
            let assets = assets(Vec::new(), 88_u32.to_le_bytes().to_vec());
            let mut runtime = ScriptRuntime::boot(1000, ScriptRuntimeConfig::default());
            let mut sprites = SpriteSystem::new();
            // Exercise real category 8 dispatch and its one-argument pop.
            runtime.stack.extend([77, -1]);
            assert!(matches!(runtime.dispatch_button_ext(36, &assets, Nls::ShiftJis,
                None, None, None, None), ExtCallOutcome::Value(1)));
            assert_eq!(runtime.stack, [77]);
            assert!(runtime.button_release_mode);
            call(&mut runtime, &assets, 1, &[0], &mut sprites, None);
            call(&mut runtime, &assets, 2, &[0, 1, 40, 40, 3, 0], &mut sprites, None);
            call(&mut runtime, &assets, 3, &[], &mut sprites, None);
            let mut input = mouse(42, 42, false);
            if let Some(key) = key {
                input.handle_keyboard_event(key, true);
            } else {
                input.handle_mouse_button_event(crate::event::MouseButton::Left, true);
            }
            runtime.pc = 1000;
            call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
            assert_eq!(runtime.pc, 988, "push must keep polling in release mode");
            input.begin_frame();
            if let Some(key) = key {
                input.handle_keyboard_event(key, false);
            } else {
                input.handle_mouse_button_event(crate::event::MouseButton::Left, false);
            }
            runtime.pc = 1000;
            call(&mut runtime, &assets, 3, &[], &mut sprites, Some(&input));
            assert_eq!(runtime.pc, 100, "release must choose the branch");
            runtime.stack.push(0);
            runtime.dispatch_button_ext(36, &assets, Nls::ShiftJis, None, None, None, None);
            assert!(!runtime.button_release_mode);
        }
    }

    #[test]
    #[ignore = "requires the local read-only Koikake fixture"]
    fn select_koikake_first_choice_is_visible_and_branches() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testcase");
        let mut manager = ResourceManager::bootstrap(root, Nls::ShiftJis).unwrap();
        let assets = CoreAssets::load(&mut manager, None).unwrap();
        let config = ScriptRuntimeConfig::default();
        let mut runtime = ScriptRuntime::boot(0x7c1ec, config.clone());
        let mut sprites = SpriteSystem::new();
        runtime.stack.extend([0xffffff, 0xffffff, 0xffffff, 1452]);
        runtime.dispatch_select_ext(
            0,
            &assets,
            Nls::ShiftJis,
            Some(&mut manager),
            Some(&mut sprites),
            None,
        );
        let tick = runtime
            .run_frame_with_resources(
                &assets,
                Some(&mut manager),
                Some(&mut sprites),
                None,
                None,
                None,
                &config,
            )
            .unwrap();
        assert_eq!(tick.wait_request, None);
        assert_eq!(runtime.pc, 0x7c278);
        assert_eq!(runtime.select_state.options.len(), 2);
        assert_eq!(sprites.len(), 4, "real labels and real background art");
        for option in &runtime.select_state.options {
            let bg = sprites.get(option.background.unwrap()).unwrap();
            assert!(bg.cell_size.width > 100);
            assert!(bg.cell_size.height < 150);
            eprintln!(
                "choice {:?}: cell={}x{} source={}",
                option.rect, bg.cell_size.width, bg.cell_size.height, bg.source_name
            );
        }
        let rect = runtime.select_state.options[1].rect;
        let input = mouse((rect[0] + rect[2]) / 2, (rect[1] + rect[3]) / 2, true);
        runtime.resolve_pending_wait();
        runtime
            .run_frame_with_resources(
                &assets,
                Some(&mut manager),
                Some(&mut sprites),
                None,
                None,
                Some(&input),
                &config,
            )
            .unwrap();
        assert_eq!(
            runtime.pc,
            assets.point_table.resolve_target_pc(3641).unwrap().unwrap()
        );
        assert_eq!(sprites.len(), 0);
    }
}

#[derive(Clone, Debug)]
struct SelectOption {
    target: u32,
    flags: i32,
    frame_offset: i32,
    background: Option<SpriteHandle>,
    label: Option<SpriteHandle>,
    rect: [i32; 4],
}

impl ScriptRuntime {
    pub(super) fn dispatch_select_ext(
        &mut self,
        index: u16,
        assets: &CoreAssets,
        nls: Nls,
        resource_manager: Option<&mut ResourceManager>,
        mut sprites: Option<&mut SpriteSystem>,
        input: Option<&PalInputState>,
    ) -> ExtCallOutcome {
        match index {
            0 => {
                let args = self.pop_ext_args(4);
                if args.len() != 4 {
                    return ExtCallOutcome::Block;
                }
                self.select_state.background = args[0];
                self.select_state.colors = [
                    args[1] as u32 | 0xff000000,
                    args[2] as u32 | 0xff000000,
                    args[3] as u32 | 0xff000000,
                ];
                ExtCallOutcome::Value(1)
            }
            1 => {
                let args = self.pop_ext_args(1);
                if args.len() != 1 {
                    return ExtCallOutcome::Block;
                }
                self.release_select_sprites(sprites.as_deref_mut());
                self.select_state.options.clear();
                self.select_state.collecting = true;
                self.select_state.active = false;
                self.select_state.declarations = 0;
                self.select_state.key = 0;
                self.select_state.keyboard_index = None;
                self.select_state.navigation_index = -1;
                self.select_state.mouse_position = None;
                // 0x436240 calls 0x439C10 with this argument: ADV visibility.
                self.text_state.visible = args[0] != 0;
                self.text_state.show_wait_mark = false;
                self.text_state.reveal_enabled = false;
                self.text_state.dirty = true;
                ExtCallOutcome::Value(1)
            }
            2 => {
                let args = self.pop_ext_args(6);
                if args.len() != 6 || !self.select_state.collecting {
                    return ExtCallOutcome::Block;
                }
                self.select_state.declarations += 1;
                if self.select_state.declarations > 16 {
                    return ExtCallOutcome::Block;
                }
                // Native pop order: text, point, x, y, visibility flags, row.
                // Hidden declarations do not occupy a selection index.
                if args[4] & 1 != 0 {
                    let option = self.create_select_option(
                        &args,
                        assets,
                        nls,
                        resource_manager,
                        sprites.as_deref_mut(),
                    );
                    self.select_state.options.push(option);
                }
                ExtCallOutcome::Value(1)
            }
            3 => {
                if self.select_state.collecting {
                    self.select_state.collecting = false;
                    self.select_state.active = true;
                    // Ignore the click which arrived at this new choice block.
                    self.pc = self.pc.saturating_sub(self.extcall_instruction_size);
                    return ExtCallOutcome::Yield { value: None };
                }
                if !self.select_state.active {
                    return ExtCallOutcome::Value(1);
                }
                self.poll_select(assets, sprites.as_deref_mut(), input)
            }
            4 => {
                // 0x435C30 releases the two sprite arrays. Keep init colors,
                // filename, offsets and callback registration across clears.
                self.release_select_sprites(sprites);
                self.select_state.active = false;
                self.select_state.collecting = false;
                ExtCallOutcome::Value(1)
            }
            5 => {
                let args = self.pop_ext_args(2);
                if args.len() != 2 {
                    return ExtCallOutcome::Block;
                }
                if let Some(option) = usize::try_from(args[0])
                    .ok()
                    .and_then(|i| self.select_state.options.get_mut(i))
                {
                    option.frame_offset = args[1].saturating_mul(3);
                }
                ExtCallOutcome::Value(1)
            }
            6 => {
                let args = self.pop_ext_args(3);
                if args.len() != 3 {
                    return ExtCallOutcome::Block;
                }
                self.select_state.process = [args[0] as u32, args[1] as u32, args[2] as u32];
                ExtCallOutcome::Value(1)
            }
            7 => {
                let args = self.pop_ext_args(1);
                if args.len() != 1 {
                    return ExtCallOutcome::Block;
                }
                // Despite its name this only forces the background row base
                // to zero in 0x435C90; native still hit-tests and selects.
                self.select_state.locked = args[0] != 0;
                ExtCallOutcome::Value(1)
            }
            8 => ExtCallOutcome::Value((self.select_state.key & 0xff) as i32 - 1),
            9 => ExtCallOutcome::Value(((self.select_state.key >> 16) & 0xff) as i32 - 1),
            10 => ExtCallOutcome::Value(if self.select_state.key & 0x200 != 0 {
                (self.select_state.key & 0xff) as i32 - 1
            } else {
                -1
            }),
            // These are shared skip/auto handlers, not select locks.
            12..=16 => self.dispatch_font_system_stub(index - 12, input),
            17 => ExtCallOutcome::Value(self.system_state.auto_speed_percent()),
            _ => ExtCallOutcome::Skip,
        }
    }

    pub(super) fn release_select_sprites(&mut self, sprites: Option<&mut SpriteSystem>) {
        if let Some(sprites) = sprites {
            for option in &mut self.select_state.options {
                for handle in [option.background.take(), option.label.take()]
                    .into_iter()
                    .flatten()
                {
                    sprites.release(handle);
                }
            }
        }
    }

    /// Abandon a scene without discarding the script's global init/callbacks.
    pub(super) fn clear_select_scene(&mut self, sprites: Option<&mut SpriteSystem>) {
        self.release_select_sprites(sprites);
        self.select_state.options.clear();
        self.select_state.collecting = false;
        self.select_state.active = false;
        self.select_state.declarations = 0;
        self.select_state.key = 0;
        self.select_state.keyboard_index = None;
        self.select_state.navigation_index = -1;
        self.select_state.mouse_position = None;
    }

    fn create_select_option(
        &mut self,
        args: &[i32],
        assets: &CoreAssets,
        nls: Nls,
        resource_manager: Option<&mut ResourceManager>,
        sprites: Option<&mut SpriteSystem>,
    ) -> SelectOption {
        let (logical_width, logical_height) = self.logical_size();
        let x = scale_script_x(args[2], logical_width);
        let y = scale_script_y(args[3], logical_height);
        let label = self
            .resolve_script_string(args[0], assets, nls)
            .unwrap_or_default();
        let (label, _) = parse_pal_text_directives(&label);
        let (tw, th, pixels) = self.font_state.rasterize(&label);
        let mut option = SelectOption {
            target: args[1] as u32,
            flags: args[4],
            frame_offset: args[5],
            background: None,
            label: None,
            rect: [
                x,
                y,
                x.saturating_add(tw as i32),
                y.saturating_add(th as i32),
            ],
        };
        let Some(sprites) = sprites else {
            return option;
        };
        let name = self.resolve_resource_string(self.select_state.background, assets, nls);
        let priority = 0i32.saturating_sub(self.sprite_priority_cursor.saturating_add(4871));
        let decoded = name
            .as_deref()
            .zip(resource_manager)
            .and_then(|(name, manager)| {
                match open_resource_variant(manager, name, IMAGE_EXTENSIONS)
                    .map_err(anyhow::Error::from)
                    .and_then(|asset| decode_asset_image(manager, &asset))
                {
                    Ok(image) => Some(image),
                    Err(err) => {
                        log::warn!("select background {name:?}: {err}");
                        None
                    }
                }
            });
        let (width, height) = if let Some(image) = decoded {
            let width = image.cell_width;
            let height = image.cell_height;
            let surface = sprites.allocate_surface_id();
            if let Ok(surface_data) =
                SpriteSurface::rgba8(surface, 1, image.width, image.height, image.rgba)
            {
                sprites.insert_surface(surface_data);
                let mut desc =
                    SpriteDesc::new(SceneTextureId(surface.0), image.width, image.height);
                desc.cell_width = width;
                desc.cell_height = height;
                desc.position = PalVec3::new(x, y, 0);
                desc.base_priority = priority;
                desc.visible = true;
                desc.source_name =
                    format!("select-background:{}", name.as_deref().unwrap_or_default());
                let handle = sprites.create(desc);
                sprites.rect_set_pos(handle, 0, args[5]);
                let color = self.select_state.colors[0];
                sprites.set_color(
                    handle,
                    PalColor::from_argb(if args[4] & 2 != 0 {
                        color
                    } else {
                        color & 0xffffff | 0x7f000000
                    }),
                );
                option.background = Some(handle);
            }
            (width, height)
        } else {
            (tw, th)
        };
        option.rect = [
            x,
            y,
            x.saturating_add(width as i32),
            y.saturating_add(height as i32),
        ];
        option.label = sprites.create_rgba_sprite(
            tw,
            th,
            pixels,
            PalVec3::new(
                x + (width as i32 - tw as i32) / 2,
                y + (height as i32 - th as i32) / 2 + scale_script_y(4, logical_height),
                0,
            ),
            priority.saturating_add(1),
            format!("select-text:{label}"),
        );
        if args[4] & 2 == 0 {
            if let Some(handle) = option.label {
                sprites.set_color(handle, PalColor::from_argb(0x7fffffff));
            }
        }
        option
    }

    fn poll_select(
        &mut self,
        assets: &CoreAssets,
        mut sprites: Option<&mut SpriteSystem>,
        input: Option<&PalInputState>,
    ) -> ExtCallOutcome {
        // Native rewinds the commit instruction each polling frame. Callbacks
        // return here, while an accepted choice replaces this PC with its point.
        self.pc = self.pc.saturating_sub(self.extcall_instruction_size);
        let previous = self.select_state.key & 0xffff;
        let mut current = 0;
        let mut selected = None;
        if let Some(input) = input {
            let mouse = input.mouse_position();
            if self.select_state.mouse_position != Some(mouse) {
                self.select_state.keyboard_index = None;
            }
            self.select_state.mouse_position = Some(mouse);
            let count = self.select_state.options.len();
            let navigation = input.key_push(PalKey::Up) || input.key_push(PalKey::Down);
            if navigation && count > 0 {
                let old = self.select_state.navigation_index;
                let next = if input.key_push(PalKey::Up) {
                    old.saturating_sub(1).max(0)
                } else {
                    old.saturating_add(1).min(count as i32 - 1)
                };
                self.select_state.navigation_index = next;
                self.select_state.keyboard_index = Some(next as usize);
                // PalCursorSetPos then returns; no selection on this pass.
                return ExtCallOutcome::Yield { value: None };
            }
            // Native walks declarations in ascending order. Every hit gets
            // hover styling; the last hit wins the hover reaction, but a click
            // exits immediately at the FIRST hit (0x435C90).
            let hits: Vec<usize> = self
                .select_state
                .options
                .iter()
                .enumerate()
                .filter_map(|(i, option)| {
                    let r = option.rect;
                    (option.flags & 2 != 0
                        && if let Some(focus) = self.select_state.keyboard_index {
                            // Cursor warp puts the pointer at the focused cell's
                            // centre, so overlapping cells must still all hit.
                            let focused = self.select_state.options[focus].rect;
                            let x = (focused[0] + focused[2]) / 2;
                            let y = (focused[1] + focused[3]) / 2;
                            x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
                        } else {
                            mouse.0 >= r[0] && mouse.0 < r[2] && mouse.1 >= r[1] && mouse.1 < r[3]
                        })
                    .then_some(i)
                })
                .collect();
            if let Some(&i) = hits.last() {
                current = i as u32 + 1;
                let accept = if self.button_release_mode {
                    input.mouse_pull(PalMouseButton::Left)
                        || input.key_pull(PalKey::Return)
                        || input.key_pull(PalKey::Space)
                } else {
                    input.mouse_push(PalMouseButton::Left)
                        || input.key_push(PalKey::Return)
                        || input.key_push(PalKey::Space)
                };
                if accept {
                    selected = hits.first().copied();
                    current = selected.unwrap() as u32 + 1 | 0x200;
                }
            }
            if let Some(sprites) = sprites.as_deref_mut() {
                for (i, option) in self.select_state.options.iter().enumerate() {
                    if option.flags & 2 == 0 {
                        continue;
                    }
                    let hover = hits.contains(&i);
                    let color = PalColor::from_argb(self.select_state.colors[usize::from(hover)]);
                    let row = if self.select_state.locked {
                        0
                    } else {
                        option.frame_offset
                    };
                    if let Some(handle) = option.background {
                        sprites.set_color(handle, color);
                        sprites.rect_set_pos(
                            handle,
                            0,
                            row.wrapping_add(i32::from(hover)),
                        );
                    }
                    if let Some(handle) = option.label {
                        sprites.set_color(handle, color);
                    }
                }
            }
        }
        if let Some(i) = selected {
            let target = self.select_state.options[i].target;
            // Point zero is native's code-base offset zero, not fall-through.
            self.pc = if target == 0 {
                pal_script::script::SCRIPT_CODE_BASE
            } else {
                match assets.point_table.resolve_target_pc(target) {
                    Ok(Some(pc)) => pc,
                    _ => return ExtCallOutcome::Block,
                }
            };
            self.release_select_sprites(sprites.as_deref_mut());
            self.select_state.active = false;
            self.text_state.show_wait_mark = false;
            if self.dispatch_skip_state() == 0 {
                self.system_button_blocked_at = Some(self.pal_time_ms);
                self.button_input_block_duration = 100;
            }
            self.capture_history_checkpoint(sprites.as_deref());
            log::debug!(
                "[trace-select] selected index={i} point={target} pc=0x{:08X}",
                self.pc
            );
        }
        // Native pushes enter/choose first, then leave: leave executes first
        // and returns through enter/choose to the parked commit or target.
        if current != previous {
            let action = if selected.is_some() {
                self.select_state.process[2]
            } else if current != 0 {
                self.select_state.process[0]
            } else {
                0
            };
            for point in [
                action,
                if previous != 0 {
                    self.select_state.process[1]
                } else {
                    0
                },
            ] {
                if point != 0 {
                    if let Ok(Some(pc)) = assets.point_table.resolve_target_pc(point) {
                        self.call_stack.push(self.pc);
                        self.pc = pc;
                    }
                }
            }
        }
        self.select_state.key = current | (previous << 16);
        // Native 0x435C90/0x464120 sets the VM's end-of-slice flag, not a
        // PAL wait task. Poll on the very next frame so single-frame input
        // edges are not lost during wait-task initialization/completion.
        // Ending this slice also keeps the accepted click off the next line.
        ExtCallOutcome::Yield { value: None }
    }
}

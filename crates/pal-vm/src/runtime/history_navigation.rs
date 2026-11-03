//! Newer history checkpoint files and replay-until-PC navigation.
//! Native contracts: 0x461200/0x4612D0/0x461320/0x461580, 0x465070,
//! and the 0x47C240 pre-instruction replay completion gate.
use super::*;

#[derive(Clone, Debug, Default)]
pub(super) struct HistoryNavigation {
    next_id: u32,
    counter_initialized: bool,
    snapshots: BTreeMap<i32, RuntimeSaveSnapshot>,
    pending: BTreeSet<i32>,
    pub target: Option<u32>,
}

impl ScriptRuntime {
    pub(super) fn capture_history_checkpoint(&mut self, sprites: Option<&SpriteSystem>) {
        if !self.input_registered_profile || !self.history_state.icons.config.secondary_enabled {
            return;
        }
        self.history_navigation.next_id = self.history_navigation.next_id.wrapping_add(1);
        let id = self.history_navigation.next_id as i32;
        self.history_state.icons.current_checkpoint = id;
        let snapshot = self.capture_resumable_scene(sprites);
        self.history_navigation.snapshots.insert(id, snapshot);
        self.history_navigation.pending.insert(id);
    }

    pub(crate) fn flush_history_checkpoints(&mut self, resources: Option<&ResourceManager>) {
        let Some(resources) = resources else {
            return;
        };
        let directory = history_directory(resources.root());
        if !self.history_navigation.counter_initialized
            && self.history_navigation.pending.is_empty()
        {
            if let Ok(entries) = std::fs::read_dir(&directory) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if let Some(id) = name
                        .strip_prefix("hist")
                        .and_then(|s| s.strip_suffix(".dat"))
                        .filter(|s| s.len() == 8)
                        .and_then(|s| u32::from_str_radix(s, 16).ok())
                    {
                        self.history_navigation.next_id = self.history_navigation.next_id.max(id);
                    }
                }
            }
            self.history_navigation.counter_initialized = true;
        }
        if !self.history_navigation.pending.is_empty() {
            let result = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(&directory)?;
                let ids: Vec<_> = self.history_navigation.pending.iter().copied().collect();
                for id in ids {
                    let Some(snapshot) = self.history_navigation.snapshots.get(&id) else {
                        continue;
                    };
                    let bytes = encode_runtime_save_snapshot(snapshot)?;
                    std::fs::write(directory.join(format!("hist{:08x}.dat", id as u32)), bytes)?;
                    self.history_navigation.pending.remove(&id);
                }
                Ok(())
            })();
            if let Err(error) = result {
                log::warn!("history checkpoint write failed: {error}");
            }
        }
        // Disk retains checkpoints referenced by saves. Keep only the current
        // 128-record window in RAM, plus writes that have not reached disk.
        let live: BTreeSet<_> = self
            .history_state
            .icons
            .records
            .iter()
            .map(|r| r.checkpoint)
            .chain([self.history_state.icons.current_checkpoint])
            .collect();
        self.history_navigation
            .snapshots
            .retain(|id, _| live.contains(id) || self.history_navigation.pending.contains(id));
    }

    pub(super) fn dispatch_history_navigation(
        &mut self,
        index: u16,
        resources: Option<&ResourceManager>,
        sprites: Option<&mut SpriteSystem>,
    ) -> ExtCallOutcome {
        if index == 37 {
            self.capture_history_checkpoint(sprites.as_deref());
            self.flush_history_checkpoints(resources);
            return ExtCallOutcome::Continue;
        }
        let args = self.pop_ext_args(1);
        let Some(&id) = args.first() else {
            return ExtCallOutcome::Block;
        };
        let path = resources
            .map(|r| history_directory(r.root()).join(format!("hist{:08x}.dat", id as u32)));
        if index == 39 {
            return ExtCallOutcome::Value(i32::from(
                self.history_navigation.snapshots.contains_key(&id)
                    || path.as_ref().is_some_and(|p| p.is_file()),
            ));
        }
        let snapshot = self
            .history_navigation
            .snapshots
            .get(&id)
            .cloned()
            .or_else(|| path.and_then(|p| read_snapshot_file(&p).ok()));
        let Some(snapshot) = snapshot else {
            return ExtCallOutcome::Value(0);
        };
        let replay_target = self.history_navigation.target;
        if let Some(sprites) = sprites {
            self.restore_checkpoint_scene(&snapshot, sprites);
        }
        self.restore_save_snapshot(snapshot);
        self.history_navigation.target = replay_target;
        self.history_state.active = false;
        self.system_button_blocked_at = Some(self.pal_time_ms);
        self.button_input_block_duration = 100;
        // The original load keeps the caller's replay target across restore,
        // writes -1 into the restored destination, and yields the VM slice.
        ExtCallOutcome::Yield { value: Some(-1) }
    }

    pub(super) fn finish_history_replay(
        &mut self,
        assets: &CoreAssets,
        nls: Nls,
        sprites: Option<&SpriteSystem>,
    ) -> bool {
        let Some(target) = self.history_navigation.target else {
            return false;
        };
        if self.pc.wrapping_sub(target) >= 12 {
            return false;
        }
        self.history_navigation.target = None;
        self.last_skip_state = 0;
        self.text_skip_enabled = false;
        self.scene_skip_active = false;
        self.capture_save_checkpoint(assets, nls, sprites);
        self.system_button_blocked_at = Some(self.pal_time_ms);
        self.button_input_block_duration = 100;
        let point = self.history_state.icons.config.secondary_update;
        if let Some(pc) = u32::try_from(point)
            .ok()
            .filter(|&point| point != 0)
            .and_then(|point| assets.point_table.resolve_target_pc(point).ok().flatten())
        {
            self.call_stack.push(self.pc);
            self.pc = pc;
        }
        true
    }

    pub(super) fn restore_history_counter(&mut self, id: i32) {
        if id != -1 {
            self.history_navigation.next_id = self.history_navigation.next_id.max(id as u32);
        }
    }
}

fn history_directory(root: &Path) -> PathBuf {
    crate::save_format::save_directory(root).join("sena_rs").join("history")
}

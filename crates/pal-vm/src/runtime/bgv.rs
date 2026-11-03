//! Independent native BGV lanes (Game.exe 0x4435F0/0x4434E0).
use super::*;

// Internal audio-map namespace, not an external opcode category. ADV voices
// use category 13; native BGV wrappers occupy a separate eight-lane array.
pub(super) const BGV_NAMESPACE: u16 = 0x800D;
pub(super) const BGV_EXTENSIONS: &[&str] = &["", ".OGG", ".ogg", ".WAV", ".wav", ".WMA", ".wma"];

#[derive(Clone, Debug, Default)]
struct BgvLane {
    name: String,
    looping: bool,
    paused: bool,
    stop_deadline: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioConfig;

    fn assets() -> (CoreAssets, PointTable) {
        let empty = |name: &str| LoadedAsset {
            name: name.into(),
            bytes: Vec::new(),
            source: AssetSource::Loose {
                path: PathBuf::from(name),
            },
        };
        let points = PointTable::parse(&[]).unwrap();
        (
            CoreAssets {
                script: empty("Script.src"),
                file_dat: empty("File.dat"),
                text_dat: empty("Text.dat"),
                mem_dat: empty("Mem.dat"),
                point_dat: empty("Point.dat"),
                graphic_dat: None,
                text_dat_multilingual: false,
                file_dat_layout: FileDatLayout::default(),
                script_check_value: 0,
                script_entry_pc: 12,
                extended_softpal: false,
            native_build: Default::default(),
            registered_softpal: false,
            game_token: None,
                point_table: points.clone(),
                graphic_index: None,
            },
            points,
        )
    }

    struct Fixture {
        root: PathBuf,
        resources: ResourceManager,
        audio: AudioSystem,
        vm: ScriptRuntime,
        assets: CoreAssets,
        points: PointTable,
        name: i32,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT_FIXTURE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "sena_bgv_{}_{}_{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let mut wav = Vec::new();
            wav.extend(b"RIFF");
            wav.extend(40u32.to_le_bytes());
            wav.extend(b"WAVEfmt ");
            wav.extend(16u32.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(8000u32.to_le_bytes());
            wav.extend(16000u32.to_le_bytes());
            wav.extend(2u16.to_le_bytes());
            wav.extend(16u16.to_le_bytes());
            wav.extend(b"data");
            wav.extend(4u32.to_le_bytes());
            wav.extend([0u8; 4]);
            std::fs::write(root.join("room.wav"), wav).unwrap();
            // A colliding ADV playlist must not steal the ordinary BGV clip.
            let mut mix = [0u8; 64];
            for (index, unit) in "room.wav".encode_utf16().enumerate() {
                mix[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
            std::fs::write(root.join("room.mix"), mix).unwrap();
            let resources = ResourceManager::new(&root, Nls::ShiftJis);
            let audio = AudioSystem::new(AudioConfig { enabled: false }).unwrap();
            let mut vm = ScriptRuntime::boot(12, ScriptRuntimeConfig::default());
            let name = vm.store_dynamic_string("room".into());
            let (assets, points) = assets();
            Self {
                root,
                resources,
                audio,
                vm,
                assets,
                points,
                name,
            }
        }
        fn call(&mut self, category: u16, index: u16, args: &[i32]) -> ExtCallOutcome {
            self.vm.stack.push(0x123456);
            self.vm.stack.extend(args.iter().rev());
            let value = self.vm.dispatch_extcall(
                category,
                index,
                &[],
                &self.assets,
                &self.points,
                Some(&mut self.resources),
                None,
                None,
                Some(&mut self.audio),
                None,
            );
            assert_eq!(self.vm.stack.pop(), Some(0x123456));
            assert!(self.vm.stack.is_empty());
            value
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn bgv_lanes_are_independent_of_adv_and_have_native_eight_slot_allocation() {
        let mut f = Fixture::new();
        f.call(13, 0, &[0, f.name, 0, 0]);
        let voice = f.vm.game_audio[&(13, 0)];
        for lane in 0..8 {
            let result = f.call(13, 8, &[-1, f.name, 1, 0]);
            assert!(
                f.vm.game_audio.contains_key(&(BGV_NAMESPACE, lane)),
                "lane {lane}, result {result:?}, map {:?}, resource {:?}",
                f.vm.game_audio,
                f.vm.resolve_resource_string(f.name, &f.assets, Nls::ShiftJis)
            );
            assert_eq!(result, ExtCallOutcome::Value(lane));
        }
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP2), 8);
        assert_eq!(f.vm.game_audio[&(13, 0)], voice);
        assert_eq!(f.call(13, 8, &[-1, f.name, 0, 0]), ExtCallOutcome::Value(0));
        assert_eq!(f.call(13, 8, &[8, f.name, 0, 0]), ExtCallOutcome::Value(0));
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP2), 8);
        f.call(13, 1, &[-1, 0]);
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP1), 0);
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP2), 8);
        f.call(13, 0, &[0, f.name, 0, 0]);
        f.call(13, 9, &[-1, 0]);
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP2), 0);
        assert_eq!(f.audio.loaded_channel_count(PalSoundGroup::GROUP1), 1);
    }

    #[test]
    fn bgv_aliases_use_independent_gain_and_mute_without_character_volume() {
        let mut f = Fixture::new();
        f.vm.voice_unit_headers = Some(vec!["room".into()]);
        f.vm.voice_ex_enabled.insert(0, false);
        f.call(13, 21, &[37]);
        assert_eq!(f.call(2, 56, &[3, f.name, 0, 0]), ExtCallOutcome::Value(3));
        let bgv = f.vm.game_audio[&(BGV_NAMESPACE, 3)];
        assert_eq!(bgv.group, PalSoundGroup::GROUP2);
        assert_eq!(f.audio.channel_volume(bgv).unwrap(), PalVolume::MAX);
        assert_eq!(f.audio.effective_volume(bgv).unwrap().raw(), 3700);
        f.call(2, 68, &[1]);
        assert_eq!(f.audio.effective_volume(bgv).unwrap().raw(), 0);
        f.call(2, 69, &[61]);
        f.call(2, 68, &[0]);
        assert_eq!(f.audio.effective_volume(bgv).unwrap().raw(), 6100);
        f.call(2, 67, &[3, 1, 20]);
        assert!(f.vm.bgv_state.lanes[3].paused);
        f.call(13, 19, &[3, 0, 20]);
        assert!(!f.vm.bgv_state.lanes[3].paused);
        f.call(2, 57, &[3, 0]);
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 3)));
    }

    #[test]
    fn bgv_snapshot_roundtrip_restores_paused_loops_on_next_frame_only() {
        let mut f = Fixture::new();
        f.call(13, 0, &[0, f.name, 0, 0]);
        let adv = f.vm.game_audio[&(13, 0)];
        f.call(13, 8, &[2, f.name, 1, 0]);
        f.call(13, 19, &[2, 1, 0]);
        let snapshot = f.vm.capture_save_snapshot();
        assert_eq!(snapshot.bgv_tracks.len(), 1);
        assert_eq!(snapshot.bgv_tracks[0].name, "room");
        assert!(snapshot.bgv_tracks[0].looping);
        assert!(snapshot.bgv_tracks[0].paused);
        let encoded = encode_runtime_save_snapshot(&snapshot).unwrap();
        let decoded = decode_runtime_save_snapshot(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.bgv_tracks, snapshot.bgv_tracks);
        f.vm.replace_dynamic_string(f.name, "changed after saving".into());
        f.call(13, 9, &[-1, 0]);
        f.vm.replace_dynamic_string(f.name, "room".into());
        f.call(13, 8, &[5, f.name, 0, 0]);
        let outgoing = f.vm.game_audio[&(BGV_NAMESPACE, 5)];
        f.call(13, 21, &[63]); // Current preference survives scene restoration.
        f.vm.restore_save_snapshot(decoded);
        assert_eq!(f.vm.game_audio[&(BGV_NAMESPACE, 5)], outgoing);
        assert_eq!(f.vm.capture_save_snapshot().bgv_tracks, snapshot.bgv_tracks);
        f.vm.status = RuntimeStatus::WaitClick { pc: f.vm.pc };
        f.vm.run_frame_with_resources(
            &f.assets,
            Some(&mut f.resources),
            None,
            None,
            Some(&mut f.audio),
            None,
            &ScriptRuntimeConfig::default(),
        )
        .unwrap();
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 5)));
        let restored = f.vm.game_audio[&(BGV_NAMESPACE, 2)];
        assert_eq!(f.audio.effective_volume(restored).unwrap().raw(), 6300);
        assert!(f.vm.bgv_state.lanes[2].looping);
        assert!(f.vm.bgv_state.lanes[2].paused);
        assert_eq!(
            f.vm.game_audio[&(13, 0)],
            adv,
            "BGV restore must not stop ADV voice"
        );
        f.vm.replay_restored_bgv(Some(&mut f.resources), Some(&mut f.audio));
        assert_eq!(
            f.vm.game_audio[&(BGV_NAMESPACE, 2)],
            restored,
            "replay runs once"
        );
    }

    #[test]
    fn bgv_legacy_snapshots_stop_outgoing_lanes_without_inventing_tracks() {
        let mut f = Fixture::new();
        f.call(13, 0, &[0, f.name, 0, 0]);
        let adv = f.vm.game_audio[&(13, 0)];
        for version in 1..=9 {
            f.call(13, 8, &[1, f.name, 1, 0]);
            let snapshot = RuntimeSaveSnapshot {
                version,
                bgv_tracks: f.vm.saved_bgv_tracks(),
                ..RuntimeSaveSnapshot::default()
            };
            f.vm.restore_save_snapshot(snapshot);
            assert!(f.vm.saved_bgv_tracks().is_empty());
            f.vm.replay_restored_bgv(Some(&mut f.resources), Some(&mut f.audio));
            assert!(!f
                .vm
                .game_audio
                .keys()
                .any(|(category, _)| *category == BGV_NAMESPACE));
            assert_eq!(f.vm.game_audio[&(13, 0)], adv);
        }
        let bytes = encode_runtime_save_snapshot(&RuntimeSaveSnapshot::default()).unwrap();
        let mut legacy = bytes;
        legacy[8..12].copy_from_slice(&9u32.to_le_bytes());
        let decoded = decode_runtime_save_snapshot(&mut legacy.as_slice()).unwrap();
        assert!(decoded.bgv_tracks.is_empty());
    }

    #[test]
    fn bgv_restore_preserves_remaining_stop_time_and_waits_for_resources() {
        let mut f = Fixture::new();
        f.call(13, 8, &[1, f.name, 1, 0]);
        f.call(13, 9, &[1, 100]);
        f.vm.set_pal_time(25);
        let snapshot = f.vm.capture_save_snapshot();
        assert_eq!(snapshot.bgv_tracks[0].stop_remaining_ms, Some(75));
        f.vm.restore_save_snapshot(snapshot);
        f.vm.replay_restored_bgv(None, Some(&mut f.audio));
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 1)));
        assert!(f.vm.bgv_state.restored.is_some());
        f.vm.set_pal_time(500);
        f.vm.replay_restored_bgv(Some(&mut f.resources), Some(&mut f.audio));
        assert_eq!(f.vm.bgv_state.lanes[1].stop_deadline, Some(575));
        f.vm.set_pal_time(574);
        f.vm.update_bgv(Some(&mut f.audio));
        assert!(f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 1)));
        f.vm.set_pal_time(575);
        f.vm.update_bgv(Some(&mut f.audio));
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 1)));
    }

    #[test]
    fn bgv_snapshot_rejects_duplicate_and_out_of_range_lanes() {
        let track = SavedBgvTrack {
            slot: 0,
            name: "room".into(),
            looping: true,
            paused: false,
            stop_remaining_ms: None,
        };
        let mut bytes = Vec::new();
        write_saved_bgv(&mut bytes, &[track.clone()]).unwrap();
        let record = bytes[4..].to_vec();
        bytes[0..4].copy_from_slice(&2u32.to_le_bytes());
        bytes.extend(record);
        assert!(read_saved_bgv(&mut bytes.as_slice()).is_err());
        let mut bad = track.clone();
        bad.slot = 8;
        assert!(write_saved_bgv(&mut Vec::new(), &[bad]).is_err());
        assert!(write_saved_bgv(&mut Vec::new(), &[track.clone(), track]).is_err());
        assert!(read_saved_bgv(&mut 9u32.to_le_bytes().as_slice()).is_err());
    }

    #[test]
    fn bgv_stop_fade_keeps_lane_until_deadline_and_respects_skip_and_mute() {
        let mut f = Fixture::new();
        f.vm.set_pal_time(u32::MAX - 50);
        f.call(13, 8, &[2, f.name, 5, 80]);
        f.call(13, 9, &[2, 100]);
        f.vm.set_pal_time(48);
        f.vm.update_bgv(Some(&mut f.audio));
        assert!(f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 2)));
        f.vm.set_pal_time(49);
        f.vm.update_bgv(Some(&mut f.audio));
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 2)));
        f.call(13, 8, &[2, f.name, 1, 0]);
        f.call(13, 20, &[1]);
        f.call(13, 9, &[-1, 100]);
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 2)));
        f.call(13, 20, &[0]);
        f.call(13, 8, &[2, f.name, 1, 0]);
        f.vm.scene_skip_active = true;
        f.vm.current_line_read = true;
        f.call(13, 9, &[-1, 100]);
        assert!(!f.vm.game_audio.contains_key(&(BGV_NAMESPACE, 2)));
    }
}

#[derive(Debug, Default)]
pub(super) struct BgvState {
    lanes: [BgvLane; 8],
    restored: Option<Vec<SavedBgvTrack>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SavedBgvTrack {
    slot: i32,
    name: String,
    looping: bool,
    paused: bool,
    stop_remaining_ms: Option<u32>,
}

impl SavedBgvTrack {
    pub(super) fn native_restore(slot: i32, name: String) -> Self {
        Self { slot, name, looping: true, paused: false, stop_remaining_ms: None }
    }
}

impl ScriptRuntime {
    pub(super) fn saved_bgv_tracks(&self) -> Vec<SavedBgvTrack> {
        if let Some(tracks) = &self.bgv_state.restored {
            return tracks.clone();
        }
        self.bgv_state
            .lanes
            .iter()
            .enumerate()
            .filter_map(|(slot, lane)| {
                if lane.name.is_empty()
                    || !self.game_audio.contains_key(&(BGV_NAMESPACE, slot as i32))
                {
                    return None;
                }
                let remaining = lane
                    .stop_deadline
                    .map(|deadline| deadline.wrapping_sub(self.pal_time_ms) as i32);
                if remaining.is_some_and(|remaining| remaining <= 0) {
                    return None;
                }
                Some(SavedBgvTrack {
                    slot: slot as i32,
                    name: lane.name.clone(),
                    looping: lane.looping,
                    paused: lane.paused,
                    stop_remaining_ms: remaining.map(|value| value as u32),
                })
            })
            .collect()
    }

    pub(super) fn queue_restored_bgv(&mut self, tracks: Vec<SavedBgvTrack>) {
        // Even an empty/legacy snapshot must silence the outgoing scene's BGV.
        self.bgv_state.restored = Some(tracks);
    }

    pub(super) fn replay_restored_bgv(
        &mut self,
        mut resources: Option<&mut ResourceManager>,
        audio: Option<&mut AudioSystem>,
    ) {
        if self.bgv_state.restored.is_none() {
            return;
        }
        let Some(audio) = audio else {
            return;
        };
        for slot in 0..8 {
            self.release_bgv_lane(slot, audio);
        }
        if resources.is_none()
            && self
                .bgv_state
                .restored
                .as_ref()
                .is_some_and(|tracks| !tracks.is_empty())
        {
            return; // Keep replay pending until resources become available.
        }
        let tracks = self.bgv_state.restored.take().unwrap_or_default();
        self.apply_bgv_group_volume(Some(audio));
        for track in tracks {
            if !(0..8).contains(&track.slot)
                || track.name.is_empty()
                || track.stop_remaining_ms == Some(0)
            {
                continue;
            }
            if !self.load_named_audio(
                BGV_NAMESPACE,
                track.slot,
                PalSoundGroup::GROUP2,
                &track.name,
                i32::from(track.looping),
                100,
                false,
                None,
                resources.as_deref_mut(),
                Some(audio),
            ) {
                log::warn!(
                    "[trace-audio] restored BGV slot={} name={:?} failed",
                    track.slot,
                    track.name
                );
                continue;
            }
            let handle = self.game_audio[&(BGV_NAMESPACE, track.slot)];
            if let Err(error) = audio.play(handle, track.looping) {
                log::warn!("[trace-audio] restored BGV play failed: {error}");
            }
            if track.paused {
                let _ = audio.pause(handle);
            }
            if let Some(remaining) = track.stop_remaining_ms {
                let _ = audio.stop_fade(handle, remaining);
            }
            self.bgv_state.lanes[track.slot as usize] = BgvLane {
                name: track.name,
                looping: track.looping,
                paused: track.paused,
                stop_deadline: track
                    .stop_remaining_ms
                    .map(|remaining| self.pal_time_ms.wrapping_add(remaining)),
            };
        }
    }

    pub(super) fn ext_bgv_play(
        &mut self,
        assets: &CoreAssets,
        nls: Nls,
        resources: Option<&mut ResourceManager>,
        audio: Option<&mut AudioSystem>,
    ) -> ExtCallOutcome {
        let args = self.pop_ext_args(4);
        if args.len() != 4 {
            return ExtCallOutcome::Block;
        }
        let slot = if args[0] == -1 {
            self.next_free_audio_slot(BGV_NAMESPACE, 8)
        } else {
            args[0]
        };
        if !(0..8).contains(&slot) {
            return ExtCallOutcome::Value(0);
        }
        let Some(audio) = audio else {
            return ExtCallOutcome::Value(slot);
        };
        self.release_bgv_lane(slot, audio);
        let Some(name) = self.resolve_resource_string(args[1], assets, nls) else {
            return ExtCallOutcome::Value(slot);
        };
        // Unlike ADV voice, native BGV calls the ordinary audio loader and
        // never probes a .MIX playlist or assigns per-character voice gain.
        if name.to_ascii_lowercase().ends_with(".mix") {
            return ExtCallOutcome::Value(slot);
        }
        self.apply_bgv_group_volume(Some(audio));
        if self.load_named_audio(
            BGV_NAMESPACE,
            slot,
            PalSoundGroup::GROUP2,
            &name,
            args[2],
            100,
            false,
            None,
            resources,
            Some(audio),
        ) {
            let handle = self.game_audio[&(BGV_NAMESPACE, slot)];
            let fade = if args[2] & 4 != 0 && self.dispatch_skip_state() == 0 {
                args[3].max(0)
            } else {
                0
            };
            self.bgv_state.lanes[slot as usize].name = name;
            self.bgv_state.lanes[slot as usize].looping = args[2] & 1 != 0;
            if let Err(error) = audio.play_fade(handle, args[2] & 1 != 0, fade) {
                log::warn!("[trace-audio] BGV play failed: {error}");
            }
        }
        // The native syscall returns the selected lane even if loading fails.
        ExtCallOutcome::Value(slot)
    }

    pub(super) fn ext_bgv_stop(&mut self, audio: Option<&mut AudioSystem>) -> ExtCallOutcome {
        let args = self.pop_ext_args(2);
        if args.len() != 2 {
            return ExtCallOutcome::Block;
        }
        let Some(audio) = audio else {
            return ExtCallOutcome::Value(1);
        };
        let duration = if self.text_state.bgv_muted || self.dispatch_skip_state() != 0 {
            0
        } else {
            args[1].max(0) as u32
        };
        for slot in 0..8 {
            if args[0] != -1 && args[0] != slot {
                continue;
            }
            let Some(&handle) = self.game_audio.get(&(BGV_NAMESPACE, slot)) else {
                continue;
            };
            if duration == 0 {
                self.release_bgv_lane(slot, audio);
            } else {
                let lane = &mut self.bgv_state.lanes[slot as usize];
                lane.looping = false;
                lane.stop_deadline = Some(self.pal_time_ms.wrapping_add(duration));
                let _ = audio.stop_fade(handle, duration);
            }
        }
        ExtCallOutcome::Value(1)
    }

    pub(super) fn ext_bgv_pause(&mut self, audio: Option<&mut AudioSystem>) -> ExtCallOutcome {
        let args = self.pop_ext_args(3);
        if args.len() != 3 {
            return ExtCallOutcome::Block;
        }
        let Some(audio) = audio else {
            return ExtCallOutcome::Value(1);
        };
        for slot in 0..8 {
            if args[0] != -1 && args[0] != slot {
                continue;
            }
            let Some(&handle) = self.game_audio.get(&(BGV_NAMESPACE, slot)) else {
                continue;
            };
            let lane = &mut self.bgv_state.lanes[slot as usize];
            if lane.paused != (args[1] != 0) {
                lane.paused = args[1] != 0;
                let _ = audio.pause_fade(handle, lane.paused, args[2].max(0) as u32);
            }
        }
        ExtCallOutcome::Value(1)
    }

    pub(super) fn update_bgv(&mut self, audio: Option<&mut AudioSystem>) {
        let Some(audio) = audio else {
            return;
        };
        for slot in 0..8 {
            let Some(&handle) = self.game_audio.get(&(BGV_NAMESPACE, slot)) else {
                continue;
            };
            let lane = &self.bgv_state.lanes[slot as usize];
            let finished = lane.stop_deadline.map_or_else(
                || {
                    audio.is_enabled()
                        && !lane.looping
                        && !lane.paused
                        && !audio.is_playing(handle).unwrap_or(false)
                },
                |deadline| self.pal_time_ms.wrapping_sub(deadline) as i32 >= 0,
            );
            if finished {
                self.release_bgv_lane(slot, audio);
            }
        }
    }

    fn release_bgv_lane(&mut self, slot: i32, audio: &mut AudioSystem) {
        if let Some(handle) = self.game_audio.remove(&(BGV_NAMESPACE, slot)) {
            let _ = audio.release(handle);
        }
        self.bgv_state.lanes[slot as usize] = BgvLane::default();
    }
}

// Version-10 suffix, following Tips. BGV is independent of category-13 ADV voice.
pub(super) fn write_saved_bgv(out: &mut Vec<u8>, tracks: &[SavedBgvTrack]) -> std::io::Result<()> {
    if tracks.len() > 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too many BGV tracks",
        ));
    }
    write_u32(out, tracks.len() as u32)?;
    let mut slots = 0u8;
    for track in tracks {
        if !(0..8).contains(&track.slot)
            || slots & (1 << track.slot) != 0
            || track.name.len() > SAVE_NAME_CAP
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid BGV track",
            ));
        }
        slots |= 1 << track.slot;
        write_i32(out, track.slot)?;
        write_bytes(out, track.name.as_bytes(), SAVE_NAME_CAP)?;
        out.push(u8::from(track.looping) | (u8::from(track.paused) << 1));
        write_i32(
            out,
            track
                .stop_remaining_ms
                .map(|ms| ms.min(i32::MAX as u32) as i32)
                .unwrap_or(-1),
        )?;
    }
    Ok(())
}

pub(super) fn read_saved_bgv(reader: &mut impl Read) -> std::io::Result<Vec<SavedBgvTrack>> {
    let count = read_u32_from(reader)? as usize;
    if count > 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too many BGV tracks",
        ));
    }
    let mut tracks = Vec::with_capacity(count);
    let mut slots = 0u8;
    for _ in 0..count {
        let slot = read_i32_from(reader)?;
        let name = String::from_utf8(read_bytes_capped(reader, SAVE_NAME_CAP)?)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let mut flags = [0];
        reader.read_exact(&mut flags)?;
        let stop = read_i32_from(reader)?;
        if !(0..8).contains(&slot) || slots & (1 << slot) != 0 || flags[0] & !3 != 0 || stop < -1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid BGV track",
            ));
        }
        slots |= 1 << slot;
        tracks.push(SavedBgvTrack {
            slot,
            name,
            looping: flags[0] & 1 != 0,
            paused: flags[0] & 2 != 0,
            stop_remaining_ms: (stop >= 0).then_some(stop as u32),
        });
    }
    Ok(tracks)
}

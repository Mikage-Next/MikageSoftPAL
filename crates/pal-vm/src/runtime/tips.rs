//! Tips command state recovered from Game.exe 0x441A30..0x4420B0.
//!
//! Record production follows the layout parser (0x43C7C0). Geometry is
//! taken from the same glyph boundaries used by the portable ADV renderer.
use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TipRecord {
    // Native record: eleven dwords, a wrap flag, three 2047-byte strings.
    parameters: [i32; 11],
    wrapped: bool,
    strings: [String; 3],
}

#[derive(Clone, Debug, Default)]
pub(super) struct TipsState {
    count: usize,
    callbacks: [u32; 2], // point ids
    capture_pending: bool,
    completion_pending: bool,
    pending_records: VecDeque<(usize, bool, TipRecord)>,
    total_chars: usize,
    suspended_clock: Option<(u32, u32)>,
    resume_clock: Option<(u32, u32)>,

    mode: u16,
    records: [TipRecord; 32],
    flag: u8,
    custom_colors: bool,
    colors: [i32; 2],
}

impl ScriptRuntime {
    pub(super) fn begin_tips_text(&mut self) {
        self.tips_state.pending_records.clear();
        self.tips_state.capture_pending = true;
        self.tips_state.completion_pending = true;
        self.tips_state.mode &= 0xff00;
    }

    /// Validate before Engine releases the parked wait. Invalid callbacks
    /// must not resume the story merely because no gosub can be entered.
    pub fn queue_tips_tick_checked(&mut self, points: &PointTable, script_len: usize) -> bool {
        let point = self.tips_state.callbacks[0];
        if !matches!(points.resolve_target_pc(point), Ok(Some(pc)) if (pc as usize) < script_len) {
            return false;
        }
        self.queue_tips_tick()
    }

    /// Native 0x439480 runs the script callback on each text-task tick.
    /// Mouse hit testing belongs to that script, not to the native function.
    pub fn queue_tips_tick(&mut self) -> bool {
        if self.tips_state.mode & 0xff == 0
            || self.tips_state.callbacks[0] == 0
            || !self.tips_state.completion_pending
            || !self.pending_gosub_points.is_empty()
            || !self.modal_wait_suspensions.is_empty()
            || !matches!(self.status, RuntimeStatus::WaitClick { .. })
        {
            return false;
        }
        self.pending_gosub_points
            .push_back(self.tips_state.callbacks[0]);
        self.tips_state.flag = 1;
        true
    }

    pub(super) fn finish_tips_text(&mut self) {
        if !self.tips_state.completion_pending {
            return;
        }
        self.activate_tips_records(true);
        self.tips_state.completion_pending = false;
        if self.tips_state.mode & 0xff00 != 0 && self.tips_state.callbacks[1] != 0 {
            self.pending_gosub_points
                .push_back(self.tips_state.callbacks[1]);
            self.tips_state.flag = 0;
        }
    }

    pub(super) fn capture_tips_layout(&mut self, body: &str, lines: &[AdvTextLineLayout]) {
        capture_tip_records(
            &mut self.tips_state,
            body,
            lines,
            self.font_state.font_size(),
        );
        self.activate_tips_records(false);
    }

    pub(super) fn activate_tips_records(&mut self, complete: bool) {
        let visible = if complete || !self.text_state.reveal_enabled {
            usize::MAX
        } else {
            let elapsed = self
                .pal_time_ms
                .wrapping_sub(self.text_state.reveal_start_ms);
            let duration = self.text_state.reveal_duration_ms.max(1);
            ((u64::from(elapsed) * self.tips_state.total_chars as u64) / u64::from(duration))
                as usize
        };
        activate_tip_records(&mut self.tips_state, visible);
    }

    pub(super) fn capture_tips_snapshot(&self) -> SavedTipsState {
        let mut completed = self.tips_state.clone();
        activate_tip_records(&mut completed, usize::MAX);
        SavedTipsState::from(&completed)
    }

    pub(super) fn capture_tips_snapshot_for_body(&self, body: &str) -> SavedTipsState {
        let mut completed = self.tips_state.clone();
        if completed.capture_pending {
            if tip_spans(body).is_empty() {
                completed.capture_pending = false;
            } else {
                let mut font = self.font_state.clone();
                font.set_font_size((self.text_state.init_font_size().max(1) as u16).max(22));
                let (text, temporary_size) = parse_pal_text_directives(body);
                if let Some(size) = temporary_size {
                    font.set_font_size(size);
                }
                let (width, height, lines) = measure_wrapped_text(
                    &font,
                    &text,
                    self.text_state.init_text_width().max(1) as u32,
                );
                let (_, _, _, layout) =
                    rasterize_text_block_with_layout(&font, &lines, width, height);
                capture_tip_records(&mut completed, body, &layout, font.font_size());
            }
        }
        activate_tip_records(&mut completed, usize::MAX);
        SavedTipsState::from(&completed)
    }

    pub(super) fn restore_tips_snapshot(&mut self, saved: SavedTipsState) {
        self.tips_state = TipsState {
            count: saved.count,
            callbacks: saved.callbacks,
            completion_pending: saved.completion_pending,
            mode: saved.mode,
            records: saved.records,
            flag: saved.flag,
            custom_colors: saved.custom_colors,
            colors: saved.colors,
            ..TipsState::default()
        };
    }

    pub fn suspend_wait_for_tips(&mut self, clock: Option<(u32, u32)>) {
        self.tips_state.suspended_clock = clock;
        self.suspend_wait_for_modal();
    }

    pub(super) fn discard_tips_wait_clock(&mut self) {
        self.tips_state.suspended_clock = None;
        self.tips_state.resume_clock = None;
    }

    pub(super) fn repark_tips_clock(&mut self) {
        self.tips_state.resume_clock = self.tips_state.suspended_clock.take();
    }

    pub fn take_tips_resumed_wait_ms(&mut self) -> Option<u32> {
        self.tips_state
            .resume_clock
            .take()
            .map(|(start, duration)| duration.saturating_sub(self.pal_time_ms.wrapping_sub(start)))
    }

    pub(super) fn apply_tips_colors(
        &mut self,
        body: &str,
        lines: &[String],
        layout: &[AdvTextLineLayout],
        width: u32,
        height: u32,
        rgba: &mut [u8],
    ) {
        if !self.tips_state.custom_colors {
            return;
        }
        let spans = tip_spans(body);
        if spans.is_empty() {
            return;
        }
        let saved = self.font_state.color();
        self.font_state.set_color(
            self.tips_state.colors[0] as u32,
            self.tips_state.colors[1] as u32,
        );
        let (_, _, colored, _) =
            rasterize_text_block_with_layout(&self.font_state, lines, width, height);
        self.font_state.set_color(saved.0, saved.1);
        for span in spans {
            let start = visible_tip_offset(&body[..span.start]);
            let end = visible_tip_offset(&body[..span.end]);
            for line in layout {
                let from = start.max(line.char_start);
                let to = end.min(line.char_start + line.char_count);
                if from >= to {
                    continue;
                }
                let left = line.char_x[from - line.char_start].min(width);
                let right = line.char_x[to - line.char_start].min(width);
                for y in line.y..line.y.saturating_add(line.height).min(height) {
                    let from = ((y * width + left) * 4) as usize;
                    let to = ((y * width + right) * 4) as usize;
                    rgba[from..to].copy_from_slice(&colored[from..to]);
                }
            }
        }
    }

    pub(super) fn dispatch_tips_ext(&mut self, index: u16) -> ExtCallOutcome {
        let count = match index {
            38 => 3,
            36 | 39 | 42 => 2,
            41 => 1,
            37 | 40 | 43..=46 => 0,
            _ => return ExtCallOutcome::Skip,
        };
        let args = self.pop_ext_args(count);
        if args.len() != count {
            return ExtCallOutcome::Block;
        }
        match index {
            36 => {
                // 0x441950 sets two callbacks and zeroes only the 32 records.
                // Count/mode/flag/colors are outside that memset range.
                self.tips_state.callbacks = [args[0] as u32, args[1] as u32];
                self.tips_state.records = Default::default();
                ExtCallOutcome::Value(1)
            }
            // 0x441A30 reports the last completed index, not record count.
            37 => ExtCallOutcome::Value(self.tips_state.count.saturating_sub(1) as i32),
            38 => {
                // Native pop order: record index, destination string, selector.
                let Some(record) = usize::try_from(args[0])
                    .ok()
                    .and_then(|i| self.tips_state.records.get(i))
                else {
                    return ExtCallOutcome::Value(0);
                };
                let Some(value) = usize::try_from(args[2])
                    .ok()
                    .and_then(|i| record.strings.get(i))
                    .cloned()
                else {
                    return ExtCallOutcome::Value(0);
                };
                let id = record.parameters[0];
                if !is_dynamic_string_handle(args[1]) {
                    return ExtCallOutcome::Value(0);
                }
                self.replace_dynamic_string(args[1], value);
                ExtCallOutcome::Value(id)
            }
            39 => {
                // 0x441BD0: (record index, selector), in pop order.
                // 0x441400 stores the text_init arguments directly into
                // ctx dwords 4,6,7,10,11,5. These queries return the raw
                // script values, independent of the current PAL font or
                // the renderer's coordinate projection. They never read
                // the record pointer, so the index is immaterial here.
                let context_value = match args[1] {
                    12 => Some(self.text_state.init_font_size()),
                    13 => Some(self.text_state.init_body_x()),
                    14 => Some(self.text_state.init_body_y()),
                    15 => Some(self.text_state.init_name_x()),
                    16 => Some(self.text_state.init_name_y()),
                    17 => Some(self.text_state.init_mode()),
                    _ => None,
                };
                if let Some(value) = context_value {
                    return ExtCallOutcome::Value(value);
                }
                let Some(record) = usize::try_from(args[0])
                    .ok()
                    .and_then(|i| self.tips_state.records.get(i))
                else {
                    return ExtCallOutcome::Value(0);
                };
                let value = match args[1] {
                    0..=8 => record.parameters[args[1] as usize],
                    9 => i32::from(record.wrapped),
                    10 | 11 => {
                        let padding = match self.font_state.effect() {
                            0 => 0,
                            1 | 2 => 2,
                            3 => 4,
                            _ => return ExtCallOutcome::Value(0),
                        };
                        record.parameters[args[1] as usize - 1].wrapping_add(padding)
                    }
                    _ => 0,
                };
                ExtCallOutcome::Value(value)
            }
            40 => {
                // Reset clears the current slot AND slot zero, preserving
                // other records, the pause byte and the configured colors.
                if let Some(record) = self.tips_state.records.get_mut(self.tips_state.count) {
                    *record = TipRecord::default();
                }
                self.tips_state.count = 0;
                self.tips_state.mode = 1;
                self.tips_state.records[0] = TipRecord::default();
                ExtCallOutcome::Value(1)
            }
            41 => {
                // Search accepts a dynamic string only, uses exact strcmp,
                // and keeps scanning so the LAST matching record wins.
                let found = dynamic_string_index(args[0])
                    .and_then(|i| self.dynamic_strings.get(i))
                    .and_then(|needle| {
                        self.tips_state
                            .records
                            .iter()
                            .take(self.tips_state.count)
                            .rposition(|r| r.strings[0] == *needle)
                    })
                    .map_or(-1, |i| i as i32);
                ExtCallOutcome::Value(found)
            }
            42 => {
                self.text_state.dirty = true;
                self.tips_state.custom_colors = args[0] != -1;
                if self.tips_state.custom_colors {
                    self.tips_state.colors = [args[0], args[1]];
                }
                ExtCallOutcome::Value(1)
            }
            43 => {
                self.tips_state.mode = 256;
                ExtCallOutcome::Value(1)
            }
            44 => ExtCallOutcome::Value(i32::from(self.tips_state.flag)),
            45 => {
                self.tips_state = TipsState::default();
                ExtCallOutcome::Value(1)
            }
            46 => {
                let old = self.tips_state.flag;
                self.tips_state.flag ^= 1;
                ExtCallOutcome::Value(i32::from(old))
            }
            _ => unreachable!(),
        }
    }
}

fn capture_tip_records(
    state: &mut TipsState,
    body: &str,
    lines: &[AdvTextLineLayout],
    font_size: u16,
) {
    if !state.capture_pending {
        return;
    }
    state.capture_pending = false;
    state.mode &= 0xff00;
    state.total_chars = lines.iter().map(|line| line.char_count).sum();
    for span in tip_spans(body) {
        let start = visible_tip_offset(&body[..span.start]);
        let end = visible_tip_offset(&body[..span.end]);
        let Some((first, start_x)) = tip_boundary(lines, start, false) else {
            continue;
        };
        let Some((last, end_x)) = tip_boundary(lines, end, true) else {
            continue;
        };
        let mut record = TipRecord::default();
        record.parameters[0] = span.id;
        record.parameters[1] = start_x as i32;
        record.parameters[2] = lines[first].y as i32;
        record.wrapped = first != last;
        record.strings = [
            body[span.start..span.end].to_owned(),
            String::new(),
            String::new(),
        ];
        if record.wrapped {
            // Native overwrites the split on every wrap, so three-line
            // terms retain the final split rather than the first one.
            let split_at = lines[last].char_start.saturating_sub(start);
            let raw = &body[span.start..span.end];
            let split = raw_tip_boundary(raw, split_at);
            record.strings[1] = raw[..split].to_owned();
            record.strings[2] = raw[split..].to_owned();
            record.parameters[3] = lines[last - 1].char_x.last().copied().unwrap_or(0) as i32;
            record.parameters[4] = lines[last].y as i32;
            record.parameters[5] = 0;
            record.parameters[6] = lines[last].y as i32;
            record.parameters[7] = end_x as i32;
            record.parameters[8] = lines[last].y.saturating_add(lines[last].height) as i32;
        } else {
            record.parameters[3] = end_x as i32;
            record.parameters[4] = lines[first].y.saturating_add(lines[first].height) as i32;
        }
        // These are font-cell width and line pitch, not term width/height.
        record.parameters[9] = i32::from(font_size);
        record.parameters[10] = lines[last].height as i32;
        state
            .pending_records
            .push_back((start, true, record.clone()));
        state.pending_records.push_back((end, false, record));
    }
}

fn activate_tip_records(state: &mut TipsState, visible: usize) {
    while state
        .pending_records
        .front()
        .is_some_and(|(position, _, _)| *position <= visible)
    {
        let (_, opening, mut record) = state.pending_records.pop_front().unwrap();
        if opening {
            if state.count == 31 {
                state.count = 0;
            }
            let current = &mut state.records[state.count];
            current.parameters[..3].copy_from_slice(&record.parameters[..3]);
            current.wrapped = false;
        } else {
            if !record.wrapped {
                record.parameters[5..9]
                    .copy_from_slice(&state.records[state.count].parameters[5..9]);
            }
            state.records[state.count] = record;
            state.count += 1;
            state.mode = 257;
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SavedTipsState {
    count: usize,
    callbacks: [u32; 2],
    completion_pending: bool,
    mode: u16,
    records: [TipRecord; 32],
    flag: u8,
    custom_colors: bool,
    colors: [i32; 2],
}

impl From<&TipsState> for SavedTipsState {
    fn from(state: &TipsState) -> Self {
        Self {
            count: state.count,
            callbacks: state.callbacks,
            completion_pending: state.completion_pending,
            mode: state.mode,
            records: state.records.clone(),
            flag: state.flag,
            custom_colors: state.custom_colors,
            colors: state.colors,
        }
    }
}

const TIPS_STRING_CAP: usize = 8192;

fn tips_save_error(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

pub(super) fn write_tips_snapshot(
    out: &mut Vec<u8>,
    state: &SavedTipsState,
) -> std::io::Result<()> {
    if state.count >= 32 || state.flag > 1 {
        return Err(tips_save_error("invalid Tips control state"));
    }
    out.extend_from_slice(b"TIP1");
    write_u32(out, state.count as u32)?;
    for callback in state.callbacks {
        write_u32(out, callback)?;
    }
    write_u32(out, u32::from(state.mode))?;
    out.extend_from_slice(&[
        state.flag,
        u8::from(state.custom_colors),
        u8::from(state.completion_pending),
    ]);
    for color in state.colors {
        write_i32(out, color)?;
    }
    write_u32(out, 32)?;
    for record in &state.records {
        for parameter in record.parameters {
            write_i32(out, parameter)?;
        }
        out.push(u8::from(record.wrapped));
        for value in &record.strings {
            if value.len() > TIPS_STRING_CAP {
                return Err(tips_save_error("Tips string too long"));
            }
            write_bytes(out, value.as_bytes(), TIPS_STRING_CAP)?;
        }
    }
    Ok(())
}

pub(super) fn read_tips_snapshot(reader: &mut impl Read) -> std::io::Result<SavedTipsState> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic)?;
    if &magic != b"TIP1" {
        return Err(tips_save_error("bad Tips save magic"));
    }
    let count = read_u32_from(reader)? as usize;
    if count >= 32 {
        return Err(tips_save_error("invalid Tips record cursor"));
    }
    let callbacks = [read_u32_from(reader)?, read_u32_from(reader)?];
    let mode =
        u16::try_from(read_u32_from(reader)?).map_err(|_| tips_save_error("invalid Tips mode"))?;
    let mut flags = [0; 3];
    reader.read_exact(&mut flags)?;
    if flags.iter().any(|&flag| flag > 1) {
        return Err(tips_save_error("invalid Tips flag"));
    }
    let colors = [read_i32_from(reader)?, read_i32_from(reader)?];
    if read_u32_from(reader)? != 32 {
        return Err(tips_save_error("invalid Tips record count"));
    }
    let mut state = SavedTipsState {
        count,
        callbacks,
        mode,
        colors,
        flag: flags[0],
        custom_colors: flags[1] != 0,
        completion_pending: flags[2] != 0,
        ..SavedTipsState::default()
    };
    for record in &mut state.records {
        for parameter in &mut record.parameters {
            *parameter = read_i32_from(reader)?;
        }
        let mut flag = [0];
        reader.read_exact(&mut flag)?;
        if flag[0] > 1 {
            return Err(tips_save_error("invalid Tips wrap flag"));
        }
        record.wrapped = flag[0] != 0;
        for value in &mut record.strings {
            *value = String::from_utf8(read_bytes_capped(reader, TIPS_STRING_CAP)?)
                .map_err(|_| tips_save_error("invalid UTF-8 in Tips string"))?;
        }
    }
    Ok(state)
}

struct TipSpan {
    id: i32,
    start: usize,
    end: usize,
}

fn visible_tip_offset(text: &str) -> usize {
    parse_pal_text_directives(text)
        .0
        .chars()
        .filter(|ch| *ch != '\n')
        .count()
}

fn raw_tip_boundary(text: &str, wanted: usize) -> usize {
    let mut offset = 0;
    let mut visible = 0;
    while offset < text.len() {
        if text[offset..].starts_with('<') {
            if let Some(end) = text[offset..].find('>') {
                if is_pal_text_tag(&text[offset + 1..offset + end]) {
                    offset += end + 1;
                    continue;
                }
            }
        }
        let ch = text[offset..].chars().next().unwrap();
        if ch != '\n' {
            if visible == wanted {
                return offset;
            }
            visible += 1;
        }
        offset += ch.len_utf8();
    }
    text.len()
}

fn tip_boundary(lines: &[AdvTextLineLayout], offset: usize, ending: bool) -> Option<(usize, u32)> {
    for (i, line) in lines.iter().enumerate() {
        let end = line.char_start + line.char_count;
        if offset >= line.char_start
            && (offset < end || ending && offset == end || i + 1 == lines.len() && offset == end)
        {
            return Some((i, *line.char_x.get(offset - line.char_start)?));
        }
    }
    None
}

fn tip_spans(text: &str) -> Vec<TipSpan> {
    let mut spans = Vec::new();
    let mut opening = None;
    let mut offset = 0;
    while let Some(relative) = text[offset..].find('<') {
        let start = offset + relative;
        let Some(length) = text[start..].find('>') else {
            break;
        };
        let tag = &text[start + 1..start + length];
        offset = start + length + 1;
        if tag == "/d" {
            if let Some((id, content)) = opening.take() {
                spans.push(TipSpan {
                    id,
                    start: content,
                    end: start,
                });
            }
        } else if let Some(value) = tag.strip_prefix('d') {
            if value.is_empty() || value.chars().all(|ch| ch.is_ascii_digit()) {
                opening = Some((value.parse().unwrap_or(-1), offset));
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> ScriptRuntime {
        ScriptRuntime::boot(12, ScriptRuntimeConfig::default())
    }

    fn call(vm: &mut ScriptRuntime, index: u16, pop_order: &[i32]) -> ExtCallOutcome {
        vm.stack.push(0x123456);
        vm.stack.extend(pop_order.iter().rev());
        let result = vm.dispatch_tips_ext(index);
        assert_eq!(
            vm.stack.pop(),
            Some(0x123456),
            "wrong stack consumption for {index}"
        );
        assert!(vm.stack.is_empty());
        result
    }

    fn layout() -> Vec<AdvTextLineLayout> {
        vec![
            AdvTextLineLayout {
                y: 0,
                height: 12,
                char_start: 0,
                char_count: 4,
                char_x: vec![0, 10, 20, 30, 40],
            },
            AdvTextLineLayout {
                y: 16,
                height: 12,
                char_start: 4,
                char_count: 4,
                char_x: vec![0, 10, 20, 30, 40],
            },
        ]
    }

    #[test]
    fn tips_markup_produces_strings_and_native_record_lanes_from_draw_layout() {
        let mut vm = runtime();
        vm.begin_tips_text();
        let text = "AB<d42>CDEF</d>GH";
        assert_eq!(parse_pal_text_directives(text).0, "ABCDEFGH");
        vm.capture_tips_layout(text, &layout());
        let record = &vm.tips_state.records[0];
        assert_eq!(record.parameters[..9], [42, 20, 0, 40, 16, 0, 16, 20, 28]);
        assert!(record.wrapped);
        assert_eq!(record.strings, ["CDEF", "CD", "EF"]);
        assert_eq!(vm.tips_state.count, 1);
        assert_eq!(vm.tips_state.mode, 257);
        vm.capture_tips_layout(text, &layout());
        assert_eq!(vm.tips_state.count, 1, "redraw must not duplicate records");
    }

    #[test]
    fn tips_end_at_wrap_boundary_stays_on_the_first_line_and_ring_skips_slot_31() {
        let mut vm = runtime();
        vm.tips_state.count = 31;
        vm.begin_tips_text();
        vm.capture_tips_layout("AB<d7>CD</d>EFGH", &layout());
        assert_eq!(vm.tips_state.count, 1);
        let record = &vm.tips_state.records[0];
        assert_eq!(record.parameters[..5], [7, 20, 0, 40, 12]);
        assert!(!record.wrapped);
        assert_eq!(record.strings, ["CD", "", ""]);
    }

    #[test]
    fn tips_callbacks_run_once_per_tick_without_recursive_modal_entry() {
        let mut vm = runtime();
        vm.tips_state.callbacks = [7, 9];
        vm.begin_tips_text();
        vm.capture_tips_layout("<d42>AB</d>CDEFGH", &layout());
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        assert!(vm.queue_tips_tick());
        assert_eq!(vm.tips_state.flag, 1);
        assert!(!vm.queue_tips_tick());
        vm.suspend_wait_for_modal();
        vm.pending_gosub_points.clear();
        assert!(!vm.queue_tips_tick());
        assert_eq!(vm.take_modal_wait_repark(), Some(WaitRequest::Click));
        assert_eq!(vm.pc, 12);
        assert!(vm.modal_wait_suspensions.is_empty());
        vm.resolve_pending_wait();
        assert_eq!(vm.pending_gosub_points.pop_front(), Some(9));
        assert_eq!(vm.tips_state.flag, 0);
        vm.finish_tips_text();
        assert!(vm.pending_gosub_points.is_empty());
    }

    #[test]
    fn tips_poll_preserves_auto_deadline_across_repeated_callbacks_and_clock_wrap() {
        let mut vm = runtime();
        let mut tasks = TaskSystem::new();
        let mut sprites = SpriteSystem::new();
        let input = PalInputState::new();
        let start = u32::MAX - 50;
        tasks.set_pal_time(start);
        vm.set_pal_time(start);
        vm.tips_state.callbacks = [7, 9];
        vm.begin_tips_text();
        vm.capture_tips_layout("<d42>AB</d>CDEFGH", &layout());
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        let mut handle = tasks.create_wait_click_or_time_gated(100, true).unwrap();
        vm.set_wait_handle(handle, WaitRequest::AutoClickOrTime(100));
        for elapsed in [20, 40, 70] {
            let now = start.wrapping_add(elapsed);
            tasks.set_pal_time(now);
            vm.set_pal_time(now);
            assert!(vm.queue_tips_tick());
            let clock = tasks.wait_clock(handle);
            tasks.free(handle);
            vm.suspend_wait_for_tips(clock);
            vm.pending_gosub_points.clear();
            assert_eq!(
                vm.take_modal_wait_repark(),
                Some(WaitRequest::AutoClickOrTime(100))
            );
            let remaining = vm.take_tips_resumed_wait_ms().unwrap();
            assert_eq!(remaining, 100 - elapsed);
            handle = tasks
                .create_wait_click_or_time_gated(remaining, true)
                .unwrap();
            vm.set_wait_handle(handle, WaitRequest::AutoClickOrTime(100));
            tasks.process(&mut sprites, &input, false, false);
            assert!(tasks.is_alive(handle));
        }
        tasks.set_pal_time(start.wrapping_add(100));
        assert!(!tasks.wait_can_poll(handle, false));
        assert!(
            tasks.wait_can_poll(handle, true),
            "the original voice gate remains active"
        );
        tasks.process(&mut sprites, &input, false, false);
        assert!(
            !tasks.wait_can_poll(handle, false),
            "pending-free waits must not restart polling"
        );
        tasks.process(&mut sprites, &input, false, false);
        assert!(!tasks.is_alive(handle));
        vm.resolve_pending_wait();
        assert_eq!(vm.pending_gosub_points.pop_front(), Some(9));
        assert!(!vm.queue_tips_tick());
    }

    #[test]
    fn tips_activate_each_record_when_its_closing_tag_is_revealed() {
        let mut vm = runtime();
        vm.tips_state.callbacks = [7, 9];
        vm.text_state.reveal_enabled = true;
        vm.text_state.reveal_duration_ms = 800;
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        vm.begin_tips_text();
        vm.capture_tips_layout("<d1>AB</d>CD<d2>EF</d>GH", &layout());
        assert_eq!(vm.tips_state.count, 0);
        assert_eq!(
            vm.tips_state.records[0].parameters[0], 1,
            "the open record exposes its ID before completion"
        );
        assert!(vm.tips_state.records[0].strings[0].is_empty());
        assert!(!vm.queue_tips_tick());
        for (time, count) in [(199, 0), (200, 1), (599, 1), (600, 2)] {
            vm.set_pal_time(time);
            vm.activate_tips_records(false);
            assert_eq!(vm.tips_state.count, count);
        }
        assert!(vm.text_state.reveal_enabled);
        assert!(
            vm.queue_tips_tick(),
            "poll starts before the complete line is revealed"
        );
        assert_eq!(vm.tips_state.records[0].strings[0], "AB");
        assert_eq!(vm.tips_state.records[1].strings[0], "EF");
    }

    #[test]
    fn tips_custom_colors_change_only_the_tagged_glyph_cells() {
        let mut vm = runtime();
        vm.font_state.set_font_size(24);
        vm.font_state.set_color(0xffffffff, 0xff000000);
        let body = "A<d7>AA</d>A";
        let (width, height, lines) = measure_wrapped_text(&vm.font_state, "AAAA", 1000);
        let (_, _, mut pixels, layout) =
            rasterize_text_block_with_layout(&vm.font_state, &lines, width, height);
        let original = pixels.clone();
        call(&mut vm, 42, &[0xffff0000_u32 as i32, 0xff0000ff_u32 as i32]);
        vm.apply_tips_colors(body, &lines, &layout, width, height, &mut pixels);
        assert_ne!(pixels, original);
        let left = layout[0].char_x[1];
        let right = layout[0].char_x[3];
        let mut red = 0;
        for (index, pixel) in pixels.chunks_exact(4).enumerate() {
            let x = index as u32 % width;
            if x < left || x >= right {
                assert_eq!(pixel, &original[index * 4..index * 4 + 4]);
            } else if pixel[0] > pixel[1] && pixel[3] > 0 {
                red += 1;
            }
        }
        assert!(red > 0);
        assert_eq!(vm.font_state.color(), (0xffffffff, 0xff000000));
        call(&mut vm, 42, &[-1, 0]);
        let mut disabled = original.clone();
        vm.apply_tips_colors(body, &lines, &layout, width, height, &mut disabled);
        assert_eq!(disabled, original);
    }

    #[test]
    fn tips_callback_wait_does_not_finish_the_suspended_adv_line() {
        let mut vm = runtime();
        vm.tips_state.callbacks = [7, 9];
        vm.begin_tips_text();
        vm.capture_tips_layout("<d42>AB</d>CDEFGH", &layout());
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        assert!(vm.queue_tips_tick());
        vm.suspend_wait_for_tips(None);
        vm.pending_gosub_points.clear();
        vm.status = RuntimeStatus::WaitClick { pc: 90 };
        vm.resolve_pending_wait();
        assert!(vm.tips_state.completion_pending);
        assert!(vm.pending_gosub_points.is_empty());
    }

    #[test]
    fn tips_save_roundtrip_completes_only_the_snapshot_and_restores_callbacks_and_queries() {
        let mut vm = runtime();
        vm.text_state.init_args = [10, 0, 0, 200, 700, 0, 0, 28];
        vm.text_state.reveal_enabled = true;
        vm.text_state.reveal_duration_ms = 800;
        call(&mut vm, 36, &[7, 9]);
        call(&mut vm, 42, &[0xff123456_u32 as i32, 0xff654321_u32 as i32]);
        call(&mut vm, 46, &[]);
        vm.begin_tips_text();
        let body = "<d1>AB</d>CD<d2>EF</d>GH";
        vm.capture_tips_layout(body, &layout());
        vm.tips_state.records[31].strings[2] = "retained unused record".into();
        vm.tips_state.suspended_clock = Some((50, 100));
        vm.tips_state.resume_clock = Some((75, 100));
        let mut snapshot = vm.capture_save_snapshot();
        snapshot.resume_wait_click = true;
        assert_eq!(snapshot.tips.count, 2);
        assert_eq!(vm.tips_state.count, 0);
        assert!(!vm.tips_state.pending_records.is_empty());
        let bytes = encode_runtime_save_snapshot(&snapshot).unwrap();
        let saved = decode_runtime_save_snapshot(&mut bytes.as_slice()).unwrap();
        assert_eq!(saved.tips, snapshot.tips);
        let mut restored = runtime();
        restored.restore_save_snapshot(saved);
        assert!(!restored.text_state.reveal_enabled);
        assert_eq!(restored.tips_state.count, 2);
        assert_eq!(
            restored.tips_state.records[31].strings[2],
            "retained unused record"
        );
        assert!(restored.tips_state.pending_records.is_empty());
        assert!(!restored.tips_state.capture_pending);
        assert_eq!(restored.tips_state.suspended_clock, None);
        assert_eq!(restored.tips_state.resume_clock, None);
        restored.capture_tips_layout(body, &layout());
        assert_eq!(
            restored.tips_state.count, 2,
            "restored redraw cannot append duplicates"
        );
        let needle = restored.store_dynamic_string("EF".into());
        let dst = restored.store_dynamic_string(String::new());
        assert_eq!(call(&mut restored, 41, &[needle]), ExtCallOutcome::Value(1));
        assert_eq!(
            call(&mut restored, 38, &[1, dst, 0]),
            ExtCallOutcome::Value(2)
        );
        assert_eq!(
            restored.dynamic_strings[dynamic_string_index(dst).unwrap()],
            "EF"
        );
        restored.status = RuntimeStatus::WaitClick { pc: 12 };
        restored.resolve_pending_wait();
        assert_eq!(restored.pending_gosub_points.pop_front(), Some(9));
        assert_eq!(restored.tips_state.flag, 0);
        call(&mut restored, 40, &[]);
        assert_eq!(restored.tips_state.count, 0);
        assert!(restored.tips_state.custom_colors);
        assert_eq!(
            restored.tips_state.colors,
            [0xff123456_u32 as i32, 0xff654321_u32 as i32]
        );
    }

    #[test]
    fn tips_checkpoint_before_first_render_captures_the_fully_visible_body() {
        let mut vm = runtime();
        vm.text_state.init_args = [10, 0, 0, 200, 700, 0, 0, 28];
        vm.text_state.reveal_enabled = true;
        vm.text_state.reveal_duration_ms = 1000;
        vm.begin_tips_text();
        let saved = vm.capture_tips_snapshot_for_body("<d10>first</d> and <d20>second</d>");
        assert_eq!(saved.count, 2);
        assert_eq!(saved.records[1].strings[0], "second");
        assert!(saved.completion_pending);
        assert_eq!(vm.tips_state.count, 0);
        assert!(vm.tips_state.capture_pending);
        assert!(vm.tips_state.pending_records.is_empty());
    }

    #[test]
    fn tips_version_nine_without_tail_loads_as_empty_state() {
        let vm = runtime();
        let mut bytes = encode_runtime_save_snapshot(&vm.capture_save_snapshot()).unwrap();
        let tail = bytes.windows(4).position(|value| value == b"TIP1").unwrap();
        bytes.truncate(tail);
        bytes[8..12].copy_from_slice(&9_u32.to_le_bytes());
        let saved = decode_runtime_save_snapshot(&mut bytes.as_slice()).unwrap();
        assert_eq!(saved.version, 9);
        assert_eq!(saved.tips, SavedTipsState::default());
        let mut restored = runtime();
        restored.tips_state.count = 4;
        restored.tips_state.callbacks = [7, 9];
        restored.restore_save_snapshot(saved);
        assert_eq!(restored.tips_state.count, 0);
        assert_eq!(restored.tips_state.callbacks, [0, 0]);
    }

    #[test]
    fn tips_save_decoder_rejects_bad_counts_flags_utf8_lengths_and_truncation() {
        let mut state = SavedTipsState::default();
        state.records[0].strings[0] = "term".into();
        let mut bytes = Vec::new();
        write_tips_snapshot(&mut bytes, &state).unwrap();
        for (offset, invalid) in [(4, 32_u32), (16, 65536), (31, 31), (80, u32::MAX)] {
            let mut bad = bytes.clone();
            bad[offset..offset + 4].copy_from_slice(&invalid.to_le_bytes());
            assert!(
                read_tips_snapshot(&mut bad.as_slice()).is_err(),
                "offset {offset}"
            );
        }
        for offset in [20, 21, 22, 79, 84] {
            let mut bad = bytes.clone();
            bad[offset] = 255;
            assert!(
                read_tips_snapshot(&mut bad.as_slice()).is_err(),
                "offset {offset}"
            );
        }
        for length in [0, 4, 23, 35, 83, bytes.len() - 1] {
            assert!(read_tips_snapshot(&mut &bytes[..length]).is_err());
        }
        state.records[0].strings[0] = "a".repeat(TIPS_STRING_CAP + 1);
        assert!(write_tips_snapshot(&mut Vec::new(), &state).is_err());
        state.records[0].strings[0].clear();
        state.count = 32;
        assert!(write_tips_snapshot(&mut Vec::new(), &state).is_err());
    }

    #[test]
    fn abandoned_tips_callback_cannot_replace_a_later_menu_wait_deadline() {
        let mut vm = runtime();
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        vm.wait_task_kind = Some(WaitRequest::AutoClickOrTime(100));
        vm.suspend_wait_for_tips(Some((10, 100)));
        vm.pc = 90;
        assert_eq!(vm.take_modal_wait_repark(), None);
        assert_eq!(vm.tips_state.suspended_clock, None);
        assert_eq!(vm.tips_state.resume_clock, None);

        vm.set_pal_time(500);
        vm.status = RuntimeStatus::WaitClick { pc: 90 };
        vm.wait_task_kind = Some(WaitRequest::AutoClickOrTime(5000));
        vm.suspend_wait_for_modal();
        assert_eq!(
            vm.take_modal_wait_repark(),
            Some(WaitRequest::AutoClickOrTime(5000))
        );
        assert_eq!(vm.take_tips_resumed_wait_ms(), None,
            "an unrelated menu must retain its own timer instead of the abandoned expired Tips clock");
    }

    #[test]
    fn abandoning_nested_menu_preserves_the_still_active_outer_tips_clock() {
        let mut vm = runtime();
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        vm.wait_task_kind = Some(WaitRequest::AutoClickOrTime(1000));
        vm.suspend_wait_for_tips(Some((10, 1000)));
        vm.call_stack.push(12);
        vm.pc = 90;
        vm.status = RuntimeStatus::WaitClick { pc: 90 };
        vm.wait_task_kind = Some(WaitRequest::Click);
        vm.suspend_wait_for_modal();
        vm.pc = 91;
        assert_eq!(vm.take_modal_wait_repark(), None);
        assert_eq!(vm.tips_state.suspended_clock, Some((10, 1000)));
        assert_eq!(vm.modal_wait_suspensions.len(), 1);
        vm.pc = vm.call_stack.pop().unwrap();
        vm.set_pal_time(110);
        assert_eq!(
            vm.take_modal_wait_repark(),
            Some(WaitRequest::AutoClickOrTime(1000))
        );
        assert_eq!(vm.take_tips_resumed_wait_ms(), Some(900));
    }

    #[test]
    fn invalid_tips_callback_keeps_the_existing_adv_wait_parked() {
        let mut vm = runtime();
        vm.tips_state.mode = 257;
        vm.tips_state.completion_pending = true;
        vm.status = RuntimeStatus::WaitClick { pc: 12 };
        vm.wait_task_kind = Some(WaitRequest::Click);
        for (id, offset, script_len) in
            [(0, 4_u32, 32), (2, 4, 32), (1, u32::MAX, 32), (1, 1000, 32)]
        {
            vm.tips_state.callbacks = [id, 9];
            let points = PointTable::parse(&offset.to_le_bytes()).unwrap();
            assert!(!vm.queue_tips_tick_checked(&points, script_len));
            assert!(matches!(vm.status, RuntimeStatus::WaitClick { pc: 12 }));
            assert_eq!(vm.wait_task_kind, Some(WaitRequest::Click));
            assert!(vm.pending_gosub_points.is_empty());
            assert!(vm.modal_wait_suspensions.is_empty());
            assert_eq!(vm.pc, 12);
        }
        vm.tips_state.callbacks[0] = 1;
        let points = PointTable::parse(&4_u32.to_le_bytes()).unwrap();
        assert!(vm.queue_tips_tick_checked(&points, 32));
        assert_eq!(vm.pending_gosub_points.pop_front(), Some(1));
    }

    #[test]
    fn tips_reject_unclosed_terms_and_preserve_unknown_markup() {
        assert!(tip_spans("prefix <d5>unclosed").is_empty());
        assert_eq!(tip_spans("<d>term</d>")[0].id, -1);
        assert_eq!(
            parse_pal_text_directives("prefix <dialog>text</dialog>").0,
            "prefix <dialog>text</dialog>"
        );
    }

    #[test]
    fn tips_process_registration_clears_records_without_resetting_control_state() {
        let mut vm = runtime();
        vm.tips_state.count = 3;
        vm.tips_state.mode = 257;
        vm.tips_state.records[2].strings[0] = "old".into();
        call(&mut vm, 42, &[11, 22]);
        call(&mut vm, 46, &[]);
        assert_eq!(call(&mut vm, 36, &[7, 9]), ExtCallOutcome::Value(1));
        assert_eq!(vm.tips_state.callbacks, [7, 9]);
        assert!(vm.tips_state.records[2].strings[0].is_empty());
        assert_eq!(vm.tips_state.count, 3);
        assert_eq!(vm.tips_state.mode, 257);
        assert_eq!(vm.tips_state.colors, [11, 22]);
        assert_eq!(vm.tips_state.flag, 1);
        call(&mut vm, 36, &[0, 0]);
        assert_eq!(vm.tips_state.callbacks, [0, 0]);
    }

    #[test]
    fn tips_pause_returns_old_flag_and_reset_does_not_unpause() {
        let mut vm = runtime();
        assert_eq!(call(&mut vm, 46, &[]), ExtCallOutcome::Value(0));
        assert_eq!(call(&mut vm, 44, &[]), ExtCallOutcome::Value(1));
        call(&mut vm, 40, &[]);
        assert_eq!(vm.tips_state.mode, 1);
        assert_eq!(call(&mut vm, 44, &[]), ExtCallOutcome::Value(1));
        call(&mut vm, 43, &[]);
        assert_eq!(vm.tips_state.mode, 256);
        assert_eq!(call(&mut vm, 46, &[]), ExtCallOutcome::Value(1));
        assert_eq!(call(&mut vm, 44, &[]), ExtCallOutcome::Value(0));
    }

    #[test]
    fn tips_string_destination_and_duplicate_search_follow_native_contract() {
        let mut vm = runtime();
        vm.tips_state.count = 3;
        for i in [0, 2] {
            vm.tips_state.records[i].parameters[0] = 400 + i as i32;
            vm.tips_state.records[i].strings[0] = "term".into();
        }
        vm.tips_state.records[2].strings[1] = "first line".into();
        let dst = vm.store_dynamic_string("old".into());
        let needle = vm.store_dynamic_string("term".into());
        assert_eq!(call(&mut vm, 41, &[needle]), ExtCallOutcome::Value(2));
        assert_eq!(call(&mut vm, 37, &[]), ExtCallOutcome::Value(2));
        assert_eq!(call(&mut vm, 38, &[2, dst, 1]), ExtCallOutcome::Value(402));
        assert_eq!(vm.dynamic_strings[0], "first line");
        assert_eq!(call(&mut vm, 38, &[2, dst, 3]), ExtCallOutcome::Value(0));
        assert_eq!(vm.dynamic_strings[0], "first line");
        assert_eq!(call(&mut vm, 41, &[17]), ExtCallOutcome::Value(-1));
    }

    #[test]
    fn tips_reset_preserves_other_records_colors_and_flag_but_init_clears_all() {
        let mut vm = runtime();
        vm.tips_state.count = 2;
        for i in 0..3 {
            vm.tips_state.records[i].parameters[0] = 99;
        }
        call(&mut vm, 42, &[0x1234, 0x5678]);
        call(&mut vm, 42, &[-1, 0]);
        assert!(!vm.tips_state.custom_colors);
        assert_eq!(vm.tips_state.colors, [0x1234, 0x5678]);
        call(&mut vm, 40, &[]);
        assert_eq!(call(&mut vm, 39, &[0, 0]), ExtCallOutcome::Value(0));
        assert_eq!(call(&mut vm, 39, &[1, 0]), ExtCallOutcome::Value(99));
        assert_eq!(call(&mut vm, 39, &[2, 0]), ExtCallOutcome::Value(0));
        call(&mut vm, 45, &[]);
        assert_eq!(vm.tips_state.colors, [0, 0]);
        assert_eq!(call(&mut vm, 39, &[1, 0]), ExtCallOutcome::Value(0));
    }

    #[test]
    fn tips_dimensions_include_native_font_effect_padding_and_reject_invalid_queries() {
        let mut vm = runtime();
        vm.tips_state.records[0].parameters[9] = 100;
        vm.tips_state.records[0].parameters[10] = 28;
        vm.tips_state.records[0].wrapped = true;
        for (effect, padding) in [(0, 0), (1, 2), (2, 2), (3, 4)] {
            vm.font_state.set_effect(effect);
            assert_eq!(
                call(&mut vm, 39, &[0, 10]),
                ExtCallOutcome::Value(100 + padding)
            );
            assert_eq!(
                call(&mut vm, 39, &[0, 11]),
                ExtCallOutcome::Value(28 + padding)
            );
        }
        assert_eq!(call(&mut vm, 39, &[0, 9]), ExtCallOutcome::Value(1));
        assert_eq!(call(&mut vm, 39, &[0, 12]), ExtCallOutcome::Value(0));
        assert_eq!(call(&mut vm, 39, &[0, 18]), ExtCallOutcome::Value(0));
        assert_eq!(call(&mut vm, 39, &[-1, 0]), ExtCallOutcome::Value(0));
        assert_eq!(vm.dispatch_tips_ext(38), ExtCallOutcome::Block);
    }

    #[test]
    fn tips_context_queries_follow_text_init_without_font_or_projection_scaling() {
        let empty_asset = |name: &str| LoadedAsset {
            name: name.into(),
            bytes: Vec::new(),
            source: AssetSource::Loose {
                path: PathBuf::from(name),
            },
        };
        let assets = CoreAssets {
            script: empty_asset("Script.src"),
            file_dat: empty_asset("File.dat"),
            text_dat: empty_asset("Text.dat"),
            mem_dat: empty_asset("Mem.dat"),
            point_dat: empty_asset("Point.dat"),
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
        };
        let mut vm = runtime();
        // Native pop order: size, body x/y, width/height, name x/y, pitch.
        // Distinct values also catch accidental source/pop-order reversal.
        for (logical_size, args) in [
            ((1280, 720), [28, 293, 512, 700, 150, 291, 471, 10]),
            ((1920, 1080), [42, 441, 768, 1092, 225, 438, 707, 15]),
            // The context stores all 32 bits even though PalFontSetFontSize
            // takes only a u16; negative coordinates also remain signed.
            ((960, 540), [0x1_002A, -21, -32, 701, 153, -43, -54, -3]),
        ] {
            vm.system_state
                .set_logical_size(logical_size.0, logical_size.1);
            vm.stack.push(0x123456);
            vm.stack.extend(args.iter().rev());
            assert_eq!(
                vm.dispatch_text_stub(0, &assets, Nls::ShiftJis, None, None),
                ExtCallOutcome::Value(1)
            );
            assert_eq!(vm.stack.pop(), Some(0x123456));
            assert!(vm.stack.is_empty());
            // Menu/UI changes to the shared font and the active ADV mode
            // must not change text_init's stored context values.
            vm.font_state.set_font_size(9);
            vm.text_state.mode = 99;
            let expected = [args[0], args[1], args[2], args[5], args[6], args[7]];
            for (selector, value) in (12..=17).zip(expected) {
                for record in [0, 31, -1, i32::MAX] {
                    assert_eq!(
                        call(&mut vm, 39, &[record, selector]),
                        ExtCallOutcome::Value(value)
                    );
                }
            }
        }
        assert_eq!(
            vm.dispatch_text_stub(6, &assets, Nls::ShiftJis, None, None),
            ExtCallOutcome::Value(1)
        );
        for selector in 12..=17 {
            assert_eq!(call(&mut vm, 39, &[0, selector]), ExtCallOutcome::Value(0));
        }
    }
}

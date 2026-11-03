use super::{bitmap, raster, PalBitmapGlyphRecord32, PalFontProfile, PalFontSystem};
use ab_glyph::{Font, PxScale, ScaleFont};
use pal_asset::Nls;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PalDrawTextRect32 {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub glyph_width: u32,
}

const _: () = {
    assert!(std::mem::size_of::<PalDrawTextRect32>() == 20);
    assert!(std::mem::align_of::<PalDrawTextRect32>() == 4);
    assert!(std::mem::offset_of!(PalDrawTextRect32, glyph_width) == 16);
};

#[derive(Clone, Debug)]
pub(super) struct ExtendedFont {
    width: u32,
    height: u32,
    coverage: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Glyph(char),
    Space(bool),
    Break,
    Extended(i32),
    Ignored,
}

impl PalFontSystem {
    /// Native PAL success is a 32-bit integer; convenience APIs may use bool.
    pub fn native_begin(&mut self) -> i32 {
        i32::from(self.begin())
    }

    pub fn native_set_type(&mut self, font_type: i32) -> i32 {
        i32::from(self.set_type_word(font_type))
    }

    pub(super) fn has_native_markup(&self, text: &str) -> bool {
        self.profile != PalFontProfile::LocalizedReference
            && (text.contains(['<', '%'])
                || (self.profile == PalFontProfile::Totsulover && text.contains('\\')))
    }

    /// UTF-8 slices that must remain intact when the ADV adapter wraps text or
    /// measures reveal prefixes. The bool excludes nonprinting native tags
    /// from the reveal count. Parsing/validation still belongs to `tokens`.
    pub(crate) fn layout_units<'a>(&self, text: &'a str) -> Vec<(&'a str, bool)> {
        let markup = self.has_native_markup(text);
        let newer = self.profile == PalFontProfile::Totsulover;
        let mut units = Vec::new();
        let mut chars = text.char_indices().peekable();
        while let Some((start, ch)) = chars.next() {
            let mut visible = true;
            if markup {
                match ch {
                    '\\' if newer => { chars.next(); }
                    '<' => {
                        visible = false;
                        for (_, next) in chars.by_ref() {
                            if next == '>' { break; }
                        }
                    }
                    '%' if newer => {
                        while chars.peek().is_some_and(|(_, ch)| ch.is_ascii_digit()) {
                            chars.next();
                        }
                    }
                    '%' => { chars.next(); }
                    _ => {}
                }
            }
            let end = chars.peek().map_or(text.len(), |(offset, _)| *offset);
            units.push((&text[start..end], visible));
        }
        units
    }

    pub(super) fn measure_markup(&self, text: &str) -> Result<(u32, u32), &'static str> {
        let encoded = self
            .font_nls
            .encode(text)
            .map_err(|_| "text cannot be encoded for the native font parser")?;
        let mut context = self.clone();
        let (width, height) = context.native_measure_encoded(&encoded, self.font_nls)?;
        let padding = if self.profile == PalFontProfile::Totsulover {
            3
        } else {
            2
        };
        Ok((
            width.saturating_sub(padding).max(0) as u32,
            height.saturating_sub(2).max(0) as u32,
        ))
    }

    pub(super) fn rasterize_markup(&self, text: &str) -> Result<(u32, u32, Vec<u8>), &'static str> {
        let encoded = self
            .font_nls
            .encode(text)
            .map_err(|_| "text cannot be encoded for the native font parser")?;
        let mut context = self.clone();
        let (width, height) = context.native_measure_encoded(&encoded, self.font_nls)?;
        let width_u32 = u32::try_from(width).map_err(|_| "native text width wrapped negative")?;
        let height_u32 =
            u32::try_from(height).map_err(|_| "native text height wrapped negative")?;
        let length = (width_u32 as usize)
            .checked_mul(height_u32 as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("text surface size overflow")?;
        if length > 256 * 1024 * 1024 {
            return Err("text surface exceeds the allocation limit");
        }
        let mut pixels = vec![0; length];
        context.native_draw_encoded(&mut pixels, &encoded, 0, 0, width, height, self.font_nls)?;
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok((width_u32, height_u32, pixels))
    }

    /// Load the native eight-bit strip layout, independent of the general TGA
    /// decoder. Native row reversal ignores the TGA orientation bit. Invalid
    /// extents return an error instead of reproducing unchecked native reads.
    pub fn load_extended_font(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        if bytes.len() < 44 {
            return Err("extended font has no complete native header/footer");
        }
        let width = u32::from(u16::from_le_bytes(bytes[12..14].try_into().unwrap()));
        let height = u32::from(u16::from_le_bytes(bytes[14..16].try_into().unwrap()));
        if width == 0 || height == 0 {
            return Err("extended font has zero dimensions");
        }
        let length = (width as usize)
            .checked_mul(height as usize)
            .ok_or("extended font size overflow")?;
        if length > bytes.len() - 44 || length > 16 * 1024 * 1024 {
            return Err("extended font pixels exceed the native allocation extent");
        }
        let mut coverage = vec![0; length];
        for y in 0..height as usize {
            let source = 18 + (height as usize - 1 - y) * width as usize;
            for x in 0..width as usize {
                coverage[y * width as usize + x] = ((u16::from(bytes[source + x]) + 1) >> 2) as u8;
            }
        }
        self.extended = Some(ExtendedFont {
            width,
            height,
            coverage: coverage.into(),
        });
        self.ex_font_loaded = true;
        Ok(())
    }

    /// The native getter returns AL only; this adapter keeps its reviewed width.
    pub fn native_has_draw_text_rect(&self) -> u8 {
        u8::from(self.has_draw_rect)
    }

    /// Copy the native 20-byte last-glyph state, even when its validity flag is
    /// clear. PalFontGetDrawTextRect has no established source return value.
    pub fn native_draw_text_rect(&self) -> PalDrawTextRect32 {
        self.last_draw_rect
    }

    pub fn native_draw_text_step_x(effect: u16) -> i32 {
        match effect {
            0..=2 => 1,
            3 => 2,
            _ => 0,
        }
    }

    /// Checked PalFontGetSize adapter for the profile's encoded input. The
    /// original returns 1 on normal completion; Result distinguishes portable
    /// invalid-buffer errors from that native successful return contract.
    pub fn native_measure_encoded(
        &mut self,
        text: &[u8],
        nls: Nls,
    ) -> Result<(i32, i32), &'static str> {
        let implicit_begin = !self.begun;
        if implicit_begin {
            self.begin();
        }
        let result = self.measure_encoded_inner(text, nls);
        if implicit_begin {
            self.end();
        }
        result
    }

    fn measure_encoded_inner(&self, text: &[u8], nls: Nls) -> Result<(i32, i32), &'static str> {
        let cell_height = self.native_cell_height();
        let mut width = 0_i32;
        let mut x = 0_i32;
        let mut height = cell_height;
        for token in tokens(text, nls, self.profile, false)? {
            match token {
                Token::Break => {
                    width = width.max(x);
                    x = 0;
                    height = height.wrapping_add(cell_height);
                }
                Token::Space(full) => {
                    x = x.wrapping_add(if full { cell_height } else { cell_height >> 1 })
                }
                Token::Extended(_) => {
                    x = x.wrapping_add(self.extended_size()? as i32).wrapping_add(2)
                }
                Token::Glyph(ch) => {
                    x = x.wrapping_add(i32::from(self.native_glyph(ch, false)?.metrics.cell_inc_x))
                }
                Token::Ignored => {}
            }
        }
        Ok((
            width
                .max(x)
                .wrapping_add(if self.profile == PalFontProfile::Totsulover {
                    3
                } else {
                    2
                }),
            height.wrapping_add(2),
        ))
    }

    /// Checked native BGRA surface adapter. The caller owns the buffer; extra
    /// bytes after width*height pixels remain unchanged. Reference DrawText
    /// consumes one UTF-16 code unit per call, matching its Game.exe loop.
    #[allow(clippy::too_many_arguments)]
    pub fn native_draw_encoded(
        &mut self,
        pixels_bgra: &mut [u8],
        text: &[u8],
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        nls: Nls,
    ) -> Result<i32, &'static str> {
        let implicit_begin = !self.begun;
        if implicit_begin {
            self.begin();
        }
        if self.profile == PalFontProfile::Totsulover {
            self.has_draw_rect = false;
        }
        let result = self.draw_encoded_inner(pixels_bgra, text, x, y, width, height, nls);
        if implicit_begin {
            self.end();
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_encoded_inner(
        &mut self,
        pixels_bgra: &mut [u8],
        text: &[u8],
        start_x: i32,
        start_y: i32,
        width: i32,
        height: i32,
        nls: Nls,
    ) -> Result<i32, &'static str> {
        let width = u32::try_from(width).map_err(|_| "negative text surface width")?;
        let height = u32::try_from(height).map_err(|_| "negative text surface height")?;
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("text surface size overflow")?;
        if length > pixels_bgra.len() || length > 256 * 1024 * 1024 {
            return Err("text surface exceeds its writable buffer");
        }
        let mut pixels = pixels_bgra[..length].to_vec();
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let cell_height = self.native_cell_height();
        let mut x = start_x;
        let mut y = start_y;
        for token in tokens(text, nls, self.profile, true)? {
            match token {
                Token::Break => {
                    x = start_x;
                    y = y.wrapping_add(cell_height);
                }
                Token::Space(full) => {
                    let advance = if full { cell_height } else { cell_height >> 1 };
                    self.record_draw_rect(x, y, advance, cell_height, advance as u32);
                    x = x.wrapping_add(advance);
                }
                Token::Ignored => {}
                Token::Extended(index) => {
                    let size = self.extended_size()?;
                    let coverage = self.extended_glyph(index)?;
                    raster::draw_glyph(
                        &mut pixels,
                        width,
                        height,
                        &coverage,
                        size,
                        size,
                        x,
                        y,
                        self.color,
                        self.effect_color,
                        self.effect,
                        self.profile,
                    );
                    let advance = if self.profile == PalFontProfile::Totsulover {
                        cell_height
                    } else {
                        size as i32
                    };
                    self.record_draw_rect(x, y, advance, cell_height, advance as u32);
                    x = x.wrapping_add(advance);
                }
                Token::Glyph(ch) => {
                    let glyph = self.native_glyph(ch, true)?;
                    if glyph.coverage.is_empty() {
                        continue;
                    }
                    let baseline = self.native_baseline();
                    let top = y
                        .wrapping_add(baseline)
                        .wrapping_sub(glyph.metrics.origin_y);
                    let top = if self.profile == PalFontProfile::LocalizedReference {
                        (f64::from(top) - f64::from(self.layout_font_size()) / 3.5) as i32
                    } else {
                        top
                    };
                    raster::draw_glyph(
                        &mut pixels,
                        width,
                        height,
                        &glyph.coverage,
                        glyph.stride,
                        glyph.metrics.black_box_height,
                        x.wrapping_add(glyph.metrics.origin_x),
                        top,
                        self.color,
                        self.effect_color,
                        self.effect,
                        self.profile,
                    );
                    let advance = i32::from(glyph.metrics.cell_inc_x);
                    let rect_width = if advance > cell_height / 2 + 1 {
                        cell_height
                    } else {
                        advance
                    };
                    self.record_draw_rect(
                        x,
                        y,
                        rect_width,
                        cell_height,
                        glyph.metrics.black_box_width,
                    );
                    x = x.wrapping_add(advance);
                }
            }
        }
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        pixels_bgra[..length].copy_from_slice(&pixels);
        Ok(1)
    }

    fn record_draw_rect(&mut self, x: i32, y: i32, width: i32, height: i32, glyph_width: u32) {
        if self.profile != PalFontProfile::Totsulover {
            return;
        }
        let step = Self::native_draw_text_step_x(self.effect);
        if step == 0 {
            return;
        }
        self.last_draw_rect = PalDrawTextRect32 {
            left: x.wrapping_add(step),
            top: y,
            right: x.wrapping_add(step).wrapping_add(width),
            bottom: y.wrapping_add(height),
            glyph_width,
        };
        self.has_draw_rect = true;
    }

    fn native_cell_height(&self) -> i32 {
        self.bitmap("")
            .map_or(i32::from(self.layout_font_size()), |font| {
                font.height(self.layout_font_size())
            })
    }

    fn fallback_scale(&self) -> f32 {
        let size = f32::from(self.layout_font_size());
        if self.profile == PalFontProfile::LocalizedReference {
            (size * 1.5).trunc()
        } else {
            size
        }
    }

    fn native_baseline(&self) -> i32 {
        self.bitmap("").map_or_else(
            || {
                self.fallback.font.as_ref().map_or(0, |font| {
                    font.as_scaled(PxScale::from(self.fallback_scale())).ascent() as i32
                })
            },
            |font| font.baseline(self.layout_font_size()),
        )
    }

    fn native_glyph(&self, ch: char, draw: bool) -> Result<bitmap::BitmapGlyph, &'static str> {
        let size = self.layout_font_size();
        if let Some(font) = self.bitmap(&ch.to_string()) {
            if draw {
                if let Some(glyph) = font.glyph(ch, size, self.profile)? {
                    return Ok(glyph);
                }
            } else if let Some(metrics) = font.glyph_metrics(ch, size, self.profile)? {
                return Ok(bitmap::BitmapGlyph {
                    metrics,
                    stride: 0,
                    coverage: Vec::new(),
                });
            }
        }
        let scale = PxScale::from(self.fallback_scale());
        let Some(face) = self.fallback.font.as_ref() else {
            return Ok(bitmap::BitmapGlyph {
                metrics: PalBitmapGlyphRecord32::default(),
                stride: 0,
                coverage: Vec::new(),
            });
        };
        let id = face.glyph_id(ch);
        let advance = face.as_scaled(scale).h_advance(id).trunc() as i32 as i16;
        let mut metrics = PalBitmapGlyphRecord32 {
            cell_inc_x: advance,
            ..Default::default()
        };
        let Some(outline) = face.outline_glyph(id.with_scale(scale)) else {
            return Ok(bitmap::BitmapGlyph {
                metrics,
                stride: 0,
                coverage: Vec::new(),
            });
        };
        let bounds = outline.px_bounds();
        metrics.black_box_width = (bounds.max.x - bounds.min.x) as u32;
        metrics.black_box_height = (bounds.max.y - bounds.min.y) as u32;
        metrics.origin_x = bounds.min.x as i32;
        metrics.origin_y = -bounds.min.y as i32;
        let stride = metrics
            .black_box_width
            .checked_add(3)
            .ok_or("fallback font width overflow")?
            & !3;
        metrics.bitmap_bytes = stride
            .checked_mul(metrics.black_box_height)
            .ok_or("fallback font size overflow")?;
        if metrics.bitmap_bytes > 16 * 1024 * 1024 {
            return Err("fallback font exceeds the coverage limit");
        }
        let mut coverage = if draw {
            vec![0; metrics.bitmap_bytes as usize]
        } else {
            Vec::new()
        };
        if draw {
            outline.draw(|x, y, value| {
                coverage[(y * stride + x) as usize] = (value * 64.0).round().clamp(0.0, 64.0) as u8;
            });
        }
        Ok(bitmap::BitmapGlyph {
            metrics,
            stride,
            coverage,
        })
    }

    fn extended_size(&self) -> Result<u32, &'static str> {
        let strip = self
            .extended
            .as_ref()
            .ok_or("extended glyph requested without a loaded strip")?;
        Ok((f32::from(self.layout_font_size()) / strip.height as f32 * strip.height as f32) as u32)
    }

    fn extended_glyph(&self, index: i32) -> Result<Vec<u8>, &'static str> {
        let strip = self
            .extended
            .as_ref()
            .ok_or("extended glyph requested without a loaded strip")?;
        let size = self.extended_size()?;
        let count = strip.width / strip.height;
        let invalid = index < 0 || index as u32 >= count;
        if invalid && self.profile != PalFontProfile::Totsulover {
            return Err("native extended glyph crop would exceed its source");
        }
        let length = (strip.height as usize)
            .checked_mul(strip.height as usize)
            .ok_or("extended glyph size overflow")?;
        if length > 16 * 1024 * 1024 {
            return Err("extended glyph exceeds the coverage limit");
        }
        let mut source = vec![64; length];
        if !invalid {
            for y in 0..strip.height {
                let start = (y * strip.width + index as u32 * strip.height) as usize;
                source
                    [y as usize * strip.height as usize..(y as usize + 1) * strip.height as usize]
                    .copy_from_slice(&strip.coverage[start..start + strip.height as usize]);
            }
        }
        bitmap::resample_coverage(&source, strip.height, strip.height, size, size)
    }
}

fn tokens(
    text: &[u8],
    nls: Nls,
    profile: PalFontProfile,
    drawing: bool,
) -> Result<Vec<Token>, &'static str> {
    if profile == PalFontProfile::LocalizedReference {
        return reference_tokens(text, drawing);
    }
    let mut result = Vec::new();
    let mut position = 0;
    while position < text.len() && text[position] != 0 {
        let (ch, length) = encoded_char(text, position, nls)?;
        if profile == PalFontProfile::Totsulover && ch == '\\' {
            position += length;
            if position >= text.len() || text[position] == 0 {
                return Err("unterminated native font escape");
            }
            let (quoted, length) = encoded_char(text, position, nls)?;
            result.push(Token::Glyph(quoted));
            position += length;
            continue;
        }
        match ch {
            '\r' | '\n' => result.push(Token::Break),
            ' ' => result.push(Token::Space(false)),
            '\u{3000}' => result.push(Token::Space(true)),
            '<' => {
                let start = position + 1;
                let end = text[start..]
                    .iter()
                    .position(|&byte| byte == b'>' || byte == 0)
                    .map(|offset| start + offset)
                    .ok_or("unterminated native font tag")?;
                if text[end] != b'>' {
                    return Err("unterminated native font tag");
                }
                let tag_capacity = if profile == PalFontProfile::Totsulover {
                    267
                } else {
                    259
                };
                if end - start > tag_capacity {
                    return Err("native font tag exceeds its scratch buffer");
                }
                result.push(if matches!(&text[start..end], b"br" | b"BR") {
                    Token::Break
                } else {
                    Token::Ignored
                });
                position = end + 1;
                continue;
            }
            '%' => {
                position += 1;
                let index = if profile == PalFontProfile::Totsulover {
                    let mut index = 0_i32;
                    while position < text.len() && text[position].is_ascii_digit() {
                        index = index
                            .wrapping_mul(10)
                            .wrapping_add(i32::from(text[position] - b'0'));
                        position += 1;
                    }
                    index
                } else {
                    let byte = *text
                        .get(position)
                        .filter(|&&byte| byte != 0)
                        .ok_or("unterminated native extended glyph escape")?;
                    let (_, length) = encoded_char(text, position, nls)?;
                    position += length;
                    i32::from(byte) - 48
                };
                result.push(Token::Extended(index));
                continue;
            }
            _ => result.push(Token::Glyph(ch)),
        }
        position += length;
    }
    Ok(result)
}

fn encoded_char(text: &[u8], position: usize, nls: Nls) -> Result<(char, usize), &'static str> {
    let first = text[position];
    let length = if (0x81..=0x9f).contains(&first) || (0xe0..=0xfc).contains(&first) {
        2
    } else {
        1
    };
    let bytes = text
        .get(position..position + length)
        .ok_or("truncated native Shift-JIS character")?;
    let decoded = nls
        .decode(bytes)
        .map_err(|_| "native font text is invalid in the configured code page")?;
    let mut chars = decoded.chars();
    let ch = chars
        .next()
        .ok_or("native font text contains an empty character")?;
    if chars.next().is_some() {
        return Err("native font code page does not match the byte parser");
    }
    Ok((ch, length))
}

fn reference_tokens(text: &[u8], drawing: bool) -> Result<Vec<Token>, &'static str> {
    let mut result = Vec::new();
    let mut position = 0;
    while position < text.len() {
        let bytes = text
            .get(position..position + 2)
            .ok_or("truncated native UTF-16 font character")?;
        let code = u16::from_le_bytes(bytes.try_into().unwrap());
        if code == 0 || (!drawing && code == 0xcc00) {
            break;
        }
        let ch = char::from_u32(u32::from(code))
            .ok_or("native UTF-16 font input contains an isolated surrogate")?;
        match ch {
            '\r' | '\n' => result.push(Token::Break),
            ' ' => result.push(Token::Space(false)),
            '\u{3000}' => result.push(Token::Space(true)),
            '<' if drawing => result.push(Token::Ignored),
            '<' => {
                // The localized binary retains an ANSI tag scanner at byte+1,
                // even though the surrounding traversal advances UTF-16 units.
                let start = position + 1;
                let end = text[start..]
                    .iter()
                    .position(|&b| b == b'>' || b == 0)
                    .map(|offset| start + offset)
                    .ok_or("unterminated localized font tag")?;
                if end - start > 267 {
                    return Err("localized font tag exceeds its scratch buffer");
                }
                result.push(if matches!(&text[start..end], b"br" | b"BR") {
                    Token::Break
                } else {
                    Token::Ignored
                });
                position = position
                    .checked_add(end - start + 3)
                    .ok_or("localized font cursor overflow")?;
                continue;
            }
            '%' => {
                // DrawText uses the high byte of '%' as its index byte. GetSize
                // instead skips the following UTF-16 unit. Keep both quirks.
                result.push(Token::Extended(i32::from(bytes[1]) - 48));
                position += 2;
            }
            _ => result.push(Token::Glyph(ch)),
        }
        position += 2;
        if drawing {
            break;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Vec<u8> {
        let mut bytes = vec![0; 18];
        bytes[2] = 3;
        bytes[12..14].copy_from_slice(&2u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&2u16.to_le_bytes());
        bytes[16] = 8;
        // Native ignores this top-origin flag and reverses the rows anyway.
        bytes[17] = 0x20;
        bytes.extend_from_slice(&[255, 0, 0, 127]);
        bytes.extend_from_slice(&[0; 26]);
        bytes
    }

    fn bitmap_font(profile: PalFontProfile) -> PalFontSystem {
        let mut font = PalFontSystem::new();
        font.set_profile(profile);
        font.load_bitmap_font(super::super::tests::bitmap_font_fixture(), Nls::ShiftJis)
            .unwrap();
        assert!(font.set_type(4));
        font.set_color(0x00ff_0000, 0x0000_00ff);
        font
    }

    #[test]
    fn native_measure_handles_independent_line_breaks_and_case_exact_tags() {
        let mut font = bitmap_font(PalFontProfile::Koikake);
        assert_eq!(
            font.native_measure_encoded(b"A A<BR>A\0", Nls::ShiftJis)
                .unwrap(),
            (24, 58)
        );
        assert_eq!(
            font.native_measure_encoded(b"A\r\nA", Nls::ShiftJis)
                .unwrap(),
            (6, 86)
        );
        assert_eq!(
            font.native_measure_encoded(b"A<Br>A", Nls::ShiftJis)
                .unwrap(),
            (10, 30)
        );
        assert!(!font.begun);
        font.set_profile(PalFontProfile::Totsulover);
        assert_eq!(
            font.native_measure_encoded(b"A A<br>A", Nls::ShiftJis)
                .unwrap(),
            (25, 58)
        );
        assert_eq!(
            tokens(
                b"\\%12%0012%X",
                Nls::ShiftJis,
                PalFontProfile::Totsulover,
                true
            )
            .unwrap(),
            [
                Token::Glyph('%'),
                Token::Glyph('1'),
                Token::Glyph('2'),
                Token::Extended(12),
                Token::Extended(0),
                Token::Glyph('X')
            ]
        );
    }

    #[test]
    fn extended_strip_reverses_rows_converts_coverage_and_preserves_old_on_error() {
        let mut font = PalFontSystem::new();
        font.load_extended_font(&strip()).unwrap();
        font.set_font_size(2);
        assert_eq!(font.extended_glyph(0).unwrap(), [0, 32, 64, 0]);
        assert!(font.load_extended_font(&[0; 44]).is_err());
        assert_eq!(font.extended_glyph(0).unwrap(), [0, 32, 64, 0]);
        font.set_ex_font_loaded(false);
        assert!(font.extended_glyph(0).is_err());
    }

    #[test]
    fn extended_indices_keep_old_single_byte_and_new_decimal_behavior() {
        assert_eq!(
            tokens(b"%12", Nls::ShiftJis, PalFontProfile::Koikake, true).unwrap(),
            [Token::Extended(1), Token::Glyph('2')]
        );
        assert_eq!(
            tokens(b"%12", Nls::ShiftJis, PalFontProfile::Totsulover, true).unwrap(),
            [Token::Extended(12)]
        );
        let mut font = PalFontSystem::new();
        font.load_extended_font(&strip()).unwrap();
        font.set_font_size(2);
        font.set_color(0x00ff0000, 0);
        assert!(font.extended_glyph(1).is_err());
        font.set_profile(PalFontProfile::Totsulover);
        assert_eq!(font.extended_glyph(-1).unwrap(), [64; 4]);
        assert_eq!(
            font.native_measure_encoded(b"%999", Nls::ShiftJis).unwrap(),
            (7, 4)
        );
        let mut pixels = vec![0; 7 * 4 * 4];
        assert_eq!(
            font.native_draw_encoded(&mut pixels, b"%999", 0, 0, 7, 4, Nls::ShiftJis)
                .unwrap(),
            1
        );
        assert_eq!(&pixels[(7 + 1) * 4..][..4], [0, 0, 255, 255]);
        assert_eq!(
            font.native_draw_text_rect(),
            PalDrawTextRect32 {
                left: 1,
                top: 0,
                right: 3,
                bottom: 2,
                glyph_width: 2
            }
        );
    }

    #[test]
    fn draw_rect_tracks_last_glyph_or_space_and_keeps_stale_bytes_when_invalid() {
        let mut font = bitmap_font(PalFontProfile::Totsulover);
        let mut pixels = vec![0; 40 * 40 * 4];
        font.native_draw_encoded(&mut pixels, b"AA", 3, 4, 40, 40, Nls::ShiftJis)
            .unwrap();
        assert_eq!(font.native_has_draw_text_rect(), 1);
        assert_eq!(
            font.native_draw_text_rect(),
            PalDrawTextRect32 {
                left: 8,
                top: 4,
                right: 12,
                bottom: 32,
                glyph_width: 2
            }
        );
        font.native_draw_encoded(&mut pixels, b" ", 0, 0, 40, 40, Nls::ShiftJis)
            .unwrap();
        let space = PalDrawTextRect32 {
            left: 1,
            top: 0,
            right: 15,
            bottom: 28,
            glyph_width: 14,
        };
        assert_eq!(font.native_draw_text_rect(), space);
        font.native_draw_encoded(&mut pixels, b"", 0, 0, 40, 40, Nls::ShiftJis)
            .unwrap();
        assert_eq!(font.native_has_draw_text_rect(), 0);
        assert_eq!(font.native_draw_text_rect(), space);
        assert_eq!(PalFontSystem::native_draw_text_step_x(3), 2);
        assert_eq!(PalFontSystem::native_draw_text_step_x(4), 0);
    }

    #[test]
    fn reference_draws_one_utf16_unit_and_preserves_its_percent_quirk() {
        let mut font = PalFontSystem::new();
        font.set_profile(PalFontProfile::LocalizedReference);
        let mut first = vec![0; 80 * 80 * 4];
        let mut pair = first.clone();
        font.native_draw_encoded(&mut first, &[b'A', 0, 0, 0], 0, 0, 80, 80, Nls::ShiftJis)
            .unwrap();
        font.native_draw_encoded(
            &mut pair,
            &[b'A', 0, b'B', 0, 0, 0],
            0,
            0,
            80,
            80,
            Nls::ShiftJis,
        )
        .unwrap();
        assert_eq!(first, pair);
        assert!(first.iter().any(|&byte| byte != 0));
        assert_eq!(
            reference_tokens(&[b'%', 0, b'1', 0], true).unwrap(),
            [Token::Extended(-48)]
        );
        assert_eq!(
            reference_tokens(&[b'%', 0, b'1', 0, b'A', 0], false).unwrap(),
            [Token::Extended(-48), Token::Glyph('A')]
        );
    }

    #[test]
    fn checked_native_adapters_reject_bad_extents_and_unterminated_markup() {
        let mut font = bitmap_font(PalFontProfile::Totsulover);
        assert!(font.native_measure_encoded(b"<br", Nls::ShiftJis).is_err());
        assert!(font.native_measure_encoded(b"\\", Nls::ShiftJis).is_err());
        assert!(font
            .native_draw_encoded(&mut [0; 4], b"A", 0, 0, 10, 10, Nls::ShiftJis)
            .is_err());
        let mut pixels = [0; 16];
        font.native_draw_encoded(&mut pixels, b"A", -100, -100, 2, 2, Nls::ShiftJis)
            .unwrap();
        assert_eq!(pixels, [0; 16]);
    }

    #[test]
    fn portable_text_path_uses_native_markup_and_extended_glyphs() {
        let mut font = bitmap_font(PalFontProfile::Totsulover);
        font.load_extended_font(&strip()).unwrap();
        font.set_font_size(2);
        let (width, height, pixels) = font.try_rasterize("%0<br>%9").unwrap();
        assert_eq!((width, height), (7, 6));
        assert_eq!(font.measure("%0<br>%9"), (4, 4));
        assert_eq!(
            &pixels[(3 * width as usize + 1) * 4..][..4],
            [255, 0, 0, 255]
        );
    }
}

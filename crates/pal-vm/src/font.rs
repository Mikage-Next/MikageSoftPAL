use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use pal_asset::Nls;
mod bitmap;
mod native;
mod raster;

use bitmap::PalBitmapFont;
pub use native::PalDrawTextRect32;

/// TrueType face registered by an embedding host (the iOS app passes its
/// system font), tried before the desktop system font paths. No face is
/// bundled with the engine.
static HOST_FALLBACK_TTF: std::sync::Mutex<Option<(&'static [u8], u32)>> =
    std::sync::Mutex::new(None);

/// Registers the host's text face, used by every engine created afterwards.
/// Returns false, and keeps the previous face, when `bytes` is not a font.
/// `index` picks the face of a collection (TTC), 0 for a single font.
pub fn set_host_fallback_font(bytes: Vec<u8>, index: u32) -> bool {
    if FontRef::try_from_slice_and_index(&bytes, index).is_err() {
        return false;
    }
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    *HOST_FALLBACK_TTF.lock().unwrap_or_else(|p| p.into_inner()) = Some((leaked, index));
    true
}

/// Release-specific native font semantics. Selection by script metadata is an
/// audited-release heuristic, not a universal PAL version detector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PalFontProfile {
    #[default]
    Koikake,
    LocalizedReference,
    Totsulover,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PalBitmapGlyphRecord32 {
    pub black_box_width: u32,
    pub black_box_height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub cell_inc_x: i16,
    pub cell_inc_y: i16,
    pub bitmap_bytes: u32,
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct PalBitmapFontMetricsFooter32 {
    pub height: i32,
    pub ascent: i32,
    pub remaining_text_metrics: [u8; 48],
}

const _: () = {
    assert!(std::mem::size_of::<PalBitmapGlyphRecord32>() == 24);
    assert!(std::mem::align_of::<PalBitmapGlyphRecord32>() == 4);
    assert!(std::mem::offset_of!(PalBitmapGlyphRecord32, cell_inc_x) == 16);
    assert!(std::mem::offset_of!(PalBitmapGlyphRecord32, cell_inc_y) == 18);
    assert!(std::mem::offset_of!(PalBitmapGlyphRecord32, bitmap_bytes) == 20);
    assert!(std::mem::size_of::<PalBitmapFontMetricsFooter32>() == 56);
    assert!(std::mem::align_of::<PalBitmapFontMetricsFooter32>() == 4);
};

#[derive(Clone, Debug)]
struct NamedBitmapFont {
    name: String,
    font: PalBitmapFont,
}

#[derive(Clone, Debug)]
pub struct PalFontSystem {
    fallback: PalFontFallback,
    bitmaps: Vec<NamedBitmapFont>,
    active_bitmap: Option<usize>,
    begin_bitmap: Option<usize>,
    begin_size: u16,
    profile: PalFontProfile,
    font_nls: Nls,
    /// Whether the PAL bitmap font may draw body text. A release that ships its
    /// own TrueType faces draws text from those faces, and the bitmap font it
    /// carries alongside is the original one.
    bitmap_text_enabled: bool,
    begun: bool,
    font_size: u16,
    font_type: u16,
    effect: u16,
    color: u32,
    effect_color: u32,
    ex_font_loaded: bool,
    extended: Option<native::ExtendedFont>,
    last_draw_rect: native::PalDrawTextRect32,
    has_draw_rect: bool,
}

impl Default for PalFontSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PalFontSystem {
    pub fn new() -> Self {
        Self {
            fallback: PalFontFallback::default_ttf(),
            bitmaps: Vec::new(),
            active_bitmap: None,
            begin_bitmap: None,
            begin_size: 28,
            profile: PalFontProfile::Koikake,
            font_nls: Nls::ShiftJis,
            bitmap_text_enabled: true,
            begun: false,
            font_size: 28,
            font_type: 1,
            effect: 0,
            color: 0xFF00_0000,
            effect_color: 0xFFFF_FFFF,
            ex_font_loaded: false,
            extended: None,
            last_draw_rect: native::PalDrawTextRect32::default(),
            has_draw_rect: false,
        }
    }

    pub fn set_profile(&mut self, profile: PalFontProfile) {
        if self.profile != profile {
            self.profile = profile;
            self.begun = false;
            self.begin_bitmap = None;
        }
    }

    pub fn profile(&self) -> PalFontProfile {
        self.profile
    }

    pub fn begin(&mut self) -> bool {
        if self.profile == PalFontProfile::LocalizedReference && self.font_type == 4 {
            self.font_type = 1;
        }
        self.begin_bitmap = self.choose_bitmap();
        self.begin_size = self.font_size;
        self.begun = true;
        true
    }

    pub fn end(&mut self) -> bool {
        self.begun = false;
        self.begin_bitmap = None;
        true
    }

    pub fn is_begun(&self) -> bool {
        self.begun
    }

    pub fn set_color(&mut self, color: u32, effect_color: u32) {
        self.color = color;
        self.effect_color = effect_color;
    }

    pub fn color(&self) -> (u32, u32) {
        (self.color, self.effect_color)
    }

    pub fn set_effect(&mut self, effect: u16) {
        self.effect = effect;
    }

    pub fn effect(&self) -> u16 {
        self.effect
    }

    pub fn set_font_size(&mut self, font_size: u16) {
        self.font_size = font_size;
    }

    pub fn font_size(&self) -> u16 {
        self.font_size
    }

    pub fn set_type(&mut self, font_type: u16) -> bool {
        self.set_type_word(i32::from(font_type))
    }

    /// Preserve the native 32-bit comparison before storing its low 16 bits.
    pub fn set_type_word(&mut self, font_type: i32) -> bool {
        if font_type == 4 {
            if self.profile == PalFontProfile::LocalizedReference {
                self.font_type = 1;
                return true;
            }
            if self.active_bitmap.is_none() {
                return false;
            }
        }
        self.font_type = font_type as u16;
        true
    }

    pub fn font_type(&self) -> u16 {
        self.font_type
    }

    pub fn set_ex_font_loaded(&mut self, loaded: bool) {
        self.ex_font_loaded = loaded;
        if !loaded {
            self.extended = None;
        }
    }

    pub fn load_bitmap_font(&mut self, bytes: Vec<u8>, nls: Nls) -> Result<(), &'static str> {
        self.load_named_bitmap_font("", bytes, nls)
    }

    pub fn load_named_bitmap_font(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        nls: Nls,
    ) -> Result<(), &'static str> {
        if self.profile == PalFontProfile::Totsulover
            && self.bitmaps.iter().any(|entry| entry.name == name)
        {
            return Ok(());
        }
        let font = PalBitmapFont::parse(bytes, nls)?;
        self.font_nls = nls;
        if self.profile != PalFontProfile::Totsulover {
            self.bitmaps.clear();
            self.begin_bitmap = None;
        }
        self.bitmaps.push(NamedBitmapFont {
            name: name.to_owned(),
            font,
        });
        self.active_bitmap = Some(self.bitmaps.len() - 1);
        Ok(())
    }

    pub fn unload_bitmap_font(&mut self) {
        self.bitmaps.clear();
        self.active_bitmap = None;
        self.begin_bitmap = None;
    }

    fn choose_bitmap(&self) -> Option<usize> {
        if self.profile != PalFontProfile::Totsulover {
            return self.active_bitmap;
        }
        if let Some(index) = self
            .bitmaps
            .iter()
            .position(|entry| entry.font.metrics.height == i32::from(self.font_size))
        {
            return Some(index);
        }
        let minimum = self
            .bitmaps
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.font.metrics.height)?
            .0;
        if i32::from(self.font_size) < self.bitmaps[minimum].font.metrics.height {
            return Some(minimum);
        }
        // Strict native comparisons keep the first entry on ties.
        self.bitmaps
            .iter()
            .enumerate()
            .max_by_key(|(index, entry)| (entry.font.metrics.height, std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    }

    fn bitmap(&self, text: &str) -> Option<&PalBitmapFont> {
        if self.font_type != 4
            || !self.bitmap_text_enabled
            || self.profile == PalFontProfile::LocalizedReference
        {
            return None;
        }
        let index = if self.begun {
            self.begin_bitmap
        } else {
            self.choose_bitmap()
        }?;
        let font = &self.bitmaps.get(index)?.font;
        font.supports(text).then_some(font)
    }

    /// Replace the fallback face with caller-supplied TrueType bytes.
    ///
    /// Releases that deliver their CJK faces as loose files (`fonts/*.ttf`)
    /// instead of extending `DEFAULT_FONT.DAT` need a host-provided face for
    /// the glyphs the PAL bitmap font and the fallback face (the host's or a system one) do not
    /// cover. Returns whether the bytes parsed; on failure the previous
    /// fallback is kept.
    pub fn load_external_font(&mut self, bytes: Vec<u8>) -> bool {
        match PalFontFallback::from_ttf_bytes(bytes) {
            Some(fallback) => {
                self.fallback = fallback;
                true
            }
            None => false,
        }
    }

    /// Keep the loaded PAL bitmap font out of body text.
    ///
    /// The localized releases carry the original `DEFAULT_FONT.DAT` unchanged
    /// and take their text from the TrueType faces they ship instead, where
    /// `DEF_FONT_TYPE` 4 selects that face rather than the bitmap font. Leaving
    /// the bitmap in play would draw 28px original glyphs at the localized
    /// `FONT_SIZE`, so lines it happens to cover would not match the rest.
    /// `set_type` still accepts 4 either way, matching those releases' own
    /// engine.
    pub fn set_bitmap_text_enabled(&mut self, enabled: bool) {
        self.bitmap_text_enabled = enabled;
    }

    fn layout_font_size(&self) -> u16 {
        if self.begun {
            self.begin_size
        } else {
            self.font_size
        }
    }

    /// Logical layout size, excluding PalFontGetSize's outer padding. The ADV
    /// layout uses this contract; native callers use `native_size` instead.
    pub fn measure(&self, text: &str) -> (u32, u32) {
        if self.has_native_markup(text) {
            match self.measure_markup(text) {
                Ok(size) => return size,
                Err(error) => {
                    log::warn!("PAL text measurement rejected encoded input: {error}");
                    return (0, 0);
                }
            }
        }
        let font_size = self.layout_font_size();
        if let Some(font) = self.bitmap(text) {
            let cell_height = font.height(font_size);
            let mut width = 0_i32;
            let mut line = 0_i32;
            let mut height = cell_height;
            for ch in text.chars() {
                let advance = match ch {
                    '\r' | '\n' => {
                        width = width.max(line);
                        line = 0;
                        height = height.wrapping_add(cell_height);
                        continue;
                    }
                    ' ' => cell_height >> 1,
                    '\u{3000}' => cell_height,
                    _ => font
                        .glyph_metrics(ch, font_size, self.profile)
                        .ok()
                        .flatten()
                        .map_or(0, |glyph| i32::from(glyph.cell_inc_x)),
                };
                line = line.wrapping_add(advance);
            }
            return (width.max(line).max(0) as u32, height.max(0) as u32);
        }
        let size = f32::from(font_size);
        let mut height = u32::from(font_size);
        let mut width = 0;
        for line in text.split(['\r', '\n']) {
            width = width.max(self.fallback.measure_line(line, size).0);
        }
        height = height
            .saturating_mul(text.chars().filter(|ch| matches!(ch, '\r' | '\n')).count() as u32 + 1);
        (width, height)
    }

    /// Native outer padding for already decoded, plain text. Native encoded
    /// percent/tag parsing remains a separate adapter boundary.
    pub fn native_size(&self, text: &str) -> (u32, u32) {
        let (width, height) = self.measure(text);
        (
            width.saturating_add(if self.profile == PalFontProfile::Totsulover {
                3
            } else {
                2
            }),
            height.saturating_add(2),
        )
    }

    pub fn rasterize(&self, text: &str) -> (u32, u32, Vec<u8>) {
        self.try_rasterize(text).unwrap_or_else(|error| {
            log::warn!("PAL text rasterization rejected invalid dimensions: {error}");
            (1, 1, vec![0; 4])
        })
    }

    pub fn try_rasterize(&self, text: &str) -> Result<(u32, u32, Vec<u8>), &'static str> {
        if self.has_native_markup(text) {
            return self.rasterize_markup(text);
        }
        let font_size = self.layout_font_size();
        let (width, height) = self.native_size(text);
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("text surface size overflow")?;
        if length > 256 * 1024 * 1024 {
            return Err("text surface exceeds the allocation limit");
        }
        let mut pixels = vec![0; length];
        if let Some(font) = self.bitmap(text) {
            let cell_height = font.height(font_size);
            let baseline = font.baseline(font_size);
            let mut x = 0_i32;
            let mut y = 0_i32;
            for ch in text.chars() {
                match ch {
                    '\r' | '\n' => {
                        x = 0;
                        y = y.wrapping_add(cell_height);
                        continue;
                    }
                    ' ' => {
                        x = x.wrapping_add(cell_height >> 1);
                        continue;
                    }
                    '\u{3000}' => {
                        x = x.wrapping_add(cell_height);
                        continue;
                    }
                    _ => {}
                }
                let Some(glyph) = font.glyph(ch, font_size, self.profile)? else {
                    continue;
                };
                raster::draw_glyph(
                    &mut pixels,
                    width,
                    height,
                    &glyph.coverage,
                    glyph.stride,
                    glyph.metrics.black_box_height,
                    x.wrapping_add(glyph.metrics.origin_x),
                    y.wrapping_add(baseline)
                        .wrapping_sub(glyph.metrics.origin_y),
                    self.color,
                    self.effect_color,
                    self.effect,
                    self.profile,
                );
                // Native DrawText does not advance for an empty ordinary bitmap.
                if !glyph.coverage.is_empty() {
                    x = x.wrapping_add(i32::from(glyph.metrics.cell_inc_x));
                }
            }
        } else if font_size != 0 {
            let mut y = 0_i32;
            for line in text.split(['\r', '\n']) {
                let (source_width, source_height, source) =
                    self.fallback
                        .rasterize_line(line, f32::from(font_size), [255; 4]);
                // ab_glyph is a portable face substitute, not GDI. Quantize its
                // coverage into PAL's six-bit input before using native kernels.
                let coverage: Vec<_> = source
                    .chunks_exact(4)
                    .map(|pixel| ((u16::from(pixel[3]) + 3) / 4) as u8)
                    .collect();
                raster::draw_glyph(
                    &mut pixels,
                    width,
                    height,
                    &coverage,
                    source_width,
                    source_height,
                    0,
                    y,
                    self.color,
                    self.effect_color,
                    self.effect,
                    self.profile,
                );
                y = y.wrapping_add(i32::from(font_size));
            }
        }
        Ok((width, height, pixels))
    }
}

#[derive(Clone, Debug)]
pub struct PalFontFallback {
    /// `None` when neither the host nor the system has a usable face: text
    /// then measures empty instead of stopping the engine.
    font: Option<FontRef<'static>>,
}

impl PalFontFallback {
    /// The host's registered face, else the first system CJK face found.
    pub fn default_ttf() -> Self {
        let host = *HOST_FALLBACK_TTF.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(font) =
            host.and_then(|(bytes, index)| FontRef::try_from_slice_and_index(bytes, index).ok())
        {
            return Self { font: Some(font) };
        }
        for path in SYSTEM_CJK_FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            if let Ok(font) = FontRef::try_from_slice(leaked) {
                return Self { font: Some(font) };
            }
        }
        log::error!("no text face: register one with the host or pass --font-file");
        Self { font: None }
    }

    /// Build a fallback from caller-supplied TrueType bytes.
    ///
    /// Returns `None` when the face cannot be parsed. The bytes are leaked so
    /// the `FontRef` borrow can be `'static`, matching `default_ttf`.
    pub fn from_ttf_bytes(bytes: Vec<u8>) -> Option<Self> {
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        FontRef::try_from_slice(leaked)
            .ok()
            .map(|font| Self { font: Some(font) })
    }

    /// Measure the pixel width of a single text line at the given pixel height.
    /// Returns `(width_px, height_px)`.
    pub fn measure_line(&self, text: &str, px_height: f32) -> (u32, u32) {
        let Some(font) = self.font.as_ref() else {
            return (0, px_height.ceil() as u32);
        };
        let scale = PxScale::from(px_height);
        let scaled = font.as_scaled(scale);
        let width: f32 = text
            .chars()
            .map(|c| scaled.h_advance(font.glyph_id(c)))
            .sum();
        (width.ceil() as u32, px_height.ceil() as u32)
    }

    /// Rasterize a single line of text into BGRA8 pixels.
    ///
    /// Returns `(width, height, pixels_bgra)`.  If the text is empty or all
    /// glyphs have zero advance, returns a 1×height blank buffer.
    ///
    /// Color is `[B, G, R, A]` matching the PAL surface format.
    pub fn rasterize_line(
        &self,
        text: &str,
        px_height: f32,
        color_bgra: [u8; 4],
    ) -> (u32, u32, Vec<u8>) {
        let (width, height) = self.measure_line(text, px_height);
        let Some(font) = self.font.as_ref() else {
            let h = height.max(1);
            return (1, h, vec![0; h as usize * 4]);
        };
        let scale = PxScale::from(px_height);
        let scaled = font.as_scaled(scale);
        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let mut pixels = vec![0u8; w * h * 4];

        let mut cursor_x = 0.0f32;
        let baseline_y = scaled.ascent();

        for ch in text.chars() {
            let glyph_id = font.glyph_id(ch);
            let glyph =
                glyph_id.with_scale_and_position(scale, ab_glyph::point(cursor_x, baseline_y));
            cursor_x += scaled.h_advance(glyph_id);

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                // Some fallback fonts place shorter glyphs a pixel or two
                // above the bottom of the ideographic cell. PAL text uses a
                // shared cell baseline, so align substantial short glyphs to
                // that baseline while leaving centered marks and low commas
                // at their font-defined positions.
                let y_shift = glyph_baseline_shift(
                    bounds.min.y as i32,
                    bounds.max.y as i32,
                    baseline_y,
                    px_height,
                );
                outlined.draw(|gx, gy, cov| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32 + y_shift;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        return;
                    }
                    let idx = (py as usize * w + px as usize) * 4;
                    let alpha = (cov * color_bgra[3] as f32) as u8;
                    pixels[idx] = color_bgra[0];
                    pixels[idx + 1] = color_bgra[1];
                    pixels[idx + 2] = color_bgra[2];
                    pixels[idx + 3] = alpha;
                });
            }
        }

        (w as u32, h as u32, pixels)
    }
}

fn glyph_baseline_shift(min_y: i32, max_y: i32, ascent: f32, cell_height: f32) -> i32 {
    if (max_y - min_y) as f32 <= cell_height * 0.45 {
        return 0;
    }
    let baseline_bottom = ascent.ceil() as i32 - 1;
    (baseline_bottom - (max_y - 1)).clamp(0, (cell_height * 0.1).ceil() as i32)
}

const SYSTEM_CJK_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial Unicode.ttf",
    "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/msgothic.ttc",
];

fn argb_to_rgba(color: u32) -> [u8; 4] {
    [
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
        ((color >> 24) & 0xFF) as u8,
    ]
}

/// A system CJK face for tests, which no longer have a bundled one
/// (Linux: apt install fonts-noto-cjk).
#[cfg(test)]
pub(crate) fn test_face_bytes() -> Vec<u8> {
    SYSTEM_CJK_FONT_CANDIDATES
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .expect("tests need a system CJK font such as fonts-noto-cjk")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(base: i32, ascent: i32, advance_x: i16, advance_y: i16, coverage: &[u8]) -> Vec<u8> {
        let table_end = 0x108_u32;
        let mut bytes = vec![0; table_end as usize];
        bytes[0x80..0x84].copy_from_slice(&table_end.to_le_bytes());
        bytes[0x104..0x108].copy_from_slice(&table_end.to_le_bytes());
        for value in [2_u32, 2, 0, 2] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&advance_x.to_le_bytes());
        bytes.extend_from_slice(&advance_y.to_le_bytes());
        bytes.extend_from_slice(&(coverage.len() as u32).to_le_bytes());
        bytes.extend_from_slice(coverage);
        bytes.extend_from_slice(&base.to_le_bytes());
        bytes.extend_from_slice(&ascent.to_le_bytes());
        bytes.extend_from_slice(&[0; 48]);
        bytes
    }

    pub(super) fn bitmap_font_fixture() -> Vec<u8> {
        fixture(28, 24, 4, 0, &[64, 0, 0, 0, 0, 32, 0, 0])
    }

    #[test]
    fn short_glyphs_share_the_cell_baseline_without_moving_centered_marks() {
        assert_eq!(glyph_baseline_shift(7, 23, 24.0, 28.0), 1);
        assert_eq!(glyph_baseline_shift(7, 22, 24.0, 28.0), 2);
        assert_eq!(glyph_baseline_shift(12, 16, 24.0, 28.0), 0);
    }

    #[test]
    fn default_face_and_fallback_dimensions_are_valid() {
        let font = PalFontFallback::default_ttf();
        let (width, height, pixels) = font.rasterize_line("Hi", 20.0, [255; 4]);
        assert!(width > 0 && height > 0);
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
        let (width, height, pixels) = font.rasterize_line("", 16.0, [255; 4]);
        assert_eq!(width, 1);
        assert_eq!(pixels.len(), height as usize * 4);
        assert!(pixels.iter().all(|&value| value == 0));
    }

    #[test]
    fn bitmap_footer_controls_baseline_and_coverage_uses_native_alpha_or() {
        let mut font = PalFontSystem::new();
        font.load_bitmap_font(bitmap_font_fixture(), Nls::ShiftJis)
            .unwrap();
        assert!(font.set_type(4));
        font.set_color(0x00ff_0000, 0);
        assert_eq!(font.measure("A"), (4, 28));
        let (width, height, pixels) = font.rasterize("A");
        assert_eq!((width, height), (6, 30));
        let pixel = |x: usize, y: usize| &pixels[(y * width as usize + x) * 4..][..4];
        assert_eq!(pixel(1, 23), [255, 0, 0, 255]);
        assert_eq!(pixel(2, 24), [255, 0, 0, 127]);
        font.set_color(0x80ff_0000, 0);
        let (_, _, pixels) = font.rasterize("A");
        assert_eq!(pixels[(24 * width as usize + 2) * 4 + 3], 255);
    }

    #[test]
    fn cell_advances_are_separate_signed_words() {
        let font = PalBitmapFont::parse(fixture(28, 24, -3, 7, &[64; 8]), Nls::ShiftJis).unwrap();
        let glyph = font
            .glyph('A', 28, PalFontProfile::Koikake)
            .unwrap()
            .unwrap();
        assert_eq!(glyph.metrics.cell_inc_x, -3);
        assert_eq!(glyph.metrics.cell_inc_y, 7);
        assert_eq!(font.baseline(14), 12);
    }

    #[test]
    fn fixed_point_bilinear_uses_zero_neighbors_at_the_edges() {
        let resized = bitmap::resample_coverage(&[64, 0, 0, 64], 2, 2, 4, 4).unwrap();
        assert_eq!(
            resized,
            [64, 32, 0, 0, 32, 32, 32, 16, 0, 32, 64, 32, 0, 16, 32, 16]
        );
    }

    #[test]
    fn profile_padding_changes_resampling_and_minimum_advance() {
        let font = PalBitmapFont::parse(
            fixture(4, 2, 4, 0, &[64, 32, 16, 8, 64, 32, 16, 8]),
            Nls::ShiftJis,
        )
        .unwrap();
        let old = font
            .glyph('A', 3, PalFontProfile::Koikake)
            .unwrap()
            .unwrap();
        let new = font
            .glyph('A', 3, PalFontProfile::Totsulover)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                old.metrics.black_box_width,
                old.stride,
                old.metrics.cell_inc_x
            ),
            (4, 4, 4)
        );
        assert_eq!(
            (
                new.metrics.black_box_width,
                new.stride,
                new.metrics.cell_inc_x
            ),
            (3, 4, 3)
        );
        assert_eq!(old.coverage, [64, 32, 16, 8]);
        assert_eq!(new.coverage, [64, 26, 10, 0]);
        assert_eq!(
            (
                new.metrics.origin_x,
                new.metrics.origin_y,
                new.metrics.cell_inc_y
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn zero_bitmap_scaling_is_profile_specific_and_safely_checked() {
        let font = PalBitmapFont::parse(fixture(28, 24, 9, -2, &[]), Nls::ShiftJis).unwrap();
        assert!(font.glyph('A', 14, PalFontProfile::Koikake).is_err());
        let glyph = font
            .glyph('A', 14, PalFontProfile::Totsulover)
            .unwrap()
            .unwrap();
        assert_eq!(
            (glyph.metrics.cell_inc_x, glyph.metrics.cell_inc_y),
            (9, -2)
        );
        assert!(glyph.coverage.is_empty());
    }

    #[test]
    fn named_cache_uses_exact_names_and_native_size_selection() {
        let mut font = PalFontSystem::new();
        font.set_profile(PalFontProfile::Totsulover);
        for (name, size) in [("small", 16), ("medium", 24), ("large", 48)] {
            font.load_named_bitmap_font(name, fixture(size, 12, 4, 0, &[64; 8]), Nls::ShiftJis)
                .unwrap();
        }
        font.load_named_bitmap_font("small", vec![], Nls::ShiftJis)
            .unwrap();
        assert_eq!(font.active_bitmap, Some(2));
        assert_eq!(font.bitmaps.len(), 3);
        for (size, selected) in [(12, 0), (16, 0), (24, 1), (25, 2), (30, 2), (48, 2)] {
            font.set_font_size(size);
            assert_eq!(font.choose_bitmap(), Some(selected));
        }
        font.load_named_bitmap_font("SMALL", fixture(16, 12, 4, 0, &[64; 8]), Nls::ShiftJis)
            .unwrap();
        font.set_font_size(16);
        assert_eq!(font.choose_bitmap(), Some(0));
        assert_eq!(font.bitmaps.len(), 4);
        assert!(font.set_type(4));
        font.begin();
        font.set_font_size(48);
        assert_eq!(font.begin_bitmap, Some(0));
        assert_eq!(font.measure("A").1, 16);
        font.end();
        assert_eq!(font.choose_bitmap(), Some(2));
    }

    #[test]
    fn type_gate_preserves_full_word_and_extended_sheet_does_not_satisfy_it() {
        let mut font = PalFontSystem::new();
        font.set_ex_font_loaded(true);
        assert!(!font.set_type_word(4));
        assert!(font.set_type_word(0x10004));
        assert_eq!(font.font_type(), 4);
        font.set_profile(PalFontProfile::LocalizedReference);
        assert!(font.set_type_word(4));
        assert_eq!(font.font_type(), 1);
        assert!(font.set_type_word(0x10004));
        font.begin();
        assert_eq!(font.font_type(), 1);
        font.set_font_size(0);
        assert_eq!(font.font_size(), 0);
        font.end();
        assert_eq!(font.native_size("A"), (2, 2));
    }

    #[test]
    fn outline_is_a_cardinal_or_kernel_and_shadow_is_offset() {
        let pixel = |pixels: &[u8], x: usize, y: usize| -> [u8; 4] {
            pixels[(y * 7 + x) * 4..][..4].try_into().unwrap()
        };
        let mut pixels = vec![0; 7 * 7 * 4];
        raster::draw_glyph(
            &mut pixels,
            7,
            7,
            &[32],
            1,
            1,
            2,
            2,
            0x00ff0000,
            0x000000ff,
            1,
            PalFontProfile::Koikake,
        );
        assert_eq!(pixel(&pixels, 3, 3), [255, 0, 0, 127]);
        assert_eq!(pixel(&pixels, 3, 2), [0, 0, 255, 127]);
        assert_eq!(pixel(&pixels, 2, 2), [0; 4]);
        let mut shadow = vec![0; 7 * 7 * 4];
        raster::draw_glyph(
            &mut shadow,
            7,
            7,
            &[64],
            1,
            1,
            2,
            2,
            0x00ff0000,
            0x000000ff,
            2,
            PalFontProfile::Koikake,
        );
        assert_eq!(pixel(&shadow, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&shadow, 4, 4), [0, 0, 255, 255]);
        assert_eq!(pixel(&shadow, 2, 3), [0; 4]);
    }

    #[test]
    fn overlap_blends_with_divisor_256_and_full_coverage_overwrites() {
        let mut pixels = vec![0; 3 * 3 * 4];
        pixels[16..20].copy_from_slice(&[0, 0, 255, 255]);
        raster::draw_glyph(
            &mut pixels,
            3,
            3,
            &[32],
            1,
            1,
            0,
            0,
            0x00ff0000,
            0,
            0,
            PalFontProfile::Koikake,
        );
        assert_eq!(&pixels[16..20], [126, 0, 127, 254]);
        raster::draw_glyph(
            &mut pixels,
            3,
            3,
            &[64],
            1,
            1,
            0,
            0,
            0x0000ff00,
            0,
            0,
            PalFontProfile::Koikake,
        );
        assert_eq!(&pixels[16..20], [0, 255, 0, 255]);
    }

    #[test]
    fn effect_three_is_profile_specific_and_unknown_effects_draw_nothing() {
        for profile in [
            PalFontProfile::Koikake,
            PalFontProfile::Totsulover,
            PalFontProfile::LocalizedReference,
        ] {
            let mut pixels = vec![0; 7 * 7 * 4];
            raster::draw_glyph(
                &mut pixels,
                7,
                7,
                &[64],
                1,
                1,
                1,
                1,
                0x00ff0000,
                0x000000ff,
                3,
                profile,
            );
            assert_eq!(
                pixels.chunks_exact(4).filter(|px| px[3] != 0).count(),
                if profile == PalFontProfile::Koikake {
                    0
                } else {
                    13
                }
            );
            let mut unsupported = vec![0; pixels.len()];
            raster::draw_glyph(
                &mut unsupported,
                7,
                7,
                &[64],
                1,
                1,
                1,
                1,
                0x00ff0000,
                0,
                4,
                profile,
            );
            assert!(unsupported.iter().all(|&value| value == 0));
        }
    }

    #[test]
    fn malformed_fonts_are_rejected_before_any_state_replacement() {
        let mut font = PalFontSystem::new();
        font.load_bitmap_font(bitmap_font_fixture(), Nls::ShiftJis)
            .unwrap();
        assert!(font.set_type(4));
        assert!(font.load_bitmap_font(vec![0; 24], Nls::ShiftJis).is_err());
        assert_eq!(font.measure("A"), (4, 28));
        let mut invalid = bitmap_font_fixture();
        let footer = invalid.len() - 56;
        invalid[footer..footer + 4].fill(0);
        assert!(font.load_bitmap_font(invalid, Nls::ShiftJis).is_err());
        let mut invalid = bitmap_font_fixture();
        invalid[0x108 + 20..0x108 + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(font.load_bitmap_font(invalid, Nls::ShiftJis).is_err());
    }
    #[test]
    #[ignore = "requires PAL_FONT_SMOKE_ROOT pointing to an installed read-only game"]
    fn installed_font_resource_smoke() {
        let root = std::path::PathBuf::from(
            std::env::var_os("PAL_FONT_SMOKE_ROOT").expect("set PAL_FONT_SMOKE_ROOT"),
        );
        let mut resources = pal_asset::ResourceManager::bootstrap(&root, Nls::ShiftJis).unwrap();
        let resource = std::env::var("PAL_FONT_SMOKE_RESOURCE")
            .unwrap_or_else(|_| "default_font.dat".to_owned());
        let bytes = resources.open(&resource).unwrap().bytes;
        let mut font = PalFontSystem::new();
        if std::env::var("PAL_FONT_SMOKE_PROFILE").as_deref() == Ok("totsulover") {
            font.set_profile(PalFontProfile::Totsulover);
        }
        font.load_named_bitmap_font(&resource, bytes, Nls::ShiftJis)
            .unwrap();
        assert!(font.set_type(4));
        font.set_color(0x00ff_ffff, 0x0000_0000);
        font.set_effect(1);
        let sample = "ABC 012 \u{3042}\u{3044}\u{3046}";
        let (width, height, pixels) = font.try_rasterize(sample).unwrap();
        assert!(width > 2 && height > 2);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
        let base = font.bitmaps[0].font.metrics.height;
        let ascent = font.bitmaps[0].font.metrics.ascent;
        eprintln!("font smoke: profile={:?}, resource={resource}, footer height={base}, ascent={ascent}, image={width}x{height}", font.profile());
        if let Ok(sheet) = resources.open("font_ex.tga") {
            font.load_extended_font(&sheet.bytes).unwrap();
            let (_, _, extended) = font.try_rasterize("%0").unwrap();
            assert!(extended.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
        if let Some(output) = std::env::var_os("PAL_FONT_SMOKE_OUTPUT") {
            let output = std::path::PathBuf::from(output);
            assert!(
                !output.starts_with(&root),
                "smoke artifacts must remain outside the game root"
            );
            let mut encoder =
                png::Encoder::new(std::fs::File::create(output).unwrap(), width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixels)
                .unwrap();
        }
    }
}

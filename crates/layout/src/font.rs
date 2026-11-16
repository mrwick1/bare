//! Font metrics: how wide a letter is, and how tall a line of text is.
//!
//! A real browser asks the font. Chrome shapes each run of text with HarfBuzz and keeps the result in a cache (one
//! `ShapeResult` for each item of text). Firefox keeps a cache of shaped words (`gfxShapedWord`), and WebKit a cache of
//! word widths. Shaping is the slow part of layout: it turns letters into glyphs, with kerning (`AV` closer together)
//! and ligatures (`fi` as one glyph).
//!
//! `bare` starts with one monospace font (`docs/decisions.md`, "Fonts"): every letter has the same width. So we need
//! no shaping and no cache. The width of a word is the number of its letters times one width. The numbers here are the
//! numbers of Noto Sans Mono, so a test page in Chrome with that font gives the same layout. The bitmap font of module 5
//! must keep these numbers.
//!
//! Widths are counted in tenths of an em (`Units`), as whole numbers: a letter is 6, a wide letter (Chinese, Japanese,
//! Korean) is 10, an accent on a letter is 0. Adding whole numbers has no rounding error, so a line of 1,000 letters is
//! exactly 1,000 times one letter. Pixels come at the end: `px(units)`.

use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Property, Value};

/// A width in tenths of an em. JS: a number, but always a whole one.
pub type Units = u32;

/// The width of a normal letter: 0.6 em (600 of the 1000 units of the font).
pub const LETTER: Units = 6;
/// The width of a wide letter. Noto Sans Mono has none: the browser takes it from another font, and those are 1 em wide.
pub const WIDE: Units = 10;

/// The biggest font size, in pixels. Chrome has the same limit (`kMaximumAllowedFontSize`).
pub const MAX_SIZE: f32 = 10_000.0;

/// The font numbers for one font size, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub size: f32,
    /// From the baseline up to the top of the tallest letter.
    pub ascent: f32,
    /// From the baseline down to the bottom of `g` and `p`.
    pub descent: f32,
}

impl Metrics {
    /// The metrics for a font size in pixels. Noto Sans Mono: ascent 1069 and descent 293 for 1000 units.
    /// Chrome rounds the ascent and the descent to whole pixels (`SimpleFontData`), so we do too.
    pub fn new(size: f32) -> Metrics {
        let size = size.clamp(0.0, MAX_SIZE);
        Metrics { size, ascent: round(size * 1.069), descent: round(size * 0.293) }
    }

    pub fn of(style: &ComputedStyle) -> Metrics {
        Metrics::new(style.px(Property::FontSize))
    }

    /// Pixels for a width in tenths of an em.
    pub fn px(&self, units: Units) -> f32 {
        units as f32 * self.size / 10.0
    }

    /// Half the x-height (`0.536` em in Noto Sans Mono) as Chrome keeps it: the height in 1/64 px, rounded, and halved in
    /// whole 1/64 px (`vertical-align: middle`).
    pub fn half_x_height(&self) -> f32 {
        ((round(self.size * 0.536 * 64.0) as i32) / 2) as f32 / 64.0
    }

    /// The height of a line for `line-height: normal`: the ascent and the descent (the font has no extra gap).
    pub fn normal_line_height(&self) -> f32 {
        self.ascent + self.descent
    }

    /// The computed `line-height` in pixels: `normal`, a number (times the font size), or a length.
    pub fn line_height(&self, style: &ComputedStyle) -> f32 {
        let px = match style.get(Property::LineHeight) {
            Value::Number(n) => n * self.size,
            Value::Keyword(Keyword::Normal) => self.normal_line_height(),
            _ => style.px(Property::LineHeight),
            // The cascade turns `em` and `%` into pixels.
        };
        px.clamp(0.0, crate::block::MAX_PX)
    }
}

/// `f32::round` for a length that is not negative. Like `floor`, the method is a call into the C library.
pub fn round(x: f32) -> f32 {
    let t = x as i64 as f32;
    if x - t >= 0.5 { t + 1.0 } else { t }
}

/// `f32::floor` for lengths. The plain method is a call into the C library on a build without SSE4.1, and a line calls it often.
pub fn floor(x: f32) -> f32 {
    let t = x as i64 as f32;
    if t > x { t - 1.0 } else { t }
}

/// A length cut down to 1/64 px, as Chrome's `LayoutUnit` does for line heights and `vertical-align`.
pub fn lu(px: f32) -> f32 {
    floor(px * 64.0) / 64.0
}

/// The width of one letter. An accent that sits on the letter before it is 0. Zero-width spaces and joiners are 0.
pub fn width_of(c: char) -> Units {
    let cp = c as u32;
    if cp < 0x300 {
        // Latin, the most common case: one compare. (The soft hyphen U+00AD is only drawn at a line break.)
        return if cp == 0xad { 0 } else { LETTER };
    }
    if is_wide(cp) {
        return WIDE;
        // Before the table of accents: no wide letter is an accent, and Chinese text skips the search.
    }
    if matches!(cp, 0x200b..=0x200f | 0x2060 | 0xfeff) || bare_unicode::find(bare_unicode::tables::MARKS, cp).is_some() {
        return 0;
    }
    LETTER
}

/// Is it a letter of the "wide" set of Unicode (East Asian Width W and F): Chinese, Japanese, Korean, full-width forms
/// and most emoji. Only the big blocks are here: a short list, and no table.
fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115f | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f | 0x1f900..=0x1f9ff | 0x20000..=0x2fffd | 0x30000..=0x3fffd)
}

/// The width of a piece of text with no spaces. Plain ASCII is the fast case: one byte is one letter.
pub fn width(text: &str) -> Units {
    if text.is_ascii() {
        return (text.len() as Units).saturating_mul(LETTER);
    }
    text.chars().fold(0, |sum: Units, c| sum.saturating_add(width_of(c)))
    // `saturating_add`: a sum that is too big stops at the largest `u32`, and does not wrap round to a small number.
}

/// `letter-spacing` and `word-spacing` of a style, in pixels. `spaced`: `Styles::spaced`, false for most pages.
pub fn spacing(spaced: bool, style: &ComputedStyle) -> (f32, f32) {
    if spaced { (style.px(Property::LetterSpacing), style.px(Property::WordSpacing)) } else { (0.0, 0.0) }
}

/// The width that `letter-spacing` and `word-spacing` add to a piece of text: `letter` after each letter (also the last one,
/// as in Chrome), and `word` more after each space. An accent that sits on a letter is no letter of its own.
pub fn extra(text: &str, (letter, word): (f32, f32)) -> f32 {
    let (mut letters, mut spaces) = (0u32, 0u32);
    for c in text.chars() {
        letters += (width_of(c) != 0) as u32;
        spaces += (c == ' ' || c == '\u{a0}') as u32;
    }
    letters as f32 * letter + spaces as f32 * word
}

/// The same for a run of spaces of this width (a run of collapsing spaces is one space).
pub fn extra_spaces(width: Units, (letter, word): (f32, f32)) -> f32 {
    (width / LETTER) as f32 * (letter + word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::tests::rect;

    /// The width of `text` in an inline-block box (shrink-to-fit: the width of its text) at 16 px.
    fn width_of_text(style: &str, text: &str) -> f32 {
        rect(&format!("<span id=x style='display:inline-block;font-size:16px;{style}'>{text}</span>"), "", "x").w
    }

    /// The advances here are from `hmtx` of the font files: W 930, i 258, l 258 in Noto Sans Regular; W 967, i 299, l 298 in Bold.
    /// `the_advance_tables_match_the_font_files` (bare-paint) checks the tables against the files.
    #[test]
    fn sans_serif_text_uses_the_advances_of_noto_sans() {
        let sans = width_of_text("font-family: sans-serif", "Wil");
        assert!((sans - (930 + 258 + 258) as f32 * 16.0 / 1000.0).abs() < 0.001, "{sans}");
        assert!((sans - 3.0 * 0.6 * 16.0).abs() > 1.0, "not three letters of 0.6 em");
        assert_eq!(width_of_text("", "Wil"), sans, "the initial family is sans-serif");
        assert_eq!(width_of_text("font-family: Verdana, Geneva, sans-serif", "Wil"), sans);
    }

    #[test]
    fn monospace_text_keeps_0_6_em_a_letter() {
        for family in ["monospace", "Courier New", "Menlo, sans-serif"] {
            let w = width_of_text(&format!("font-family: {family}"), "Wil");
            assert!((w - 3.0 * 0.6 * 16.0).abs() < 0.001, "{family}: {w}");
        }
        assert!((width_of_text("", "<code>Wil</code>") - 3.0 * 0.6 * 16.0).abs() < 0.001, "the UA sheet: `code` is monospace");
    }

    #[test]
    fn the_first_family_decides() {
        let sans = width_of_text("font-family: sans-serif", "Wil");
        assert_eq!(width_of_text("font-family: \"Unknown\", monospace", "Wil"), sans);
        assert_eq!(width_of_text("font-family: Verdana, monospace", "Wil"), sans);
    }

    #[test]
    fn bold_sans_uses_the_bold_advances() {
        let bold = width_of_text("font-family: sans-serif; font-weight: bold", "Wil");
        assert!((bold - (967 + 299 + 298) as f32 * 16.0 / 1000.0).abs() < 0.001, "{bold}");
        assert_eq!(width_of_text("font-weight: 600", "Wil"), bold, "600 is bold, as in paint");
        assert_eq!(width_of_text("font-weight: 500", "Wil"), width_of_text("", "Wil"), "500 is regular, as in paint");
        let mono = width_of_text("font-family: monospace; font-weight: bold", "Wil");
        assert!((mono - 3.0 * 0.6 * 16.0).abs() < 0.001, "bold mono has the same advance: {mono}");
    }

    #[test]
    fn advances_by_face() {
        assert_eq!(width_of(Face::Sans, 'W'), 930);
        assert_eq!(width_of(Face::SansBold, 'W'), 967);
        assert_eq!(width_of(Face::Mono, 'W'), LETTER);
        assert_eq!(width_of(Face::Sans, ' '), 260);
        assert_eq!(width(Face::Sans, "Wil"), 1446);
        assert_eq!(width_of(Face::Sans, '\u{e9}'), 564, "a letter of Latin-1 is in the table");
        assert_eq!(width_of(Face::SansBold, '\u{e9}'), 597);
        assert_eq!(width_of(Face::Sans, '\u{301}'), 0, "an accent has no width");
        assert_eq!(width_of(Face::Sans, '\u{ad}'), 0, "the soft hyphen");
        assert_eq!(width_of(Face::Sans, '\u{4e2d}'), WIDE, "Chinese: 1 em");
        for c in ['\u{2190}', '\u{21d4}', '\u{221a}', '\u{221e}', '\u{2248}', '\u{2260}', '\u{2264}', '\u{2265}', '\u{25a0}', '\u{25cf}', '\u{25ff}'] {
            assert_eq!(width_of(Face::Sans, c), LETTER, "{c:?}: Noto Sans lacks it, paint takes the Mono glyph, and layout uses 0.6 em");
            assert_eq!(width_of(Face::SansBold, c), LETTER, "{c:?}");
        }
    }
}

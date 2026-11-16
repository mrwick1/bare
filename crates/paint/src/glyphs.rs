//! Glyphs: the font file, and a cache of rasterised glyphs.
//!
//! Layout measures with the advance widths of the face (`bare_layout::font`: 0.6 em for Noto Sans Mono, the numbers of `hmtx`
//! for Noto Sans). Paint draws the outlines of the same face, and it puts each letter where layout says.
//!
//! The faces are subsets of Noto Sans Mono and of Noto Sans, each Regular and Bold (Basic Latin, Latin-1, the punctuation
//! block, currency signs with the rupee, and for Mono also arrows, a few math signs and the geometric shapes; 26 to 32 KB each),
//! built into the program with `include_bytes!`. Both Mono files have the same advance, so bold Mono text keeps its width. Noto Sans
//! lacks the arrows, the math signs and the shapes: they are drawn with the Noto Sans Mono glyph (layout gives them 0.6 em).
//! A fifth face, a subset of Noto Sans Symbols2
//! (4 KB: check marks, crosses, stars, the menu and warning signs), draws the characters that the other faces lack: it is
//! the last fallback. A fourth face, a subset of Noto Sans Arabic (86 KB, the Arabic blocks), draws Arabic letters; it is
//! read on the first Arabic letter. No system font is read. The licenses are `data/fonts/OFL*.txt`.
//! A letter that none of the faces has (Chinese, Japanese, Korean, emoji) is not drawn (a gap in `docs/resources.md`).
//!
//! Why a cache: filling an outline is the costly part of text. A page repeats the same letters at the same size, so
//! each (glyph, size, quarter pixel) is filled once into a coverage mask, and every use is a blit of the mask.
//! Chrome (Skia) keeps a glyph cache with subpixel positions in the same way.

use ab_glyph::{Font as _, FontRef, Glyph, GlyphId, PxScale, point};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

static REGULAR: &[u8] = include_bytes!("../../../data/fonts/NotoSansMono-Regular-subset.ttf");
static BOLD: &[u8] = include_bytes!("../../../data/fonts/NotoSansMono-Bold-subset.ttf");
static SANS_REGULAR: &[u8] = include_bytes!("../../../data/fonts/NotoSans-Regular-subset.ttf");
static SANS_BOLD: &[u8] = include_bytes!("../../../data/fonts/NotoSans-Bold-subset.ttf");
static SYMBOLS: &[u8] = include_bytes!("../../../data/fonts/NotoSansSymbols2-subset.ttf");
static ARABIC: &[u8] = include_bytes!("../../../data/fonts/NotoSansArabic-subset.ttf");

/// The face of a glyph: 0 Mono regular, 1 Mono bold, 2 symbols, 3 Arabic, 4 Sans regular, 5 Sans bold.
pub type FaceId = u8;

/// How many places a glyph can take between two pixels. Skia uses 4 as well.
pub const SUBPIXELS: f32 = 4.0;

/// The cache stops here and starts again from empty. A page with many sizes (a zoom) cannot make it grow without limit.
const MAX_BYTES: usize = 8 << 20;

/// A hash for a key that is a number: one multiply. The standard hash (SipHash) costs more than the lookup is worth.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// Where the mask of one glyph is: `w * h` bytes at `at` in `Glyphs::masks`. `left` and `top` are from the pen
/// (the baseline, at the whole pixel left of the glyph) to the first pixel of the mask.
#[derive(Clone, Copy)]
pub struct Entry {
    pub at: u32,
    pub w: u16,
    pub h: u16,
    pub left: i16,
    pub top: i16,
}

/// One font file.
struct Face {
    font: FontRef<'static>,
    /// `ab_glyph` sizes a font by its ascent plus descent. This turns an em size into that scale.
    scale: f32,
    /// The glyph of each ASCII letter, so the common case needs no search in the font's character table.
    ascii: [u16; 128],
}

impl Face {
    fn new(data: &'static [u8]) -> Face {
        let font = FontRef::try_from_slice(data).expect("the built-in font is a valid TrueType file");
        let scale = font.height_unscaled() / font.units_per_em().expect("the font has units per em");
        let mut ascii = [0u16; 128];
        for (c, id) in ascii.iter_mut().enumerate() {
            *id = font.glyph_id(c as u8 as char).0;
        }
        Face { font, scale, ascii }
    }
}

/// The Arabic face, made on the first Arabic letter: a page with no Arabic never pays for it.
fn arabic_face() -> &'static Face {
    static FACE: std::sync::OnceLock<Face> = std::sync::OnceLock::new();
    FACE.get_or_init(|| Face::new(ARABIC))
}

pub struct Glyphs {
    /// Noto Sans Mono Regular and Bold, and Symbols2.
    faces: [Face; 3],
    /// Noto Sans Regular and Bold.
    sans: [Face; 2],
    entries: HashMap<u64, Entry, BuildHasherDefault<Fx>>,
    /// All masks, one after the other: no allocation for each glyph.
    masks: Vec<u8>,
}

impl Glyphs {
    pub fn new() -> Glyphs {
        Glyphs {
            faces: [Face::new(REGULAR), Face::new(BOLD), Face::new(SYMBOLS)],
            sans: [Face::new(SANS_REGULAR), Face::new(SANS_BOLD)],
            entries: HashMap::default(), masks: Vec::new() }
    }

    /// The font of a face id.
    fn font(&self, id: FaceId) -> &Face {
        match id {
            3 => arabic_face(),
            4 | 5 => &self.sans[id as usize - 4],
            _ => &self.faces[id as usize],
        }
    }

    /// The face and the glyph of a character, in the face that layout measured with (`face`), or in a fallback when that face lacks it.
    /// The glyph 0 is the "missing" glyph: no face has an outline for it.
    pub fn glyph(&self, face: bare_layout::font::Face, c: char) -> (FaceId, u16) {
        use bare_layout::font::Face as F;
        let main: FaceId = match face {
            F::Mono => 0,
            F::MonoBold => 1,
            F::Sans => 4,
            F::SansBold => 5,
        };
        let font = self.font(main);
        let id = match c as usize {
            i if i < 128 => font.ascii[i],
            _ => font.font.glyph_id(c).0,
        };
        if id == 0 { return self.fallback(face, c); }
        (main, id)
    }

    /// Arabic letters come from the Arabic face. Noto Sans lacks the arrows, the math signs and the shapes that Noto Sans Mono has: they
    /// come from Noto Sans Mono. Then the symbols face, and no glyph.
    #[cold]
    #[inline(never)]
    fn fallback(&self, face: bare_layout::font::Face, c: char) -> (FaceId, u16) {
        use bare_layout::font::Face as F;
        if matches!(c as u32, 0x0600..=0x06ff | 0x0750..=0x077f | 0x08a0..=0x08ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff) {
            return (3, arabic_face().font.glyph_id(c).0);
        }
        let bold = matches!(face, F::MonoBold | F::SansBold) as FaceId;
        if !face.is_mono() {
            let id = self.faces[bold as usize].font.glyph_id(c).0;
            if id != 0 { return (bold, id); }
        }
        if c as u32 >= 0x2000 { return (2, self.faces[2].font.glyph_id(c).0); }
        (bold, 0)
    }

    /// The mask of a glyph at `size` pixels and a place `sub` quarters of a pixel right of a whole pixel. Made on the first call.
    pub fn get(&mut self, face: FaceId, id: u16, size: f32, sub: u8) -> Entry {
        let key = (size.to_bits() as u64) << 32 | (id as u64) << 8 | (face as u64) << 4 | sub as u64;
        if let Some(&e) = self.entries.get(&key) {
            return e;
        }
        if self.masks.len() > MAX_BYTES {
            self.entries.clear();
            self.masks.clear();
        }
        let e = self.fill(face, id, size, sub);
        self.entries.insert(key, e);
        e
    }

    fn fill(&mut self, face: FaceId, id: u16, size: f32, sub: u8) -> Entry {
        // Not `self.font(face)`: that would borrow all of `self`, and `masks` changes below.
        let face = match face {
            3 => arabic_face(),
            4 | 5 => &self.sans[face as usize - 4],
            _ => &self.faces[face as usize],
        };
        let glyph = Glyph { id: GlyphId(id), scale: PxScale::from(size * face.scale), position: point(sub as f32 / SUBPIXELS, 0.0) };
        let at = self.masks.len() as u32;
        let Some(outline) = face.font.outline_glyph(glyph) else {
            // A space has no outline.
            return Entry { at, w: 0, h: 0, left: 0, top: 0 };
        };
        let b = outline.px_bounds();
        // A glyph of 10,000 px (the biggest font size) would make a mask of 100 MB: draw nothing at that size.
        let (w, h) = ((b.max.x - b.min.x) as usize, (b.max.y - b.min.y) as usize);
        if w * h > 1 << 20 || w > u16::MAX as usize || h > u16::MAX as usize {
            return Entry { at, w: 0, h: 0, left: 0, top: 0 };
        }
        self.masks.resize(self.masks.len() + w * h, 0);
        let mask = &mut self.masks[at as usize..];
        outline.draw(|x, y, c| mask[y as usize * w + x as usize] = (c * 255.0 + 0.5) as u8);
        Entry { at, w: w as u16, h: h as u16, left: b.min.x as i16, top: b.min.y as i16 }
    }

    pub fn mask(&self, e: Entry) -> &[u8] {
        &self.masks[e.at as usize..e.at as usize + e.w as usize * e.h as usize]
    }

    /// How many glyphs are cached, and how many bytes their masks take.
    pub fn stats(&self) -> (usize, usize) {
        (self.entries.len(), self.masks.len())
    }
}

impl Default for Glyphs {
    fn default() -> Glyphs {
        Glyphs::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_layout::font::{self, Face as F};

    #[test]
    fn built_in_arabic_face_supplies_missing_letters() {
        let mut g = Glyphs::new();
        for c in ['ع', 'ب', 'ﻋ', 'ﷲ'] {
            let (face, id) = g.glyph(F::Sans, c);
            assert_eq!(face, 3, "{c}");
            assert_ne!(id, 0, "{c}");
            let entry = g.get(face, id, 20.0, 0);
            assert!(g.mask(entry).iter().any(|&v| v != 0), "{c}");
        }
    }

    #[test]
    fn letters_have_masks_and_spaces_do_not() {
        let mut g = Glyphs::new();
        let (face, a) = g.glyph(F::Mono, 'A');
        assert_ne!(a, 0);
        let e = g.get(face, a, 16.0, 0);
        assert!(e.w > 0 && e.h > 0);
        // The capital A of a 16 px font is about 11 px tall and sits on the baseline: its mask is above it.
        assert!(e.top < 0 && e.top > -16, "{}", e.top);
        assert!(g.mask(e).contains(&255));
        let (face, space) = g.glyph(F::Mono, ' ');
        let s = g.get(face, space, 16.0, 0);
        assert_eq!((s.w, s.h), (0, 0));
        // The same request is a hit: no new mask.
        let before = g.stats();
        g.get(face, a, 16.0, 0);
        assert_eq!(g.stats(), before);
        g.get(face, a, 16.0, 1);
        assert_eq!(g.stats().0, before.0 + 1);
        // The bold face has its own mask, with more ink.
        let (bface, bid) = g.glyph(F::MonoBold, 'A');
        let bold = g.get(bface, bid, 16.0, 0);
        let ink = |e| g.mask(e).iter().map(|&c| c as u32).sum::<u32>();
        assert!(ink(bold) > ink(e));
    }

    /// Noto Sans Mono: every letter is `LETTER` (0.6 em) wide, as layout measures it.
    #[test]
    fn the_mono_advance_is_the_layout_letter_width() {
        for data in [REGULAR, BOLD] {
            let font = FontRef::try_from_slice(data).unwrap();
            let units = font.units_per_em().unwrap();
            assert_eq!(font.h_advance_unscaled(font.glyph_id('m')) / units * 1000.0, font::LETTER as f32);
        }
    }

    /// The tables of `bare_layout::advances` (made by `tools/gen_font_advances.py`) hold the advance of each letter of the Noto Sans files, and
    /// `font::width_of` reads them. The font has 1000 units to the em, so the advance in `hmtx` is the layout unit.
    #[test]
    fn the_advance_tables_match_the_font_files() {
        use bare_layout::advances;
        for (data, face, ascii, rest) in [
            (SANS_REGULAR, F::Sans, &advances::REGULAR_ASCII[..], &advances::REGULAR[..]),
            (SANS_BOLD, F::SansBold, &advances::BOLD_ASCII[..], &advances::BOLD[..]),
        ] {
            let font = FontRef::try_from_slice(data).unwrap();
            assert_eq!(font.units_per_em().unwrap(), 1000.0);
            let advance = |c: char| font.h_advance_unscaled(font.glyph_id(c));
            // Each letter of the font is in the tables with its advance.
            let mut found = 0;
            for (id, c) in font.codepoint_ids() {
                assert_ne!(id.0, 0);
                if (c as usize) < 128 {
                    assert_eq!(ascii[c as usize] as f32, advance(c), "{c:?}");
                } else {
                    let at = rest.binary_search_by_key(&(c as u32), |&(cp, _)| cp).unwrap_or_else(|_| panic!("{c:?} is not in the table"));
                    assert_eq!(rest[at].1 as f32, advance(c), "{c:?}");
                    found += 1;
                }
                assert_eq!(font::width_of(face, c) as f32, advance(c), "{c:?}");
            }
            // And the tables have no other letter.
            assert_eq!(found, rest.len());
            assert!(rest.windows(2).all(|w| w[0].0 < w[1].0), "sorted");
            for (c, &a) in ascii.iter().enumerate() {
                let c = c as u8 as char;
                if font.glyph_id(c).0 == 0 {
                    assert_eq!(a as u32, font::LETTER, "{c:?}: a letter that the font lacks has the width of Mono");
                }
            }
            assert_eq!(face.space() as f32, advance(' '));
        }
    }

    /// A glyph is drawn with the face that layout measured with. Noto Sans lacks the arrows, math signs and shapes: they come from Noto Sans Mono.
    #[test]
    fn sans_text_draws_the_mono_glyph_for_what_noto_sans_lacks() {
        let g = Glyphs::new();
        assert_eq!(g.glyph(F::Sans, 'a').0, 4);
        assert_eq!(g.glyph(F::SansBold, 'a').0, 5);
        assert_eq!(g.glyph(F::Mono, 'a').0, 0);
        assert_eq!(g.glyph(F::MonoBold, 'a').0, 1);
        for (face, mono) in [(F::Sans, 0), (F::SansBold, 1)] {
            for c in ['\u{2190}', '\u{2195}', '\u{21d0}', '\u{21d4}', '\u{221a}', '\u{221e}', '\u{2248}', '\u{2260}', '\u{2264}', '\u{2265}', '\u{25a0}', '\u{25cb}', '\u{25ff}'] {
                let (id, glyph) = g.glyph(face, c);
                assert_eq!(id, mono, "{c:?}");
                assert_ne!(glyph, 0, "{c:?}");
            }
        }
        assert_eq!(g.glyph(F::Sans, '\u{2713}').0, 2, "the check mark: Symbols2");
    }

    #[test]
    fn missing_characters_are_glyph_zero() {
        let g = Glyphs::new();
        assert_eq!(g.glyph(F::Mono, '\u{4e2d}').1, 0);
        // The rupee sign is in the main face, the check mark only in the fallback.
        assert_eq!(g.glyph(F::Mono, '\u{20b9}').0, 0);
        assert_ne!(g.glyph(F::Mono, '\u{20b9}').1, 0);
        assert_eq!(g.glyph(F::MonoBold, '\u{2713}').0, 2);
        assert_ne!(g.glyph(F::MonoBold, '\u{2713}').1, 0);
    }
}

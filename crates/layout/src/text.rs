//! Text: cut the text of a page into words and spaces, follow the `white-space` rules, and measure each piece.
//!
//! In HTML, `a    b` and `a\n\n  b` both show as `a b`: a run of spaces, tabs and newlines is one space ("collapsing").
//! A space at the start or the end of a line is not shown. `white-space` changes this:
//!
//! ```text
//!               spaces     newlines   lines wrap
//! normal        collapse   collapse   yes
//! nowrap        collapse   collapse   no
//! pre           keep       keep       no
//! pre-wrap      keep       keep       yes
//! pre-line      collapse   keep       yes
//! break-spaces  keep       keep       yes
//! ```
//!
//! How real browsers do it: Chrome (LayoutNG) copies the text of a whole paragraph into one new string, with the
//! spaces already collapsed and `text-transform` already done (`NGInlineItemsBuilder`). It keeps a map from the places
//! in that string back to the places in the DOM (`NGOffsetMapping`), for the caret and for selection. Firefox keeps a
//! transformed copy of the text in each text run (`gfxTextRun`). Both pay with a second copy of all the text on the page.
//!
//! Our choice: no copy. A `Piece` is a range of bytes in the text of one DOM node, with its width. A run of spaces is
//! one `Space` piece, and paint draws it as one space. So the text of the page stays in one place, the DOM's text buffer,
//! and paint reads it from there. The one thing that crosses text nodes is whether the last piece was a space: in
//! `a <b> b</b>` the space in the `b` element is thrown away. `Flow` carries it from one node to the next.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::font::{self, LETTER, Units};
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Property, Value};

/// The `white-space` rules of a box, as three switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhiteSpace {
    /// A run of spaces is one space.
    pub collapse: bool,
    /// A newline ends the line.
    pub newlines: bool,
    /// A line may break at a space when the next word does not fit.
    pub wrap: bool,
}

impl WhiteSpace {
    pub fn of(style: &ComputedStyle) -> WhiteSpace {
        let (collapse, newlines, wrap) = match style.get(Property::WhiteSpace) {
            Value::Keyword(Keyword::Nowrap) => (true, false, false),
            Value::Keyword(Keyword::Pre) => (false, true, false),
            Value::Keyword(Keyword::PreWrap | Keyword::BreakSpaces) => (false, true, true),
            Value::Keyword(Keyword::PreLine) => (true, true, true),
            _ => (true, false, true),
            // `normal`
        };
        WhiteSpace { collapse, newlines, wrap }
    }
}

/// The effective policy; no-wrap white-space always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wrap {
    Normal,
    BreakWord,
    Anywhere,
    All,
}

impl Wrap {
    pub(crate) fn of(style: &ComputedStyle, ws: WhiteSpace) -> Self {
        if !ws.wrap { return Self::Normal; }
        if style.get(Property::WordBreak) == Value::Keyword(Keyword::BreakAll) { return Self::All; }
        match style.get(Property::OverflowWrap) {
            Value::Keyword(Keyword::Anywhere) => Self::Anywhere,
            Value::Keyword(Keyword::BreakWord) => Self::BreakWord,
            _ => Self::Normal,
        }
    }

    pub(crate) fn min_content(self) -> bool { matches!(self, Self::Anywhere | Self::All) }
}

/// Segment one affected unbreakable unit, including seams between DOM text nodes.
/// Callers retain the source ranges; ICU returns UTF-8 byte boundaries in this join.
pub(crate) fn grapheme_breaks<'a>(parts: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut joined = String::new();
    for part in parts { joined.push_str(part); }
    icu_segmenter::GraphemeClusterSegmenter::new().segment_str(&joined).collect()
}

/// The same case conversion as Pieces::width, with word-start state across sources.
/// Spacing still counts source letters, as font::extra does.
pub(crate) fn transformed_width(c: char, transform: Transform, word_start: &mut bool) -> Units {
    let width = match transform {
        Transform::Upper => c.to_uppercase().map(font::width_of).sum(),
        Transform::Lower => c.to_lowercase().map(font::width_of).sum(),
        Transform::Capitalize if *word_start => c.to_uppercase().map(font::width_of).sum(),
        _ => font::width_of(c),
    };
    *word_start = matches!(c, ' ' | '\t' | '\n');
    width
}

/// `text-transform`: the letters change case when they are shown. The DOM keeps the text as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    None,
    Upper,
    Lower,
    /// The first letter of each word.
    Capitalize,
}

impl Transform {
    pub fn of(style: &ComputedStyle) -> Transform {
        match style.get(Property::TextTransform) {
            Value::Keyword(Keyword::Uppercase) => Transform::Upper,
            Value::Keyword(Keyword::Lowercase) => Transform::Lower,
            Value::Keyword(Keyword::Capitalize) => Transform::Capitalize,
            _ => Transform::None,
        }
    }
}

/// One piece of the text of a node. `start` and `end` are byte places in the text of that node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// Letters with no space between them. The word may go on in the next text node: `a<b>b</b>` is one word.
    Word { start: u32, end: u32, width: Units },
    /// Spaces. With `collapse`, the whole run of spaces, tabs and newlines is one space wide. A line may break after it.
    Space { start: u32, end: u32, width: Units },
    /// A newline that ends the line (`pre`, `pre-wrap`, `pre-line`).
    Newline { at: u32 },
    /// A tab that is kept (`pre`, `pre-wrap`). It goes to the next tab stop: the line breaker knows where that is.
    Tab { at: u32 },
}

/// What one text node tells the next one in the same paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    /// The last piece was a space, or this is the start of a line. A collapsible space here is thrown away.
    pub after_space: bool,
}

impl Flow {
    /// The start of a paragraph: as after a space, so that the spaces at the start are thrown away.
    pub fn new() -> Flow {
        Flow { after_space: true }
    }
}

impl Default for Flow {
    fn default() -> Flow {
        Flow::new()
    }
}

/// The pieces of the text of one node. JS: like a generator that yields `Piece`s.
pub struct Pieces<'t> {
    text: &'t str,
    pos: usize,
    ws: WhiteSpace,
    transform: Transform,
    flow: &'t mut Flow,
    // `&mut`: the iterator changes the `Flow` of its caller, so the next text node sees where this one ended.
}

/// Cut the text of a node into pieces.
pub fn pieces<'t>(text: &'t str, ws: WhiteSpace, transform: Transform, flow: &'t mut Flow) -> Pieces<'t> {
    Pieces { text, pos: 0, ws, transform, flow }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n')
    // The HTML parser already turned `\r` into `\n`. The form feed (`\x0c`) stays a letter, as in Chrome.
}

impl Iterator for Pieces<'_> {
    type Item = Piece;

    fn next(&mut self) -> Option<Piece> {
        let bytes = self.text.as_bytes();
        loop {
            let start = self.pos;
            let &b = bytes.get(start)?;
            if !is_space(b) {
                // A word: up to the next space. A byte of a letter outside ASCII is never a space byte, so the end is
                // always between two letters.
                // One pass finds the end and checks for letters outside ASCII (a byte of 0x80 or more).
                let mut end = start;
                let mut high = 0;
                while end < bytes.len() && !is_space(bytes[end]) {
                    high |= bytes[end];
                    end += 1;
                }
                self.pos = end;
                let word = &self.text[start..end];
                let width = if high < 0x80 { (word.len() as Units).saturating_mul(LETTER) } else { self.width(word) };
                self.flow.after_space = false;
                return Some(Piece::Word { start: start as u32, end: end as u32, width });
            }
            if !self.ws.collapse {
                // Spaces are kept: each space is one letter wide, each tab and newline is a piece of its own.
                self.flow.after_space = true;
                self.pos += 1;
                match b {
                    b'\n' if self.ws.newlines => return Some(Piece::Newline { at: start as u32 }),
                    b'\t' => return Some(Piece::Tab { at: start as u32 }),
                    _ => {
                        let end = bytes[start..].iter().position(|&b| b != b' ').map_or(bytes.len(), |i| start + i);
                        self.pos = end.max(start + 1);
                        let count = (self.pos - start) as Units;
                        return Some(Piece::Space { start: start as u32, end: self.pos as u32, width: count.saturating_mul(LETTER) });
                    }
                }
            }
            // Spaces collapse. With `pre-line` a newline still ends the line, and the spaces around it go.
            let end = bytes[start..].iter().position(|&b| !is_space(b)).map_or(bytes.len(), |i| start + i);
            if self.ws.newlines
                && let Some(i) = bytes[start..end].iter().position(|&b| b == b'\n')
            {
                self.pos = start + i + 1;
                self.flow.after_space = true;
                return Some(Piece::Newline { at: (start + i) as u32 });
            }
            self.pos = end;
            if self.flow.after_space {
                continue;
                // A second space in a row, or a space at the start of a line: nothing to show.
            }
            self.flow.after_space = true;
            return Some(Piece::Space { start: start as u32, end: end as u32, width: LETTER });
        }
    }
}

impl Pieces<'_> {
    /// The width of a word, after `text-transform`. A change of case can change the number of letters: `ß` becomes `SS`.
    fn width(&self, word: &str) -> Units {
        if word.is_ascii() || self.transform == Transform::None {
            return font::width(word);
            // In ASCII, a change of case never changes the number of letters.
        }
        let upper = |c: char| c.to_uppercase().fold(0, |sum: Units, u| sum + font::width_of(u));
        let lower = |c: char| c.to_lowercase().fold(0, |sum: Units, l| sum + font::width_of(l));
        let mut chars = word.chars();
        let first = match (self.transform, self.flow.after_space) {
            (Transform::Upper, _) | (Transform::Capitalize, true) => chars.next().map_or(0, upper),
            _ => 0,
        };
        let rest: Units = match self.transform {
            Transform::Upper => chars.map(upper).sum(),
            Transform::Lower => chars.map(lower).sum(),
            _ => chars.map(font::width_of).sum(),
        };
        first.saturating_add(rest)
    }
}

/// Apply `text-transform` to a piece of text, for paint. Collapsed spaces become one space. `word_start`: the text starts
/// a new word (for `capitalize`).
pub fn shown(text: &str, ws: WhiteSpace, transform: Transform, mut word_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        let space = matches!(c, ' ' | '\t' | '\n');
        if space && ws.collapse {
            if !last_space {
                out.push(' ');
            }
        } else {
            match transform {
                Transform::Upper => out.extend(c.to_uppercase()),
                Transform::Lower => out.extend(c.to_lowercase()),
                Transform::Capitalize if word_start && !space => out.extend(c.to_uppercase()),
                _ => out.push(c),
            }
        }
        last_space = space;
        word_start = space;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: WhiteSpace = WhiteSpace { collapse: true, newlines: false, wrap: true };
    const PRE: WhiteSpace = WhiteSpace { collapse: false, newlines: true, wrap: false };
    const PRE_LINE: WhiteSpace = WhiteSpace { collapse: true, newlines: true, wrap: true };

    /// The pieces as short text: `w:abc` for a word, `s` for a space with its width in letters, `n` and `t`.
    fn cut(text: &str, ws: WhiteSpace, transform: Transform, flow: &mut Flow) -> Vec<String> {
        pieces(text, ws, transform, flow)
            .map(|p| match p {
                Piece::Word { start, end, width } => format!("{}:{}", &text[start as usize..end as usize], width),
                Piece::Space { width, .. } => format!("s{}", width / LETTER),
                Piece::Newline { .. } => "n".to_string(),
                Piece::Tab { .. } => "t".to_string(),
            })
            .collect()
    }

    #[test]
    fn spaces_collapse() {
        assert_eq!(cut("  a   b\n\n c  ", NORMAL, Transform::None, &mut Flow::new()), ["a:6", "s1", "b:6", "s1", "c:6", "s1"]);
        // The space at the end stays here: the line breaker drops it at the end of a line, and it may be the space
        // between this node and the next one.
    }

    #[test]
    fn spaces_collapse_across_nodes() {
        let mut flow = Flow::new();
        assert_eq!(cut("a ", NORMAL, Transform::None, &mut flow), ["a:6", "s1"]);
        assert_eq!(cut(" b", NORMAL, Transform::None, &mut flow), ["b:6"]);
        assert_eq!(cut("   ", NORMAL, Transform::None, &mut flow), ["s1"], "the space between b and c");
        assert_eq!(cut(" ", NORMAL, Transform::None, &mut flow), Vec::<String>::new(), "a second space in a row");
        assert_eq!(cut("c", NORMAL, Transform::None, &mut flow), ["c:6"]);
        assert_eq!(cut("d", NORMAL, Transform::None, &mut flow), ["d:6"]);
    }

    #[test]
    fn pre_keeps_everything() {
        assert_eq!(cut("a  b\n\tc", PRE, Transform::None, &mut Flow::new()), ["a:6", "s2", "b:6", "n", "t", "c:6"]);
        assert_eq!(cut("  ", PRE, Transform::None, &mut Flow::new()), ["s2"]);
    }

    #[test]
    fn pre_line_keeps_newlines_only() {
        assert_eq!(cut("a  \n  b\n\nc", PRE_LINE, Transform::None, &mut Flow::new()), ["a:6", "n", "b:6", "n", "n", "c:6"]);
        // The spaces before and after a newline go (CSS Text 3, "Phase I: Collapsing and Transformation").
    }

    #[test]
    fn graphemes_cross_source_parts_and_keep_utf8_offsets() {
        assert_eq!(super::grapheme_breaks(["e", "\u{301}", "👩", "\u{200d}", "💻", "x"].into_iter()), vec![0, 3, 14, 15]);
    }

    #[test]
    fn widths() {
        assert_eq!(font::width("hello"), 30);
        assert_eq!(font::width("e\u{301}"), 6, "an accent on a letter has no width");
        assert_eq!(font::width("\u{4e2d}\u{6587}"), 20, "Chinese letters are 1 em");
        assert_eq!(font::width("caf\u{e9}"), 24);
        assert_eq!(cut("stra\u{df}e", NORMAL, Transform::Upper, &mut Flow::new()), ["stra\u{df}e:42"], "\u{df} becomes SS");
        assert_eq!(cut("\u{df}a \u{df}b", NORMAL, Transform::Capitalize, &mut Flow::new()), ["\u{df}a:18", "s1", "\u{df}b:18"]);
        let mut flow = Flow { after_space: false };
        assert_eq!(cut("\u{df}c", NORMAL, Transform::Capitalize, &mut flow), ["\u{df}c:12"], "not the start of a word");
    }

    #[test]
    fn shown_text() {
        assert_eq!(shown("a  \n b", NORMAL, Transform::None, true), "a b");
        assert_eq!(shown("hello world", NORMAL, Transform::Capitalize, true), "Hello World");
        assert_eq!(shown("a  b", PRE, Transform::Upper, true), "A  B");
    }

    #[test]
    fn metrics() {
        let m = font::Metrics::new(16.0);
        assert_eq!((m.ascent, m.descent), (17.0, 5.0));
        assert_eq!(m.normal_line_height(), 22.0);
        assert_eq!(m.px(60), 96.0, "ten letters at 16px");
    }
}

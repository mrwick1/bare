//! Grid layout (CSS Grid Layout 1).
//!
//! The steps, in the order of the standard:
//! 1. The items (`collect`): the child elements, and each run of text between them as one anonymous item. An absolute box is no item.
//! 2. The explicit grid (`expand`): the tracks of `grid-template-*`, with `repeat()` opened. `auto-fill` and `auto-fit` ask how much room there is.
//! 3. Placement (`place`, standard 8.5): items with a place, then items with a row, then the rest with the cursor, sparse or `dense`.
//!    The grid grows with implicit tracks where the items need them (the sizes come from `grid-auto-*`).
//! 4. Track sizes (`size_tracks`, standard 11): columns first, with the widths of the items. Each item is then laid out at the
//!    width of its columns, and its height gives the rows. Rows go the same way as columns.
//! 5. Alignment of the tracks (`justify-content`, `align-content`), then of each item in its area, then the fragments.
//!
//! Choices for speed and memory:
//! - Placement keeps the used cells in a bit set (`Occ`), one row of words for each row of the grid. A search for a free place
//!   skips 64 cells at a time. In dense mode, a search starts at the row where an item of the same shape was found last: all the
//!   rows before it are full for that shape, and a cell never becomes free. So a grid of N items costs O(N + cells / 64).
//! - The items are laid out once, in one list of fragments (`cx.out` is swapped for it). An item that stretches and holds boxes
//!   is laid out a second time at its stretched height, and its first fragments are left in the list, dead. Only the live
//!   ranges are copied to the page. A stretched item with no box inside only gets a taller fragment.
//! - The intrinsic widths of an item are measured only if it is in a track that needs them.
//!
//! Known gaps (see `docs/decisions.md`):
//! - `baseline` alignment is `start`, `last baseline` is `end`. There is no subgrid and no masonry.
//! - Names of lines work in the track lists and in the areas. `span name` counts as `span 1` if the name is not found, and
//!   finds the name only when the start line is known.
//! - An absolute child sits at the start of the content box, and not in the grid area that its lines name.
//! - A percent track in a grid with `height: auto` is `auto`. `max-height` does not limit the free space of the rows.
//! - The tracks of the grid are not cut at 10,000: the items are. A grid has at most 10,000 tracks on each side.

use crate::Cx;
use crate::block::{self, Kids, MAX_PX, Pos, Sides, Space, border_padding, resolve, widths};
use crate::boxes::{Child, Kind};
use crate::fragment::{Fragment, Kind as FragmentKind};
use crate::{inline, intrinsic};
use bare_css::grid::{Areas, Bound, Count, FLOW_COLUMN, FLOW_DENSE, Grid, Line, MAX_TRACKS, Part, Track};
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::NodeId;
use std::collections::HashMap;

const INF: f32 = f32::INFINITY;
/// A line that is not set yet.
const NONE: i32 = i32::MIN;
/// The three contributions of an item to a track: min-content, max-content and the minimum (the automatic minimum size).
const MIN: usize = 0;
const MAX: usize = 1;
const LEAST: usize = 2;

/// One side of a track size, in pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Size {
    Auto,
    MinContent,
    MaxContent,
    Px(f32),
    Fr(f32),
    /// `fit-content(px)`.
    Fit(f32),
}

/// The size of one track.
#[derive(Clone, Copy, Debug)]
struct Def {
    min: Size,
    max: Size,
}

const AUTO: Def = Def { min: Size::Auto, max: Size::Auto };

/// A size from the computed bound. A percent of an unknown base is `auto`.
fn size(b: Bound, base: Option<f32>) -> Size {
    let px = |l: Length| match l {
        Length::Px(x) => Some(x.min(MAX_PX)),
        Length::Percent(p) => base.map(|b| (b * p / 100.0).clamp(0.0, MAX_PX)),
        _ => None,
    };
    match b {
        Bound::Auto => Size::Auto,
        Bound::MinContent => Size::MinContent,
        Bound::MaxContent => Size::MaxContent,
        Bound::Len(l) => px(l).map_or(Size::Auto, Size::Px),
        Bound::Fr(f) => Size::Fr(f),
        Bound::Fit(l) => px(l).map_or(Size::MaxContent, Size::Fit),
    }
}

fn def(t: &Track, base: Option<f32>) -> Def {
    Def { min: size(t.min, base), max: size(t.max, base) }
}

/// The explicit tracks of one side of the grid.
struct Template<'a> {
    defs: Vec<Def>,
    /// The tracks of an `auto-fit` repeat. They collapse if no item is in them.
    fit: std::ops::Range<usize>,
    /// The names of the lines: (name, line number from 1).
    names: Vec<(&'a str, i32)>,
}

/// Open the `repeat()` of a track list. `avail`: the room, for the count of an auto repeat.
fn expand(parts: &[Part], avail: Option<f32>, gap: f32) -> Template<'_> {
    let reps = auto_count(parts, avail, gap);
    let mut t = Template { defs: Vec::new(), fit: 0..0, names: Vec::new() };
    emit(parts, reps, avail, &mut t);
    t
}

fn emit<'a>(parts: &'a [Part], reps: usize, base: Option<f32>, t: &mut Template<'a>) {
    for part in parts {
        match part {
            Part::Names(names) => t.names.extend(names.iter().map(|n| (&**n, t.defs.len() as i32 + 1))),
            Part::Track(track) if t.defs.len() < MAX_TRACKS => t.defs.push(def(track, base)),
            Part::Track(_) => {}
            Part::Repeat(count, inner) => {
                let n = match count {
                    Count::Times(n) => *n as usize,
                    _ => reps,
                };
                let start = t.defs.len();
                for _ in 0..n {
                    if t.defs.len() >= MAX_TRACKS {
                        break;
                    }
                    emit(inner, reps, base, t);
                }
                if *count == Count::Fit {
                    t.fit = start..t.defs.len();
                }
            }
        }
    }
}

/// How many times an `auto-fill` or `auto-fit` repeats (standard 7.2.3.2): as many as fit in `avail`, at least 1. With no room
/// that is known, once.
fn auto_count(parts: &[Part], avail: Option<f32>, gap: f32) -> usize {
    let Some(avail) = avail else { return 1 };
    // The size that a track counts for here: its length, from the max side, else from the min side.
    let fixed = |t: &Track| match (t.max, t.min) {
        (Bound::Len(l), _) | (_, Bound::Len(l)) => match size(Bound::Len(l), Some(avail)) {
            Size::Px(x) => x,
            _ => 0.0,
        },
        _ => 0.0,
    };
    let (mut others, mut others_n, mut one, mut per) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut has_auto = false;
    for part in parts {
        match part {
            Part::Track(t) => {
                others += fixed(t);
                others_n += 1.0;
            }
            Part::Repeat(Count::Times(n), inner) => {
                for t in inner.iter().filter_map(|p| if let Part::Track(t) = p { Some(t) } else { None }) {
                    others += fixed(t) * *n as f32;
                    others_n += *n as f32;
                }
            }
            Part::Repeat(_, inner) => {
                has_auto = true;
                for t in inner.iter().filter_map(|p| if let Part::Track(t) = p { Some(t) } else { None }) {
                    one += fixed(t);
                    per += 1.0;
                }
            }
            Part::Names(_) => {}
        }
    }
    if !has_auto {
        return 1;
    }
    // others + n * one + (others_n + n * per - 1) * gap <= avail
    let each = one + per * gap;
    if each <= 0.0 {
        return 1;
    }
    let n = ((avail - others - (others_n - 1.0) * gap) / each).floor();
    if n.is_nan() || n < 1.0 { 1 } else { (n as usize).min(MAX_TRACKS) }
}

/// How an item sits in its area on one axis.
#[derive(Clone, Copy, PartialEq)]
enum Align {
    Stretch,
    Start,
    End,
    Center,
}

fn align(v: Value, fallback: Align) -> Align {
    match v {
        Value::Keyword(Keyword::Start | Keyword::FlexStart | Keyword::SelfStart | Keyword::Left | Keyword::Baseline) => Align::Start,
        Value::Keyword(Keyword::End | Keyword::FlexEnd | Keyword::SelfEnd | Keyword::Right | Keyword::LastBaseline) => Align::End,
        Value::Keyword(Keyword::Center) => Align::Center,
        Value::Keyword(Keyword::Stretch | Keyword::Normal) => Align::Stretch,
        _ => fallback,
    }
}

/// Where a line is: a line number, `NONE` if it is not set (the item goes where auto placement puts it), and how many tracks it spans.
#[derive(Clone, Copy)]
struct Place {
    start: i32,
    span: u32,
}

/// One grid item.
struct Item {
    /// The element. For a run of text, the grid container.
    node: NodeId,
    /// A run of text and inline boxes: the first and the last sibling.
    run: Option<(NodeId, NodeId)>,
    order: i32,
    /// The number of a list item, for its marker.
    number: i32,
    /// The lines, for rows and for columns.
    place: [Place; 2],
    /// The tracks that it covers: the first and the end, for rows and for columns.
    area: [(u32, u32); 2],
    /// The contributions to the tracks of each side.
    contrib: [[f32; 3]; 2],
}

/// What is known about the lines of one side while the places are read.
struct Lines<'a> {
    /// The number of explicit tracks: the template, or the areas if they are more.
    explicit: i32,
    names: &'a [(&'a str, i32)],
    areas: Option<&'a Areas>,
}

impl Lines<'_> {
    /// The line that a `Line` names, `None` for `auto` and `span`. `end`: the line is the end side.
    fn find(&self, line: &Line, axis: usize, end: bool) -> Option<i32> {
        match line {
            Line::Auto | Line::Span(..) => None,
            Line::Num(n) => Some(if *n > 0 { *n } else { self.explicit + 2 + n }),
            Line::Name(name, k) => {
                // An area name stands for the line `name-start` or `name-end`.
                let area = |name: &str, end: bool| self.areas?.names.iter().find(|(n, _)| &**n == name).map(|(_, b)| if end { b[axis * 2 + 1] } else { b[axis * 2] } as i32);
                if *k == 1
                    && let Some(l) = area(name, end)
                {
                    return Some(l);
                }
                if let Some(stem) = name.strip_suffix("-start").and_then(|s| area(s, false)).or_else(|| name.strip_suffix("-end").and_then(|s| area(s, true))) {
                    return Some(stem);
                }
                let mut hits = self.names.iter().filter(|(n, _)| *n == &**name);
                let found = if *k > 0 { hits.nth(*k as usize - 1) } else { hits.collect::<Vec<_>>().into_iter().rev().nth((-*k) as usize - 1) };
                // A line that is not there: the lines of the implicit grid all have this name.
                Some(found.map_or(if *k > 0 { self.explicit + 1 + k } else { 1 + k }, |h| h.1))
            }
        }
    }

    /// The place of an item on this side from its two lines (standard 8.3 and 8.4).
    fn place(&self, start: &Line, end: &Line, axis: usize) -> Place {
        let span = |l: &Line| match l {
            Line::Span(_, n) => Some((*n).min(MAX_TRACKS as u32)),
            _ => None,
        };
        let limit = |l: i32| l.clamp(-(MAX_TRACKS as i32), MAX_TRACKS as i32);
        let (s, e) = (self.find(start, axis, false).map(limit), self.find(end, axis, true).map(limit));
        let (ss, es) = (span(start), span(end));
        match (s, e, ss, es) {
            (Some(s), Some(e), ..) if s < e => Place { start: s, span: (e - s) as u32 },
            (Some(s), Some(e), ..) if s > e => Place { start: e, span: (s - e) as u32 },
            (Some(s), Some(_), ..) => Place { start: s, span: 1 },
            (Some(s), None, _, Some(n)) => Place { start: s, span: n },
            (Some(s), None, ..) => Place { start: s, span: 1 },
            (None, Some(e), Some(n), _) => Place { start: limit(e - n as i32), span: n },
            (None, Some(e), ..) => Place { start: limit(e - 1), span: 1 },
            // Two spans: the end one goes away.
            (None, None, Some(n), _) | (None, None, None, Some(n)) => Place { start: NONE, span: n },
            _ => Place { start: NONE, span: 1 },
        }
    }
}

/// The cells that are used: one row of words for each row of the grid (a grid with the flow `column` has one for each column).
struct Occ {
    /// Words in a row.
    stride: usize,
    rows: usize,
    bits: Vec<u64>,
}

impl Occ {
    fn new(width: usize) -> Occ {
        Occ { stride: width.div_ceil(64).max(1), rows: 0, bits: Vec::new() }
    }

    fn grow_cols(&mut self, width: usize) {
        let need = width.div_ceil(64);
        if need <= self.stride {
            return;
        }
        // Twice as many: the rows are copied once for each doubling.
        let stride = need.max(self.stride * 2);
        let mut bits = vec![0u64; self.rows * stride];
        for r in 0..self.rows {
            bits[r * stride..r * stride + self.stride].copy_from_slice(&self.bits[r * self.stride..(r + 1) * self.stride]);
        }
        (self.stride, self.bits) = (stride, bits);
    }

    fn grow_rows(&mut self, rows: usize) {
        if rows > self.rows {
            self.bits.resize(rows * self.stride, 0);
            self.rows = rows;
        }
    }

    fn mark(&mut self, r0: usize, r1: usize, c0: usize, c1: usize) {
        self.grow_rows(r1);
        self.grow_cols(c1);
        for r in r0..r1 {
            let row = &mut self.bits[r * self.stride..(r + 1) * self.stride];
            for (wi, word) in row.iter_mut().enumerate().take(c1.div_ceil(64)).skip(c0 / 64) {
                *word |= mask(wi, c0, c1);
            }
        }
    }

    /// The used cells of the word `wi`, in all the rows `r0..r1`.
    fn word(&self, r0: usize, r1: usize, wi: usize) -> u64 {
        if wi >= self.stride {
            return 0;
        }
        (r0..r1.min(self.rows)).fold(0, |w, r| w | self.bits[r * self.stride + wi])
    }

    /// The first free cell at or after `from`, in all the rows `r0..r1`, before `end`.
    fn next_free(&self, r0: usize, r1: usize, from: usize, end: usize) -> Option<usize> {
        let mut wi = from / 64;
        let mut low = (1u64 << (from % 64)) - 1;
        while wi * 64 < end {
            let w = self.word(r0, r1, wi) | low;
            if w != !0 {
                let p = wi * 64 + (!w).trailing_zeros() as usize;
                return (p < end).then_some(p);
            }
            (wi, low) = (wi + 1, 0);
        }
        None
    }

    /// The first used cell in `a..b`, in all the rows `r0..r1`.
    fn first_used(&self, r0: usize, r1: usize, a: usize, b: usize) -> Option<usize> {
        for wi in a / 64..b.div_ceil(64) {
            let w = self.word(r0, r1, wi) & mask(wi, a, b);
            if w != 0 {
                return Some(wi * 64 + w.trailing_zeros() as usize);
            }
        }
        None
    }

    /// The first place at or after `from` where `span` cells in a row are free in the rows `r0..r1`, and end before `end`.
    fn fit(&self, r0: usize, r1: usize, from: usize, span: usize, end: usize) -> Option<usize> {
        let mut p = from;
        loop {
            p = self.next_free(r0, r1, p, end.saturating_sub(span - 1))?;
            match self.first_used(r0, r1, p + 1, p + span) {
                None => return Some(p),
                Some(q) => p = q + 1,
            }
        }
    }
}

/// The bits of the word `wi` that are in the cells `a..b`.
fn mask(wi: usize, a: usize, b: usize) -> u64 {
    let (lo, hi) = (a.max(wi * 64) - wi * 64, b.min(wi * 64 + 64).saturating_sub(wi * 64));
    if hi <= lo {
        return 0;
    }
    let top = if hi == 64 { !0 } else { (1u64 << hi) - 1 };
    top & !((1u64 << lo) - 1)
}

/// Give each item its tracks (standard 8.5). `explicit`: the number of explicit tracks for rows and columns. `major`: the
/// axis of the flow (0 for `row`, 1 for `column`). Gives the first line (the start of the implicit grid) and the number of tracks, for both sides.
fn place(items: &mut [Item], explicit: [usize; 2], major: usize, dense: bool) -> ([i32; 2], [usize; 2]) {
    let minor = 1 - major;
    let mut lo = [1i32; 2];
    let mut hi = [explicit[0] as i32 + 1, explicit[1] as i32 + 1];
    for it in items.iter() {
        for a in 0..2 {
            let p = it.place[a];
            if p.start != NONE {
                lo[a] = lo[a].min(p.start);
                hi[a] = hi[a].max(p.start + p.span as i32);
            }
        }
    }
    // An item with no column needs as many columns as it spans.
    for it in items.iter() {
        if it.place[minor].start == NONE {
            hi[minor] = hi[minor].max(lo[minor] + it.place[minor].span as i32);
        }
    }
    let count = |a: usize, hi: &[i32; 2]| ((hi[a] - lo[a]) as usize).min(MAX_TRACKS);
    let mut width = count(minor, &hi);
    let mut occ = Occ::new(width);
    let mut rows_used = 0usize;
    let at = |a: usize, line: i32| (line - lo[a]) as usize;
    let set = |it: &mut Item, occ: &mut Occ, rows_used: &mut usize, m: (usize, usize), n: (usize, usize)| {
        // Past the limit, an item goes in the last track, and a span stops at the last line (standard 9).
        let clamp = |(s, e): (usize, usize)| (s.min(MAX_TRACKS - 1), e.min(MAX_TRACKS).max(s.min(MAX_TRACKS - 1) + 1));
        let (m, n) = (clamp(m), clamp(n));
        occ.mark(m.0, m.1, n.0, n.1);
        *rows_used = (*rows_used).max(m.1);
        it.area[major] = (m.0 as u32, m.1 as u32);
        it.area[minor] = (n.0 as u32, n.1 as u32);
    };
    // Items with both places.
    for it in items.iter_mut() {
        let (m, n) = (it.place[major], it.place[minor]);
        if m.start != NONE && n.start != NONE {
            let (r0, c0) = (at(major, m.start).min(MAX_TRACKS - 1), at(minor, n.start).min(MAX_TRACKS - 1));
            set(it, &mut occ, &mut rows_used, (r0, r0 + m.span as usize), (c0, c0 + n.span as usize));
        }
    }
    // Items with a row: the first place that is free in it, after the items that this step put there before (sparse).
    let mut after: Vec<u32> = Vec::new();
    for it in items.iter_mut() {
        let (m, n) = (it.place[major], it.place[minor]);
        if m.start != NONE && n.start == NONE {
            let r0 = at(major, m.start).min(MAX_TRACKS - 1);
            let from = if dense { 0 } else { after.get(r0).copied().unwrap_or(0) as usize };
            let c0 = occ.fit(r0, r0 + m.span as usize, from, n.span as usize, usize::MAX).unwrap_or(from);
            width = width.max((c0 + n.span as usize).min(MAX_TRACKS));
            if !dense {
                if after.len() <= r0 {
                    after.resize(r0 + 1, 0);
                }
                after[r0] = (c0 + n.span as usize) as u32;
            }
            set(it, &mut occ, &mut rows_used, (r0, r0 + m.span as usize), (c0, c0 + n.span as usize));
        }
    }
    // The rest, with the cursor. In dense mode, a search starts at the row where the last item of the same shape was found.
    let (mut row, mut col) = (0usize, 0usize);
    let mut found: HashMap<(u32, u32, i32), usize> = HashMap::new();
    for it in items.iter_mut() {
        let (m, n) = (it.place[major], it.place[minor]);
        if m.start != NONE {
            continue;
        }
        let (sm, sn) = (m.span as usize, n.span as usize);
        let key = (m.span, n.span, n.start);
        if dense {
            (row, col) = (found.get(&key).copied().unwrap_or(0), 0);
        }
        let c0;
        if n.start != NONE {
            let c = at(minor, n.start).min(MAX_TRACKS - 1);
            if !dense && c < col {
                row += 1;
            }
            col = c;
            while row + sm < MAX_TRACKS && occ.fit(row, row + sm, c, sn, c + sn) != Some(c) {
                row += 1;
            }
            c0 = c;
        } else {
            loop {
                if let Some(p) = occ.fit(row, row + sm, col, sn, width) {
                    c0 = p;
                    break;
                }
                if row + sm >= MAX_TRACKS {
                    c0 = 0;
                    break;
                }
                (row, col) = (row + 1, 0);
            }
            col = c0;
        }
        let row0 = row.min(MAX_TRACKS - 1);
        if dense {
            found.insert(key, row0);
        }
        set(it, &mut occ, &mut rows_used, (row0, row0 + sm), (c0, c0 + sn));
    }
    // The tracks that the places made, and the ones that the template has.
    let mut n = [0; 2];
    n[major] = count(major, &hi).max(rows_used);
    n[minor] = count(minor, &hi).max(width);
    for it in items.iter() {
        for (len, area) in n.iter_mut().zip(it.area) {
            *len = (*len).max(area.1 as usize);
        }
    }
    (lo, n)
}

/// What the sizes of the tracks of one side are for.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// The room is known.
    Fixed(f32),
    /// The content decides: each track is as big as it wants (the max-content size of the grid).
    Max,
    /// Each track is as small as it can be (the min-content size of the grid).
    Min,
}

/// Give `extra` to the tracks that are `on`, each at most `room`, in shares as the `weight`s (standard 11.5.1: equal, or by
/// the flex factors). A track that is full does not take more, and the others go on. The gifts go in `out`.
fn fill(mut extra: f32, on: &mut [bool], room: &mut [f32], weight: &[f32], out: &mut [f32]) {
    for i in 0..on.len() {
        on[i] = on[i] && room[i] > 0.0 && weight[i] > 0.0;
    }
    while extra > 1e-4 {
        let total: f32 = (0..on.len()).filter(|&i| on[i]).map(|i| weight[i]).sum();
        if total <= 0.0 {
            return;
        }
        // The most that each unit of weight can take now: what is left shared out, or what the smallest room allows.
        let mut unit = extra / total;
        for i in (0..on.len()).filter(|&i| on[i]) {
            unit = unit.min(room[i] / weight[i]);
        }
        for i in 0..on.len() {
            if on[i] {
                let d = unit * weight[i];
                out[i] += d;
                room[i] -= d;
                on[i] = room[i] > 1e-4;
            }
        }
        extra -= unit * total;
    }
}

/// The sizes of the tracks of one side (standard 11): `defs` and the gutters before them (`lead`), and the items. `a` is the
/// side (0 rows, 1 columns). `stretch`: the `auto` tracks take the free space that is left.
struct Sizer<'a> {
    defs: &'a [Def],
    lead: &'a [f32],
    base: Vec<f32>,
    limit: Vec<f32>,
    flexible: Vec<bool>,
    flex: Vec<f32>,
    /// What the items of one span ask, until the group is done: the biggest increase of each track.
    planned: Vec<f32>,
    planned_limit: Vec<f32>,
    touched: Vec<usize>,
    // Scratch for `fill`, so that an item does not allocate.
    on: Vec<bool>,
    room: Vec<f32>,
    weight: Vec<f32>,
    gift: Vec<f32>,
}

impl<'a> Sizer<'a> {
    fn new(defs: &'a [Def], lead: &'a [f32]) -> Sizer<'a> {
        let n = defs.len();
        let mut s = Sizer { defs, lead, base: vec![0.0; n], limit: vec![INF; n], flexible: vec![false; n], flex: vec![0.0; n], planned: vec![0.0; n], planned_limit: vec![0.0; n], touched: Vec::new(), on: Vec::new(), room: Vec::new(), weight: Vec::new(), gift: Vec::new() };
        for (t, d) in defs.iter().enumerate() {
            if let Size::Px(x) = d.min {
                s.base[t] = x;
            }
            match d.max {
                Size::Px(x) => s.limit[t] = x.max(s.base[t]),
                Size::Fr(f) => (s.flexible[t], s.flex[t]) = (true, f),
                _ => {}
            }
        }
        s
    }

    /// The room that the tracks `s..e` and the gutters between them take now.
    fn span(&self, s: usize, e: usize, of: impl Fn(usize) -> f32) -> f32 {
        (s..e).map(of).sum::<f32>() + self.lead[s + 1..e].iter().sum::<f32>()
    }

    /// Let the item that covers `s..e` ask for `want` pixels in the base sizes of the tracks that `affected` says yes to. The
    /// extra room goes up to the growth limits first, and what is left goes on (standard 11.5.1). `by_flex`: share by flex factor.
    fn grow_base(&mut self, s: usize, e: usize, want: f32, affected: impl Fn(&Sizer, usize) -> bool, by_flex: bool) {
        let extra = want - self.span(s, e, |t| self.base[t]);
        if extra <= 0.0 || !(s..e).any(|t| affected(self, t)) {
            return;
        }
        let k = e - s;
        let (mut on, mut room, mut weight, mut gift) = (std::mem::take(&mut self.on), std::mem::take(&mut self.room), std::mem::take(&mut self.weight), std::mem::take(&mut self.gift));
        on.clear();
        on.extend((s..e).map(|t| affected(self, t)));
        let fixed_max: Vec<bool> = (s..e).map(|t| matches!(self.defs[t].max, Size::Px(_))).collect();
        room.clear();
        room.extend((s..e).map(|t| self.limit[t] - self.base[t]));
        weight.clear();
        weight.extend((s..e).map(|t| if by_flex { self.flex[t] } else { 1.0 }));
        gift.clear();
        gift.resize(k, 0.0);
        let mut again = on.clone();
        fill(extra, &mut on, &mut room, &weight, &mut gift);
        let left = extra - gift.iter().sum::<f32>();
        if left > 1e-4 {
            // Beyond the limits: only the tracks that have no fixed maximum.
            for i in 0..k {
                again[i] = again[i] && !fixed_max[i];
            }
            room.clear();
            room.resize(k, INF);
            let mut more = vec![0.0; k];
            fill(left, &mut again, &mut room, &weight, &mut more);
            for (g, m) in gift.iter_mut().zip(more) {
                *g += m;
            }
        }
        for (i, t) in (s..e).enumerate() {
            if gift[i] > 0.0 {
                if self.planned[t] == 0.0 && self.planned_limit[t] == 0.0 {
                    self.touched.push(t);
                }
                self.planned[t] = self.planned[t].max(gift[i]);
            }
        }
        (self.on, self.room, self.weight, self.gift) = (on, room, weight, gift);
    }

    /// The same for the growth limits: the item asks `want` pixels from the tracks that `affected` says yes to.
    fn grow_limit(&mut self, s: usize, e: usize, want: f32, affected: impl Fn(&Sizer, usize) -> bool) {
        let now = |z: &Sizer, t: usize| if z.limit[t].is_infinite() { z.base[t] } else { z.limit[t] };
        let extra = want - self.span(s, e, |t| now(self, t));
        if extra <= 0.0 || !(s..e).any(|t| affected(self, t)) {
            return;
        }
        let k = e - s;
        let (mut on, mut room, mut weight, mut gift) = (std::mem::take(&mut self.on), std::mem::take(&mut self.room), std::mem::take(&mut self.weight), std::mem::take(&mut self.gift));
        on.clear();
        on.extend((s..e).map(|t| affected(self, t)));
        // `fit-content(L)` stops at L.
        room.clear();
        room.extend((s..e).map(|t| if let Size::Fit(l) = self.defs[t].max { (l - now(self, t)).max(0.0) } else { INF }));
        weight.clear();
        weight.resize(k, 1.0);
        gift.clear();
        gift.resize(k, 0.0);
        fill(extra, &mut on, &mut room, &weight, &mut gift);
        for (i, t) in (s..e).enumerate() {
            if gift[i] > 0.0 {
                if self.planned[t] == 0.0 && self.planned_limit[t] == 0.0 {
                    self.touched.push(t);
                }
                self.planned_limit[t] = self.planned_limit[t].max(gift[i]);
            }
        }
        (self.on, self.room, self.weight, self.gift) = (on, room, weight, gift);
    }

    /// The end of a group of items: what they asked goes in the sizes.
    fn apply(&mut self) {
        for t in std::mem::take(&mut self.touched) {
            self.base[t] += std::mem::take(&mut self.planned[t]);
            if self.planned_limit[t] > 0.0 {
                let from = if self.limit[t].is_infinite() { self.base[t] } else { self.limit[t] };
                self.limit[t] = from + std::mem::take(&mut self.planned_limit[t]);
            }
            self.limit[t] = self.limit[t].max(self.base[t]);
        }
    }

    /// An item of one track: its contributions set the base size and the growth limit of the track directly.
    fn single(&mut self, t: usize, c: [f32; 3], mode: Mode) {
        let d = self.defs[t];
        // With the room known the minimum counts. With none, the min-content size, but no more than a fixed maximum.
        let least = match (mode, d.max) {
            (Mode::Fixed(_), _) => c[LEAST],
            (_, Size::Px(x)) => c[MIN].min(x),
            _ => c[MIN],
        };
        match d.min {
            Size::Auto => self.base[t] = self.base[t].max(least),
            Size::MinContent => self.base[t] = self.base[t].max(c[MIN]),
            Size::MaxContent => self.base[t] = self.base[t].max(c[MAX]),
            _ => {}
        }
        let want = match d.max {
            Size::MinContent => Some(c[MIN]),
            Size::Auto | Size::MaxContent => Some(c[MAX]),
            Size::Fit(l) => Some(c[MAX].min(l)),
            _ => None,
        };
        if let Some(w) = want {
            self.limit[t] = if self.limit[t].is_infinite() { w } else { self.limit[t].max(w) };
        }
        self.limit[t] = self.limit[t].max(self.base[t]);
    }

    /// The size of a flexible fr from `space` for the tracks `tracks` (standard 11.7.1). A track that would be smaller than its
    /// base size is not flexible: it keeps the base size, and the others share the rest.
    fn fraction(&self, tracks: impl Iterator<Item = usize>, mut space: f32) -> f32 {
        let mut active: Vec<usize> = tracks.collect();
        loop {
            let sum: f32 = active.iter().map(|&t| self.flex[t]).sum();
            let fr = space / sum.max(1.0);
            let before = active.len();
            active.retain(|&t| {
                let keep = fr * self.flex[t] >= self.base[t];
                if !keep {
                    space -= self.base[t];
                }
                keep
            });
            if active.is_empty() {
                return 0.0;
            }
            if active.len() == before {
                return fr;
            }
        }
    }
}

/// The sizes of the tracks of one side. `items` have their contributions for side `a`.
fn size_tracks(defs: &[Def], lead: &[f32], items: &[Item], a: usize, mode: Mode, stretch: bool) -> Vec<f32> {
    let n = defs.len();
    if n == 0 {
        return Vec::new();
    }
    let mut z = Sizer::new(defs, lead);
    // How many flexible tracks are before each place, to find the items that cross one.
    let mut flexible_before = Vec::with_capacity(n + 1);
    let mut count = 0u32;
    for t in 0..n {
        flexible_before.push(count);
        count += z.flexible[t] as u32;
    }
    flexible_before.push(count);
    let any_flexible = count > 0;
    let crosses = |it: &Item| flexible_before[it.area[a].1 as usize] != flexible_before[it.area[a].0 as usize];

    // 1. The items of one track that is not flexible.
    let mut multi: Vec<u32> = Vec::new();
    let mut flexing: Vec<u32> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let (s, e) = (it.area[a].0 as usize, it.area[a].1 as usize);
        if crosses(it) {
            flexing.push(i as u32);
        } else if e - s == 1 {
            z.single(s, it.contrib[a], mode);
        } else {
            multi.push(i as u32);
        }
    }
    // 2. The items that cover more tracks, the small ones first, and each size of item together.
    multi.sort_by_key(|&i| items[i as usize].area[a].1 - items[i as usize].area[a].0);
    let mut at = 0;
    while at < multi.len() {
        let span = |i: u32| items[i as usize].area[a].1 - items[i as usize].area[a].0;
        let group_end = at + multi[at..].iter().take_while(|&&i| span(i) == span(multi[at])).count();
        let group = &multi[at..group_end];
        at = group_end;
        for &i in group {
            let it = &items[i as usize];
            let (s, e, c) = (it.area[a].0 as usize, it.area[a].1 as usize, it.contrib[a]);
            z.grow_base(s, e, if matches!(mode, Mode::Fixed(_)) { c[LEAST] } else { c[MIN] }, |z, t| matches!(z.defs[t].min, Size::Auto), false);
            z.grow_base(s, e, c[MIN], |z, t| matches!(z.defs[t].min, Size::MinContent | Size::MaxContent), false);
            z.grow_base(s, e, c[MAX], |z, t| matches!(z.defs[t].min, Size::MaxContent), false);
        }
        z.apply();
        for &i in group {
            let it = &items[i as usize];
            let (s, e, c) = (it.area[a].0 as usize, it.area[a].1 as usize, it.contrib[a]);
            z.grow_limit(s, e, c[MIN], |z, t| matches!(z.defs[t].max, Size::MinContent));
            z.grow_limit(s, e, c[MAX], |z, t| matches!(z.defs[t].max, Size::Auto | Size::MaxContent | Size::Fit(_)));
        }
        z.apply();
    }
    // 3. The items that cover a flexible track, all together. Only the flexible tracks grow, by their flex factors.
    for &i in &flexing {
        let it = &items[i as usize];
        let (s, e, c) = (it.area[a].0 as usize, it.area[a].1 as usize, it.contrib[a]);
        // An item over more tracks, with a flexible one, has no automatic minimum (standard 6.6).
        let least = if e - s > 1 { 0.0 } else { c[LEAST] };
        z.grow_base(s, e, if matches!(mode, Mode::Fixed(_)) { least } else { c[MIN] }, |z, t| z.flexible[t] && matches!(z.defs[t].min, Size::Auto), true);
        z.grow_base(s, e, c[MIN], |z, t| z.flexible[t] && matches!(z.defs[t].min, Size::MinContent | Size::MaxContent), true);
        z.grow_base(s, e, c[MAX], |z, t| z.flexible[t] && matches!(z.defs[t].min, Size::MaxContent), true);
    }
    z.apply();
    // A limit that nothing set is the base size.
    for t in 0..n {
        if z.limit[t].is_infinite() || z.flexible[t] {
            z.limit[t] = z.base[t];
        }
        z.limit[t] = z.limit[t].max(z.base[t]);
    }
    let gutters: f32 = lead.iter().sum();

    // 4. Maximize: the free room goes to the tracks, up to their growth limits.
    match mode {
        Mode::Fixed(space) => {
            let free = space - z.base.iter().sum::<f32>() - gutters;
            if free > 0.0 {
                z.on.clear();
                z.on.resize(n, true);
                z.room = (0..n).map(|t| z.limit[t] - z.base[t]).collect();
                z.weight.clear();
                z.weight.resize(n, 1.0);
                let mut gift = vec![0.0; n];
                fill(free, &mut z.on, &mut z.room, &z.weight, &mut gift);
                for (b, g) in z.base.iter_mut().zip(gift) {
                    *b += g;
                }
            }
        }
        Mode::Max => z.base.clone_from(&z.limit),
        Mode::Min => {}
    }

    // 5. The flexible tracks (standard 11.7).
    let mut size = z.base.clone();
    if any_flexible {
        let fr = match mode {
            Mode::Fixed(space) => {
                let others: f32 = (0..n).filter(|&t| !z.flexible[t]).map(|t| z.base[t]).sum();
                z.fraction((0..n).filter(|&t| z.flexible[t]), space - others - gutters)
            }
            Mode::Max => {
                // The fr that each flexible track needs for its base size, and that each item needs for its max-content size.
                let mut fr = (0..n).filter(|&t| z.flexible[t]).map(|t| z.base[t] / z.flex[t].max(1.0)).fold(0.0f32, f32::max);
                for &i in &flexing {
                    let it = &items[i as usize];
                    let (s, e) = (it.area[a].0 as usize, it.area[a].1 as usize);
                    let others: f32 = (s..e).filter(|&t| !z.flexible[t]).map(|t| z.base[t]).sum();
                    let room = it.contrib[a][MAX] - others - lead[s + 1..e].iter().sum::<f32>();
                    fr = fr.max(z.fraction((s..e).filter(|&t| z.flexible[t]), room));
                }
                fr
            }
            Mode::Min => 0.0,
        };
        for (t, size) in size.iter_mut().enumerate() {
            if z.flexible[t] {
                *size = z.base[t].max(fr * z.flex[t]);
            }
        }
    }

    // 6. The `auto` tracks take what is left.
    if let (Mode::Fixed(space), true) = (mode, stretch) {
        let free = space - size.iter().sum::<f32>() - gutters;
        let autos: Vec<usize> = (0..n).filter(|&t| defs[t].max == Size::Auto).collect();
        if free > 0.0 && !autos.is_empty() {
            let share = free / autos.len() as f32;
            for t in autos {
                size[t] += share;
            }
        }
    }
    size
}

/// The tracks that an item needs measured: a track that a fixed size decides has no use for the contributions.
fn needs(defs: &[Def], it: &Item, a: usize, fixed: bool) -> bool {
    (it.area[a].0 as usize..it.area[a].1 as usize).any(|t| match (defs[t].min, defs[t].max) {
        (Size::Px(_), Size::Px(_)) => false,
        (Size::Px(_), Size::Fr(_)) => !fixed,
        _ => true,
    })
}

/// Everything that the layout reads from the styles of the container and from its children.
struct Setup {
    items: Vec<Item>,
    /// The absolute children that no run of text holds: they have a static position and no item.
    statics: Vec<NodeId>,
    /// The tracks of each side (0 rows, 1 columns), with the implicit ones.
    defs: [Vec<Def>; 2],
    /// The gutter before each track. It is 0 for a track that collapsed, and for the first one that shows.
    lead: [Vec<f32>; 2],
    /// A track of `auto-fit` that holds no item. It has no size and no gutter.
    collapsed: [Vec<bool>; 2],
}

static AUTO_LINE: Line = Line::Auto;

/// The `Line` of a placement property.
fn line_of<'a>(cx: &Cx<'a>, node: NodeId, p: Property) -> &'a Line {
    let tree = cx.tree;
    match tree.style(node).get(p) {
        Value::Grid(i) => match tree.styles.grid(i) {
            Grid::Line(l) => l,
            _ => &AUTO_LINE,
        },
        _ => &AUTO_LINE,
    }
}

/// The items and the tracks of the container. `avail`: the content box that is known (rows, columns).
fn setup(cx: &mut Cx, node: NodeId, avail: [Option<f32>; 2]) -> Setup {
    let tree = cx.tree;
    let style = tree.style(node);
    let gap = |p, base| match style.get(p) {
        Value::Length(_) => resolve(cx, style.get(p), base).unwrap_or(0.0).clamp(0.0, MAX_PX),
        _ => 0.0,
    };
    let gaps = [gap(Property::RowGap, avail[0]), gap(Property::ColumnGap, avail[1])];
    let parts = |p: Property| match style.get(p) {
        Value::Grid(i) => match tree.styles.grid(i) {
            Grid::Tracks(parts) => parts.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    let templates = [expand(parts(Property::GridTemplateRows), avail[0], gaps[0]), expand(parts(Property::GridTemplateColumns), avail[1], gaps[1])];
    let areas = match style.get(Property::GridTemplateAreas) {
        Value::Grid(i) => match tree.styles.grid(i) {
            Grid::Areas(a) => Some(a),
            _ => None,
        },
        _ => None,
    };
    let explicit = [templates[0].defs.len().max(areas.map_or(0, |a| a.rows as usize)), templates[1].defs.len().max(areas.map_or(0, |a| a.columns as usize))];
    let lines = [Lines { explicit: explicit[0] as i32, names: &templates[0].names, areas }, Lines { explicit: explicit[1] as i32, names: &templates[1].names, areas }];
    let autos: [Vec<Def>; 2] = [Property::GridAutoRows, Property::GridAutoColumns].map(|p| {
        let a = usize::from(p == Property::GridAutoColumns);
        let mut v: Vec<Def> = parts(p).iter().filter_map(|part| if let Part::Track(t) = part { Some(def(t, avail[a])) } else { None }).collect();
        if v.is_empty() {
            v.push(AUTO);
        }
        v
    });

    let (mut items, statics) = collect(cx, node, &lines);
    if items.iter().any(|i| i.order != 0) {
        items.sort_by_key(|i| i.order);
    }
    let Value::Number(flow) = style.get(Property::GridAutoFlow) else { unreachable!("grid-auto-flow is a number") };
    let flow = flow as u32;
    let (lo, n) = place(&mut items, explicit, usize::from(flow & FLOW_COLUMN as u32 != 0), flow & FLOW_DENSE as u32 != 0);

    let mut defs: [Vec<Def>; 2] = [Vec::new(), Vec::new()];
    let mut lead: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
    let mut collapsed: [Vec<bool>; 2] = [Vec::new(), Vec::new()];
    for a in 0..2 {
        let off = (1 - lo[a]) as usize;
        let t = &templates[a];
        let auto = &autos[a];
        defs[a] = (0..n[a])
            .map(|i| {
                if i < off {
                    auto[auto.len() - 1 - (off - 1 - i) % auto.len()]
                } else if i - off < t.defs.len() {
                    t.defs[i - off]
                } else {
                    auto[(i - off - t.defs.len()) % auto.len()]
                }
            })
            .collect();
        collapsed[a] = vec![false; n[a]];
        if !t.fit.is_empty() {
            // A difference list: how many items start and end at each track.
            let mut cover = vec![0i32; n[a] + 1];
            for it in &items {
                cover[it.area[a].0 as usize] += 1;
                cover[it.area[a].1 as usize] -= 1;
            }
            let mut now = 0;
            for (i, c) in collapsed[a].iter_mut().enumerate() {
                now += cover[i];
                *c = now == 0 && t.fit.contains(&(i.wrapping_sub(off)));
                if *c {
                    defs[a][i] = Def { min: Size::Px(0.0), max: Size::Px(0.0) };
                }
            }
        }
        let mut seen = false;
        lead[a] = (0..n[a])
            .map(|i| {
                if collapsed[a][i] {
                    return 0.0;
                }
                let g = if seen { gaps[a] } else { 0.0 };
                seen = true;
                g
            })
            .collect();
    }
    Setup { items, statics, defs, lead, collapsed }
}

/// The items of the container, in the order of the document, and the absolute children that are not in a run of text.
fn collect(cx: &Cx, node: NodeId, lines: &[Lines; 2]) -> (Vec<Item>, Vec<NodeId>) {
    let tree = cx.tree;
    let mut counter = tree.counter(node);
    let (mut items, mut statics) = (Vec::new(), Vec::new());
    let auto = Place { start: NONE, span: 1 };
    for child in tree.children(node) {
        match child {
            Child::Block(b) => {
                let number = if tree.kind(b) == Kind::ListItem { tree.list_number(&mut counter, b) } else { 0 };
                let get = |p| line_of(cx, b, p);
                let place = [lines[0].place(get(Property::GridRowStart), get(Property::GridRowEnd), 0), lines[1].place(get(Property::GridColumnStart), get(Property::GridColumnEnd), 1)];
                let order = match tree.style(b).get(Property::Order) {
                    Value::Number(n) => n.clamp(-1e9, 1e9) as i32,
                    _ => 0,
                };
                items.push(Item { node: b, run: None, order, number, place, area: [(0, 0); 2], contrib: [[0.0; 3]; 2] });
            }
            Child::Lines(first, last) => {
                // A run of text is an item, unless it is only spaces that go away. Its absolute boxes go on the lines.
                let (mut real, mut at) = (false, Some(first));
                while let Some(n) = at {
                    real |= tree.kind(n) == Kind::Text && !tree.is_collapsible_space(n);
                    at = if n == last { None } else { tree.doc.next_sibling(n) };
                }
                if real {
                    items.push(Item { node, run: Some((first, last)), order: 0, number: 0, place: [auto; 2], area: [(0, 0); 2], contrib: [[0.0; 3]; 2] });
                } else {
                    let mut at = Some(first);
                    while let Some(n) = at {
                        if tree.kind(n) == Kind::OutOfFlow {
                            statics.push(n);
                        }
                        at = if n == last { None } else { tree.doc.next_sibling(n) };
                    }
                }
            }
        }
    }
    (items, statics)
}

/// The width of the border box of an item and its margins, in an area `area_w` wide: (content width, left margin, right margin,
/// the shift of the box inside the area that the alignment gives). An item stretches if it has no width and no auto margin.
fn fit_width(cx: &mut Cx, node: NodeId, area_w: f32, how: Align) -> (f32, f32, f32, f32) {
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, area_w);
    let bp = border.horizontal() + padding.horizontal();
    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
    let margin = |cx: &Cx, p| resolve(cx, style.get(p), Some(area_w));
    let (ml, mr) = (margin(cx, Property::MarginLeft), margin(cx, Property::MarginRight));
    let (l0, r0) = (ml.unwrap_or(0.0), mr.unwrap_or(0.0));
    let fixed = resolve(cx, style.get(Property::Width), Some(area_w)).is_some();
    let w = if fixed || (how == Align::Stretch && ml.is_some() && mr.is_some()) {
        widths(cx, style, area_w, bp).0
    } else {
        // Fit the content: its max-content width, but between its min-content width and the room.
        let (min, max) = intrinsic::sizes(cx, node);
        let mut w = max.min(min.max((area_w - l0 - r0 - bp).max(0.0)));
        if let Some(hi) = resolve(cx, style.get(Property::MaxWidth), Some(area_w)).map(content) {
            w = w.min(hi);
        }
        if let Some(lo) = resolve(cx, style.get(Property::MinWidth), Some(area_w)).map(content) {
            w = w.max(lo);
        }
        w
    };
    let free = area_w - w - bp - l0 - r0;
    let (left, shift) = match (ml, mr) {
        (None, None) => (free.max(0.0) / 2.0, 0.0),
        (None, Some(_)) => (free.max(0.0), 0.0),
        (Some(_), None) => (l0, 0.0),
        (Some(_), Some(_)) => (l0, free * shift_of(how)),
    };
    (w, left, area_w - w - bp - left - shift, shift)
}

/// How much of the free room goes before the box.
fn shift_of(how: Align) -> f32 {
    match how {
        Align::Stretch | Align::Start => 0.0,
        Align::Center => 0.5,
        Align::End => 1.0,
    }
}

/// The contributions of an item to the columns: min-content, max-content and the minimum (standard 6.6), as widths of the margin box.
fn column_contrib(cx: &mut Cx, it: &Item) -> [f32; 3] {
    if let Some((first, last)) = it.run {
        let (min, max) = intrinsic::lines(cx, first, last);
        return [min, max, min];
    }
    let (min, max) = intrinsic::outer(cx, it.node);
    let style = cx.tree.style(it.node);
    let (border, padding) = border_padding(cx, style, 0.0);
    let bp = border.horizontal() + padding.horizontal();
    let margins = |cx: &Cx, p| resolve(cx, style.get(p), Some(0.0)).unwrap_or(0.0);
    let extras = bp + margins(cx, Property::MarginLeft) + margins(cx, Property::MarginRight);
    let least = if cx.tree.scrolls(it.node) {
        extras
    } else {
        match style.get(Property::MinWidth) {
            Value::Keyword(Keyword::Auto) => min,
            v => {
                let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
                resolve(cx, v, None).map_or(0.0, |lo| if border_box { (lo - bp).max(0.0) } else { lo }) + extras
            }
        }
    };
    [min, max, least]
}

/// The start of each track, from the content box, when the free room `free` is shared by `how` (`justify-content`).
fn positions(sizes: &[f32], lead: &[f32], collapsed: &[bool], free: f32, how: Value) -> Vec<f32> {
    let k = collapsed.iter().filter(|c| !**c).count() as f32;
    let (mut at, between) = match how {
        Value::Keyword(Keyword::End | Keyword::FlexEnd | Keyword::Right) => (free, 0.0),
        Value::Keyword(Keyword::Center) => (free / 2.0, 0.0),
        Value::Keyword(Keyword::SpaceBetween) if free > 0.0 && k > 1.0 => (0.0, free / (k - 1.0)),
        Value::Keyword(Keyword::SpaceAround) if free > 0.0 && k > 0.0 => (free / k / 2.0, free / k),
        Value::Keyword(Keyword::SpaceEvenly) if free > 0.0 && k > 0.0 => (free / (k + 1.0), free / (k + 1.0)),
        _ => (0.0, 0.0),
    };
    let mut first = true;
    let mut out = Vec::with_capacity(sizes.len());
    for t in 0..sizes.len() {
        if !collapsed[t] {
            if !first {
                at += between;
            }
            first = false;
        }
        at += lead[t];
        out.push(at);
        at += sizes[t];
    }
    out
}

/// What the first layout of an item leaves.
#[derive(Clone, Copy, Default)]
struct Lay {
    /// The fragments of the item: from `frag` to `end`, in the temporary list.
    frag: usize,
    end: u32,
    /// How many static positions inside are still waiting for a containing block.
    waited: u32,
    /// The border box height, the margins on top and at the bottom.
    h: f32,
    mt: f32,
    mb: f32,
    base: Option<f32>,
    /// The border box, from the content box of the container: left edge, and the width of the area.
    x: f32,
    area_w: f32,
    /// What `block::sized` was given for the width of the content and the margins.
    w: f32,
    ml: f32,
    mr: f32,
}

fn run_height(rows: &[f32]) -> f32 {
    rows.iter().sum()
}

/// Move the top fragments of the range `from..end` by `dx`, `dy`.
fn shift_range(out: &mut [Fragment], from: usize, end: usize, dx: f32, dy: f32) {
    let mut i = from;
    while i < end {
        out[i].rect.x += dx;
        out[i].rect.y += dy;
        i = out[i].end as usize;
    }
}

/// Lay out the content of a grid container in `space` (its content box). The fragments are relative to the border box
/// of the container: the content box starts at `bp.left`, `bp.top`.
pub(crate) fn layout(cx: &mut Cx, node: NodeId, space: Space, bp: Sides) -> Kids {
    let tree = cx.tree;
    let style = tree.style(node);
    let mut g = setup(cx, node, [space.height, Some(space.width)]);
    let (justify, align_content) = (style.get(Property::JustifyContent), style.get(Property::AlignContent));
    let stretches = |v: Value| matches!(v, Value::Keyword(Keyword::Normal | Keyword::Stretch));
    let (justify_items, align_items) = (align(style.get(Property::JustifyItems), Align::Stretch), align(style.get(Property::AlignItems), Align::Stretch));

    // 1. The columns.
    for it in g.items.iter_mut() {
        if needs(&g.defs[1], it, 1, true) {
            it.contrib[1] = column_contrib(cx, it);
        }
    }
    let cols = size_tracks(&g.defs[1], &g.lead[1], &g.items, 1, Mode::Fixed(space.width), stretches(justify));
    let free = space.width - cols.iter().sum::<f32>() - g.lead[1].iter().sum::<f32>();
    let col_at = positions(&cols, &g.lead[1], &g.collapsed[1], free, justify);

    // 2. Each item at the width of its columns, in a list of its own. The height of the rows is not known yet.
    let mut tmp = cx.spare.pop().unwrap_or_default();
    tmp.clear();
    let real = std::mem::replace(&mut cx.out, tmp);
    let mut lays = vec![Lay::default(); g.items.len()];
    for (it, l) in g.items.iter_mut().zip(lays.iter_mut()) {
        let (s, e) = (it.area[1].0 as usize, it.area[1].1 as usize);
        let area_w = col_at[e - 1] + cols[e - 1] - col_at[s];
        let before = cx.waiting;
        l.area_w = area_w;
        l.x = col_at[s];
        l.frag = cx.out.len();
        if let Some((first, last)) = it.run {
            let lines = inline::lines(cx, node, first, last, area_w, (0.0, 0.0));
            l.h = lines.height;
            l.base = lines.baseline.filter(|_| lines.any);
        } else {
            if tree.kind(it.node) == Kind::ListItem {
                cx.number = it.number;
            }
            let how = align(tree.style(it.node).get(Property::JustifySelf), justify_items);
            let (w, ml, mr, shift) = fit_width(cx, it.node, area_w, how);
            let done = block::sized(cx, it.node, Space { width: area_w, height: None }, Some((w, ml, mr)), None, Pos::default());
            (l.h, l.mt, l.mb, l.base, l.w, l.ml, l.mr) = (done.height, done.margin_top, done.margin_bottom, done.baseline, w, ml, mr);
            l.x += ml + shift;
        }
        l.end = cx.out.len() as u32;
        l.waited = cx.waiting - before;
        let h = l.h + l.mt + l.mb;
        // An item that scrolls has no automatic minimum height.
        let least = if it.run.is_none() && matches!(tree.overflow(it.node).1, Keyword::Hidden | Keyword::Scroll | Keyword::Auto) { 0.0 } else { h };
        it.contrib[0] = [h, h, least];
    }

    // 3. The rows. With no height, each row is as high as its items; a `min-height` that is more makes the room known.
    let mut room = space.height;
    let stretch_rows = stretches(align_content);
    let gutters: f32 = g.lead[0].iter().sum();
    let mut rows = match room {
        Some(h) => size_tracks(&g.defs[0], &g.lead[0], &g.items, 0, Mode::Fixed(h), stretch_rows),
        None => size_tracks(&g.defs[0], &g.lead[0], &g.items, 0, Mode::Max, false),
    };
    if room.is_none() {
        let min = resolve(cx, style.get(Property::MinHeight), None).map(|m| if style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox) { (m - bp.vertical()).max(0.0) } else { m });
        if let Some(min) = min.filter(|&m| m > run_height(&rows) + gutters) {
            room = Some(min);
            rows = size_tracks(&g.defs[0], &g.lead[0], &g.items, 0, Mode::Fixed(min), stretch_rows);
        }
    }
    let total = run_height(&rows) + gutters;
    let free = room.map_or(0.0, |h| h - total);
    let row_at = positions(&rows, &g.lead[0], &g.collapsed[0], free, align_content);

    // 4. Each item in its area. An item that stretches, and has boxes inside, is laid out again at its height.
    let mut baseline: Option<(u32, f32)> = None;
    for (it, l) in g.items.iter().zip(lays.iter_mut()) {
        let (r0, r1) = (it.area[0].0 as usize, it.area[0].1 as usize);
        let y0 = row_at[r0];
        let area_h = row_at[r1 - 1] + rows[r1 - 1] - y0;
        let mut y = y0;
        if it.run.is_none() {
            let style = tree.style(it.node);
            let how = align(style.get(Property::AlignSelf), align_items);
            let (border, padding) = border_padding(cx, style, l.area_w);
            let bpv = border.vertical() + padding.vertical();
            let margin = |cx: &Cx, p| resolve(cx, style.get(p), Some(l.area_w));
            let (mt, mb) = (margin(cx, Property::MarginTop), margin(cx, Property::MarginBottom));
            let (t0, b0) = (mt.unwrap_or(0.0), mb.unwrap_or(0.0));
            let height = style.get(Property::Height);
            let stretch = how == Align::Stretch && height == Value::Keyword(Keyword::Auto) && mt.is_some() && mb.is_some();
            let percent = matches!(height, Value::Length(Length::Percent(_) | Length::Calc(_)));
            let target = (area_h - t0 - b0 - bpv).max(0.0);
            let again = if stretch && (area_h - t0 - b0 - l.h).abs() > 0.01 {
                if tree.doc.children(it.node).any(|c| matches!(tree.doc.data(c), bare_html::dom::NodeData::Element { .. })) {
                    Some(Some(target))
                } else {
                    // Only text inside: the box gets taller, and its lines stay.
                    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
                    let content = |v: f32| if border_box { (v - bpv).max(0.0) } else { v };
                    let lo = resolve(cx, style.get(Property::MinHeight), Some(area_h)).map_or(0.0, content);
                    let hi = resolve(cx, style.get(Property::MaxHeight), Some(area_h)).map(content);
                    l.h = hi.map_or(target, |m| target.min(m)).max(lo) + bpv;
                    cx.out[l.frag].rect.h = l.h;
                    None
                }
            } else if percent {
                Some(None)
            } else {
                None
            };
            if let Some(tall) = again {
                cx.waiting -= l.waited;
                if tree.kind(it.node) == Kind::ListItem {
                    cx.number = it.number;
                }
                let before = cx.waiting;
                let done = block::sized(cx, it.node, Space { width: l.area_w, height: Some(area_h) }, Some((l.w, l.ml, l.mr)), tall, Pos::default());
                (l.frag, l.end, l.waited, l.h, l.base) = (done.fragment, cx.out.len() as u32, cx.waiting - before, done.height, done.baseline);
            }
            let free = area_h - l.h - t0 - b0;
            let (top, shift) = match (mt, mb) {
                (None, None) => (free.max(0.0) / 2.0, 0.0),
                (None, Some(_)) => (free.max(0.0), 0.0),
                (Some(_), None) => (t0, 0.0),
                (Some(_), Some(_)) => (t0, free * shift_of(how)),
            };
            y = y0 + top + shift;
            cx.out[l.frag].rect.x = bp.left + l.x;
            cx.out[l.frag].rect.y = bp.top + y;
        } else {
            shift_range(&mut cx.out, l.frag, l.end as usize, bp.left + l.x, bp.top + y);
        }
        if let Some(b) = l.base
            && baseline.is_none_or(|(r, _)| (r0 as u32) < r)
        {
            baseline = Some((r0 as u32, y + b));
        }
    }

    // 5. The live fragments go to the page, in the order of the items. The first layout of an item that was laid out again is left.
    let tmp = std::mem::replace(&mut cx.out, real);
    for l in &lays {
        let base = cx.out.len() as u32;
        cx.out.extend(tmp[l.frag..l.end as usize].iter().map(|f| Fragment { end: f.end - l.frag as u32 + base, ..*f }));
    }
    let mut tmp = tmp;
    tmp.clear();
    cx.spare.push(tmp);

    // 6. The absolute children that are no part of a run of text: at the start of the content box.
    for &n in &g.statics {
        cx.fixed |= tree.position(n) == Keyword::Fixed;
        cx.waiting += 1;
        let at = cx.out.len() as u32;
        cx.out.push(Fragment { rect: crate::fragment::Rect { x: bp.left, y: bp.top, w: 0.0, h: 0.0 }, node: n, kind: FragmentKind::Static, end: at + 1 });
    }
    Kids::of(total, baseline.map(|(_, b)| b))
}

/// The min-content and max-content widths of the content box of a grid container.
pub(crate) fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    let mut g = setup(cx, node, [None, None]);
    for it in g.items.iter_mut() {
        if needs(&g.defs[1], it, 1, false) {
            it.contrib[1] = column_contrib(cx, it);
        }
    }
    let gutters: f32 = g.lead[1].iter().sum();
    let total = |mode| size_tracks(&g.defs[1], &g.lead[1], &g.items, 1, mode, false).iter().sum::<f32>() + gutters;
    (total(Mode::Min), total(Mode::Max))
}

#[cfg(test)]
mod tests {
    use crate::block::tests::{dump, rect};
    use crate::fragment::Rect;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A grid with `n` boxes, ids a, b, c, ...
    fn boxes(n: usize, extra: &str) -> String {
        (0..n).map(|i| format!("<div id={}{extra}></div>", (b'a' + i as u8) as char)).collect()
    }

    fn grid(css: &str, html: &str, id: &str) -> Rect {
        rect(&format!("<div id=g>{html}</div>"), &format!("#g {{ display: grid; {css} }}"), id)
    }

    #[test]
    fn fixed_and_fr_columns() {
        let html = boxes(3, "");
        assert_eq!(grid("grid-template-columns: 100px 1fr 2fr", &html, "a"), r(8.0, 8.0, 100.0, 0.0));
        assert_eq!(grid("grid-template-columns: 100px 1fr 2fr", &html, "b"), r(108.0, 8.0, 228.0, 0.0));
    }

    #[test]
    fn gaps_and_rows() {
        let html = boxes(4, " style='height: 10px'");
        let css = "grid-template-columns: 50px 50px; gap: 5px 7px";
        assert_eq!(grid(css, &html, "d"), r(65.0, 23.0, 50.0, 10.0));
    }

    #[test]
    fn explicit_places_and_spans() {
        let html = "<div id=a style='grid-column: 2 / span 2; grid-row: 2'></div>";
        let css = "grid-template-columns: repeat(3, 40px); grid-template-rows: 10px 20px";
        assert_eq!(grid(css, html, "a"), r(48.0, 18.0, 80.0, 20.0));
        let html = "<div id=a style='grid-column: -2 / -1'></div>";
        assert_eq!(grid(css, html, "a"), r(88.0, 8.0, 40.0, 10.0));
    }

    #[test]
    fn auto_placement_sparse_and_dense() {
        let html = "<div id=a style='grid-column: span 2'></div><div id=b style='grid-column: span 2'></div><div id=c></div>";
        let sparse = "grid-template-columns: repeat(3, 10px); grid-auto-rows: 10px";
        assert_eq!(grid(sparse, html, "c"), r(28.0, 18.0, 10.0, 10.0));
        let dense = "grid-template-columns: repeat(3, 10px); grid-auto-rows: 10px; grid-auto-flow: dense";
        assert_eq!(grid(dense, html, "c"), r(28.0, 8.0, 10.0, 10.0));
    }

    #[test]
    fn an_item_past_the_last_track_goes_in_the_last_track() {
        let html = format!("{}<div id=z style='grid-row: 1'></div>", "<div style='grid-row: 1'></div>".repeat(super::MAX_TRACKS + 1));
        let css = "grid-auto-columns: 1px; grid-auto-rows: 1px";
        assert_eq!(grid(css, &html, "z"), r(8.0 + (super::MAX_TRACKS - 1) as f32, 8.0, 1.0, 1.0));
    }

    #[test]
    fn the_flow_can_be_column() {
        let css = "grid-template-rows: repeat(2, 10px); grid-auto-columns: 30px; grid-auto-flow: column";
        assert_eq!(grid(css, &boxes(3, ""), "c"), r(38.0, 8.0, 30.0, 10.0));
    }

    #[test]
    fn implicit_tracks_take_grid_auto() {
        let css = "grid-template-columns: 20px; grid-auto-rows: 10px 30px";
        assert_eq!(grid(css, &boxes(3, ""), "c"), r(8.0, 48.0, 20.0, 10.0));
        assert_eq!(grid(css, &boxes(3, ""), "b"), r(8.0, 18.0, 20.0, 30.0));
    }

    #[test]
    fn areas_and_named_lines() {
        let css = "grid-template-areas: 'h h' 'n m'; grid-template-columns: 30px 70px; grid-template-rows: 10px 20px";
        let html = "<div id=a style='grid-area: m'></div>";
        assert_eq!(grid(css, html, "a"), r(38.0, 18.0, 70.0, 20.0));
        let css = "grid-template-columns: [one] 30px [two] 70px [three]; grid-template-rows: 10px";
        let html = "<div id=a style='grid-column: two / three'></div>";
        assert_eq!(grid(css, html, "a"), r(38.0, 8.0, 70.0, 10.0));
    }

    #[test]
    fn auto_fill_and_auto_fit() {
        let css = "width: 330px; grid-template-columns: repeat(auto-fill, 100px); gap: 10px";
        assert_eq!(grid(css, &boxes(4, ""), "d").x, 8.0);
        assert_eq!(grid(css, &boxes(4, ""), "c").x, 228.0);
        let css = "width: 330px; grid-template-columns: repeat(auto-fit, minmax(100px, 1fr)); gap: 10px";
        assert_eq!(grid(css, &boxes(2, ""), "b"), r(178.0, 8.0, 160.0, 0.0));
    }

    #[test]
    fn items_align_in_their_areas() {
        let html = "<div id=a style='width: 20px; height: 10px'></div>";
        let base = "grid-template-columns: 100px; grid-template-rows: 50px;";
        assert_eq!(grid(&format!("{base} justify-items: center; align-items: end"), html, "a"), r(48.0, 48.0, 20.0, 10.0));
        assert_eq!(grid(&format!("{base} justify-items: end; align-items: center"), html, "a"), r(88.0, 28.0, 20.0, 10.0));
        let html = "<div id=a></div>";
        assert_eq!(grid(base, html, "a"), r(8.0, 8.0, 100.0, 50.0));
        let html = "<div id=a style='justify-self: start; align-self: start; width: 5px'></div>";
        assert_eq!(grid(base, html, "a"), r(8.0, 8.0, 5.0, 0.0));
    }

    #[test]
    fn content_alignment() {
        let css = "width: 100px; height: 100px; grid-template-columns: 20px 20px; grid-template-rows: 10px 10px";
        let b = |how: &str| grid(&format!("{css}; justify-content: {how}; align-content: {how}"), &boxes(4, ""), "d");
        assert_eq!(b("center"), r(58.0, 58.0, 20.0, 10.0));
        assert_eq!(b("end").x, 88.0);
        assert_eq!(b("space-between").x, 88.0);
        assert_eq!(b("space-around").x, 8.0 + 15.0 + 20.0 + 30.0);
        assert_eq!(b("space-evenly").y, 8.0 + 80.0 / 3.0 * 2.0 + 10.0);
    }

    #[test]
    fn auto_tracks_share_what_is_left() {
        let css = "width: 200px; grid-template-columns: 50px auto auto";
        assert_eq!(grid(css, &boxes(3, ""), "b"), r(58.0, 8.0, 75.0, 0.0));
    }

    #[test]
    fn content_sizes_the_tracks() {
        // A word of 3 letters is 28.8px wide.
        let css = "justify-content: start; grid-template-columns: auto auto";
        let html = "<div id=a>abc</div><div id=b>abcde</div>";
        assert_eq!(grid(css, html, "a").w, 28.8);
        assert!((grid(css, html, "b").w - 48.0).abs() < 0.01);
        let css = "grid-template-columns: min-content max-content";
        let html = "<div id=a>abc de</div><div id=b>abc de</div>";
        assert!((grid(css, html, "a").w - 28.8).abs() < 0.01);
        assert!((grid(css, html, "b").w - 57.6).abs() < 0.01);
    }

    #[test]
    fn minmax_and_fit_content() {
        let css = "width: 200px; grid-template-columns: minmax(50px, 100px) minmax(10px, 1fr)";
        assert_eq!(grid(css, &boxes(2, ""), "a").w, 100.0);
        assert_eq!(grid(css, &boxes(2, ""), "b").w, 100.0);
        let css = "grid-template-columns: fit-content(60px) 1fr";
        let html = "<div id=a>abc defg hij</div><div id=b>x</div>";
        assert_eq!(grid(css, html, "a").w, 60.0);
        // A word that cannot break is wider than the limit.
        let html = "<div id=a>abcdefghij</div><div id=b>x</div>";
        assert_eq!(grid(css, html, "a").w, 96.0);
    }

    #[test]
    fn a_fr_track_has_a_content_minimum() {
        let css = "width: 100px; grid-template-columns: 1fr 1fr";
        let html = "<div id=a>abcdefghij</div><div id=b></div>";
        assert_eq!(grid(css, html, "a").w, 96.0);
        assert_eq!(grid(css, html, "b").w, 4.0);
        let css = "width: 100px; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr)";
        assert_eq!(grid(css, html, "a").w, 50.0);
    }

    #[test]
    fn percent_tracks() {
        let css = "width: 200px; height: 100px; grid-template-columns: 25% 1fr; grid-template-rows: 40%";
        assert_eq!(grid(css, &boxes(2, ""), "a"), r(8.0, 8.0, 50.0, 40.0));
        assert_eq!(grid(css, &boxes(2, ""), "b").w, 150.0);
    }

    #[test]
    fn a_container_without_a_height_grows_with_its_rows() {
        let html = "<div id=a style='height: 30px'></div><div id=b style='height: 50px'></div>";
        assert_eq!(grid("grid-template-columns: 10px 10px", html, "g").h, 50.0);
        assert_eq!(grid("grid-template-columns: 10px; gap: 5px", html, "g").h, 85.0);
        // A `min-height` gives room: the stretched rows share it.
        assert_eq!(grid("grid-template-columns: 10px 10px; grid-auto-rows: auto; min-height: 200px", html, "a").h, 30.0);
    }

    #[test]
    fn a_run_of_text_is_an_item() {
        let css = "grid-template-columns: 100px 100px";
        let html = "x<div id=a></div>";
        assert_eq!(grid(css, html, "a").x, 108.0);
        // The spaces between items are not items.
        let html = "<div id=a></div> <div id=b></div>";
        assert_eq!(grid(css, html, "b").x, 108.0);
    }

    #[test]
    fn an_absolute_child_is_no_item() {
        let css = "position: relative; grid-template-columns: 100px 100px";
        let html = "<div id=a style='position: absolute; left: 5px; top: 6px'></div><div id=b></div>";
        assert_eq!(grid(css, html, "a").x, 13.0);
        assert_eq!(grid(css, html, "b").x, 8.0);
    }

    #[test]
    fn an_inline_grid_shrinks_to_fit() {
        let html = "<div id=g style='display: inline-grid; grid-template-columns: auto auto; gap: 4px'><div>abc</div><div>de</div></div>";
        assert_eq!(rect(html, "", "g").w, 28.8 + 19.2 + 4.0);
    }

    #[test]
    fn order_moves_items() {
        let html = "<div id=a style='order: 2'></div><div id=b></div>";
        assert_eq!(grid("grid-template-columns: 10px 10px", html, "b").x, 8.0);
        assert_eq!(grid("grid-template-columns: 10px 10px", html, "a").x, 18.0);
    }

    #[test]
    fn a_stretched_item_passes_its_height_to_its_children() {
        let html = "<div id=a style='height: 60px'></div><div id=b><div id=c style='height: 50%'></div></div>";
        assert_eq!(grid("grid-template-columns: 10px 10px", html, "c").h, 30.0);
    }

    #[test]
    fn nothing_is_lost_in_the_dump() {
        let out = dump("<div id=g style='display: grid; grid-template-columns: 1fr 1fr'><div id=a>x</div><div id=b>y</div></div>", "");
        assert!(out.contains("div#a") && out.contains("div#b"));
    }
}

//! Grid values (CSS Grid Layout 1): track lists, line placements and area names.
//!
//! A `Value` is 8 bytes and `Copy`, and a track list does not fit in it. So the list lives in a side list and the value
//! is `Value::Grid(i)`, the place in that list. This is the way `Length::Calc` works too. Two lists, as for `calc()`:
//! - `Side::grids`: what the parser read. The lengths are not computed yet (`em`, `vw`, ...).
//! - `Styles::grids`: after the cascade. All lengths are pixels or percents, and a list that two elements have is kept once.
//!
//! What fits in 8 bytes stays in the `Value`: `none` and `auto` are keywords, and `grid-auto-flow` is a number
//! (`FLOW_COLUMN` and `FLOW_DENSE` added).
//!
//! The text of these properties is read here, from the tokens, because a `Part` list cannot hold `repeat(...)`, `[a b]`
//! or `1fr`.
//!
//! Left out:
//! - `calc()` inside a track size: the declaration is dropped.
//! - `subgrid` and `masonry`: the declaration is dropped.
//! - The `grid` and `grid-template` shorthands.
//! - `grid-template-areas` also does not read `repeat()`, as nothing does.

use crate::tokenizer::{Token, tokenize};
use crate::values::{Declared, Keyword, Length, Property, Side, Value, length};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// `grid-auto-flow` is a number: the bits that follow. `row` is 0.
pub const FLOW_COLUMN: f32 = 1.0;
pub const FLOW_DENSE: f32 = 2.0;

/// The most tracks that a grid has in one direction. Chrome stops at 10,000 too.
pub const MAX_TRACKS: usize = 10_000;

/// One side of the size of a track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bound {
    Auto,
    MinContent,
    MaxContent,
    /// A length or a percent.
    Len(Length),
    /// A share of the free space: `1fr` is `Fr(1.0)`. Only for the end (max) side.
    Fr(f32),
    /// `fit-content(L)`: the max-content size, but not more than `L`. Only for the end (max) side. The start side is `Auto`.
    Fit(Length),
}

// The `f32` of a bound is never NaN: the parser takes numbers from tokens. So `==` is a good equality.
impl Eq for Bound {}

impl Hash for Bound {
    fn hash<H: Hasher>(&self, h: &mut H) {
        let (tag, x) = match *self {
            Bound::Auto => (0u8, 0.0),
            Bound::MinContent => (1, 0.0),
            Bound::MaxContent => (2, 0.0),
            Bound::Len(l) => (3 + len_tag(l), len_value(l)),
            Bound::Fr(x) => (8, x),
            Bound::Fit(l) => (9 + len_tag(l), len_value(l)),
        };
        tag.hash(h);
        x.to_bits().hash(h);
    }
}

fn len_tag(l: Length) -> u8 {
    matches!(l, Length::Percent(_)) as u8
}

fn len_value(l: Length) -> f32 {
    match l {
        Length::Px(x) | Length::Percent(x) | Length::Em(x) | Length::Rem(x) | Length::Vw(x) | Length::Vh(x) | Length::Vmin(x) | Length::Vmax(x) => x,
        Length::Calc(_) => 0.0,
    }
}

/// The size of one track: `minmax(min, max)`. A plain size is `min == max`, and `1fr` is `minmax(auto, 1fr)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Track {
    pub min: Bound,
    pub max: Bound,
}

/// How many times a `repeat()` repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Count {
    Times(u32),
    /// As many as fit. An empty one stays (`auto-fill`) or goes to 0 width (`auto-fit`).
    Fill,
    Fit,
}

/// One part of a track list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Part {
    /// `[a b]`: names for the line at this place.
    Names(Box<[Box<str>]>),
    Track(Track),
    /// `repeat(count, ...)`. Inside are only `Names` and `Track`.
    Repeat(Count, Vec<Part>),
}

/// How one grid item is placed on one side: `grid-row-start` and the three like it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Line {
    Auto,
    /// `3`, `-1`. Never 0.
    Num(i32),
    /// `name`, `name 2`: the n-th line with this name (from the end if n is negative). Or an area name, for n = 1.
    Name(Box<str>, i32),
    /// `span 2`, `span name`, `span name 2`. The count is at least 1.
    Span(Option<Box<str>>, u32),
}

/// `grid-template-areas`: the names of the areas, and the lines of each (row start, row end, column start, column end).
/// The lines count from 1, and the end line is after the last cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Areas {
    pub rows: u32,
    pub columns: u32,
    pub names: Vec<(Box<str>, [u32; 4])>,
}

/// What `Value::Grid(i)` points to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Grid {
    /// `grid-template-columns`, `grid-template-rows`, `grid-auto-columns`, `grid-auto-rows`.
    Tracks(Vec<Part>),
    Line(Line),
    Areas(Areas),
}

impl Grid {
    /// Give each length of the value to `f`, and keep what it returns. The cascade turns `em` and the like into pixels.
    pub fn map_lengths(&mut self, f: &mut dyn FnMut(Length) -> Length) {
        fn bound(b: &mut Bound, f: &mut dyn FnMut(Length) -> Length) {
            if let Bound::Len(l) | Bound::Fit(l) = b {
                *l = f(*l);
            }
        }
        fn parts(list: &mut [Part], f: &mut dyn FnMut(Length) -> Length) {
            for part in list {
                match part {
                    Part::Track(t) => {
                        bound(&mut t.min, f);
                        bound(&mut t.max, f);
                    }
                    Part::Repeat(_, inner) => parts(inner, f),
                    Part::Names(_) => {}
                }
            }
        }
        if let Grid::Tracks(list) = self {
            parts(list, f);
        }
    }

    /// A hash, to find a list that is kept already.
    pub fn hash_value(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// The computed grid values, each one once: two elements with the same list share a number, so their styles can be shared too.
#[derive(Default)]
pub struct Kept {
    pub list: Vec<Grid>,
    by_hash: HashMap<u64, Vec<u32>>,
}

impl Kept {
    pub fn keep(&mut self, grid: Grid) -> u32 {
        let hash = grid.hash_value();
        let found = self.by_hash.entry(hash).or_default();
        if let Some(&i) = found.iter().find(|&&i| self.list[i as usize] == grid) {
            return i;
        }
        self.list.push(grid);
        found.push(self.list.len() as u32 - 1);
        self.list.len() as u32 - 1
    }
}

/// The longhands of the shorthands that this file reads. `None` for any other name.
pub fn longhands(name: &str) -> Option<&'static [Property]> {
    use Property::*;
    Some(match name {
        "grid-row" => &[GridRowStart, GridRowEnd],
        "grid-column" => &[GridColumnStart, GridColumnEnd],
        "grid-area" => &[GridRowStart, GridColumnStart, GridRowEnd, GridColumnEnd],
        "place-items" => &[AlignItems, JustifyItems],
        "place-self" => &[AlignSelf, JustifySelf],
        "place-content" => &[AlignContent, JustifyContent],
        _ => return Option::None,
    })
}

/// Read the properties whose text is not a list of simple parts. `None`: this is not one of them. `Some(false)`: the
/// value is not good, and `out` is as before. `Some(true)`: the longhands are in `out`.
pub fn declaration(name: &str, value: &str, important: bool, side: &mut Side, out: &mut Vec<Declared>) -> Option<bool> {
    use Property::*;
    let text = value.trim();
    let wide = |t: &str| match t {
        t if t.eq_ignore_ascii_case("inherit") => Some(Value::Inherit),
        t if t.eq_ignore_ascii_case("initial") => Some(Value::Initial),
        t if t.eq_ignore_ascii_case("unset") => Some(Value::Unset),
        _ => Option::None,
    };
    let list: &[Property] = match name {
        "grid-template-columns" => &[GridTemplateColumns],
        "grid-template-rows" => &[GridTemplateRows],
        "grid-auto-columns" => &[GridAutoColumns],
        "grid-auto-rows" => &[GridAutoRows],
        "grid-auto-flow" => &[GridAutoFlow],
        "grid-template-areas" => &[GridTemplateAreas],
        "grid-row-start" => &[GridRowStart],
        "grid-row-end" => &[GridRowEnd],
        "grid-column-start" => &[GridColumnStart],
        "grid-column-end" => &[GridColumnEnd],
        _ => longhands(name).filter(|_| name.starts_with("grid-"))?,
    };
    if let Some(w) = wide(text) {
        out.extend(list.iter().map(|&property| Declared { property, value: w, important }));
        return Some(true);
    }
    let start = out.len();
    let mut emit = |property, value| out.push(Declared { property, value, important });
    let ok = read(name, text, side, &mut emit);
    if !ok {
        out.truncate(start);
    }
    Some(ok)
}

fn read(name: &str, text: &str, side: &mut Side, emit: &mut dyn FnMut(Property, Value)) -> bool {
    use Property::*;
    let mut keep = |grid: Grid| {
        side.grids.push(grid);
        Value::Grid(side.grids.len() as u32 - 1)
    };
    let tokens: Vec<Token> = tokenize(text).filter(|t| *t != Token::Whitespace).collect();
    match name {
        "grid-template-columns" | "grid-template-rows" => {
            let Some(v) = template(&tokens) else { return false };
            let property = if name.ends_with("columns") { GridTemplateColumns } else { GridTemplateRows };
            emit(property, v.map_or(Value::Keyword(Keyword::None), &mut keep));
            true
        }
        "grid-auto-columns" | "grid-auto-rows" => {
            let Some(list) = auto_tracks(&tokens) else { return false };
            let property = if name.ends_with("columns") { GridAutoColumns } else { GridAutoRows };
            emit(property, if list.is_empty() { Value::Keyword(Keyword::Auto) } else { keep(Grid::Tracks(list)) });
            true
        }
        "grid-auto-flow" => {
            let Some(flow) = auto_flow(&tokens) else { return false };
            emit(GridAutoFlow, Value::Number(flow));
            true
        }
        "grid-template-areas" => {
            let Some(areas) = areas(&tokens) else { return false };
            emit(GridTemplateAreas, areas.map_or(Value::Keyword(Keyword::None), |a| keep(Grid::Areas(a))));
            true
        }
        "grid-row-start" | "grid-row-end" | "grid-column-start" | "grid-column-end" => {
            let Some(line) = line(&tokens) else { return false };
            let property = match name {
                "grid-row-start" => GridRowStart,
                "grid-row-end" => GridRowEnd,
                "grid-column-start" => GridColumnStart,
                _ => GridColumnEnd,
            };
            emit(property, line_value(line, &mut keep));
            true
        }
        // `a / b`, and for `grid-area` up to four.
        "grid-row" | "grid-column" | "grid-area" => {
            let mut lines: Vec<Line> = Vec::new();
            for part in tokens.split(|t| *t == Token::Delim('/')) {
                let Some(l) = line(part) else { return false };
                lines.push(l);
            }
            let max = if name == "grid-area" { 4 } else { 2 };
            if lines.len() > max {
                return false;
            }
            // What is missing is the same name if the first line is a name, else `auto`.
            let same = |l: &Line| match l {
                Line::Name(n, 1) => Line::Name(n.clone(), 1),
                _ => Line::Auto,
            };
            let first = lines[0].clone();
            let get = |i: usize| lines.get(i).cloned();
            let order: Vec<(Property, Line)> = match name {
                "grid-row" => vec![(GridRowStart, first.clone()), (GridRowEnd, get(1).unwrap_or(same(&first)))],
                "grid-column" => vec![(GridColumnStart, first.clone()), (GridColumnEnd, get(1).unwrap_or(same(&first)))],
                // row-start / column-start / row-end / column-end. The end of the columns copies the start of the columns.
                _ => {
                    let column = get(1).unwrap_or(same(&first));
                    let column_end = get(3).unwrap_or(same(&column));
                    vec![(GridRowStart, first.clone()), (GridColumnStart, column), (GridRowEnd, get(2).unwrap_or(same(&first))), (GridColumnEnd, column_end)]
                }
            };
            for (property, l) in order {
                emit(property, line_value(l, &mut keep));
            }
            true
        }
        _ => false,
    }
}

fn line_value(line: Line, keep: &mut dyn FnMut(Grid) -> Value) -> Value {
    match line {
        Line::Auto => Value::Keyword(Keyword::Auto),
        l => keep(Grid::Line(l)),
    }
}

/// `none`, or a track list. `Some(None)` is `none`.
fn template(tokens: &[Token]) -> Option<Option<Grid>> {
    if let [Token::Ident(w)] = tokens {
        if w.eq_ignore_ascii_case("none") {
            return Some(None);
        }
    }
    let mut at = 0;
    let (parts, auto) = track_list(tokens, &mut at, false)?;
    // An auto repeat needs all the other tracks to be fixed.
    if auto && !fixed_only(&parts) {
        return None;
    }
    (at == tokens.len() && parts.iter().any(|p| !matches!(p, Part::Names(_)))).then_some(Some(Grid::Tracks(parts)))
}

fn auto_tracks(tokens: &[Token]) -> Option<Vec<Part>> {
    let mut at = 0;
    let mut list = Vec::new();
    while at < tokens.len() {
        list.push(Part::Track(track_size(tokens, &mut at)?));
    }
    // `auto` alone is the starting value: the caller gets an empty list for it.
    if list == [Part::Track(Track { min: Bound::Auto, max: Bound::Auto })] {
        return Some(Vec::new());
    }
    (!list.is_empty()).then_some(list)
}

/// A track list from `at`. `inner`: inside a `repeat()`, where no other `repeat()` goes. Gives the parts and if an auto
/// repeat is in them. Stops before a `)`.
fn track_list(tokens: &[Token], at: &mut usize, inner: bool) -> Option<(Vec<Part>, bool)> {
    let mut parts: Vec<Part> = Vec::new();
    let mut auto = false;
    while let Some(token) = tokens.get(*at) {
        match token {
            Token::OpenSquare => {
                *at += 1;
                let mut names = Vec::new();
                loop {
                    match tokens.get(*at)? {
                        Token::Ident(n) if !is_reserved(n) => names.push(n.as_ref().into()),
                        Token::CloseSquare => break,
                        _ => return None,
                    }
                    *at += 1;
                }
                *at += 1;
                // Two lists of names in a row are one.
                if let Some(Part::Names(prev)) = parts.last_mut() {
                    let mut all = prev.to_vec();
                    all.extend(names);
                    *prev = all.into();
                } else {
                    parts.push(Part::Names(names.into()));
                }
            }
            Token::CloseParen if inner => break,
            Token::Function(f) if f.eq_ignore_ascii_case("repeat") && !inner => {
                *at += 1;
                let count = match tokens.get(*at)? {
                    Token::Number { value, is_int: true } if *value >= 1.0 => Count::Times(value.min(MAX_TRACKS as f32) as u32),
                    Token::Ident(w) if w.eq_ignore_ascii_case("auto-fill") => Count::Fill,
                    Token::Ident(w) if w.eq_ignore_ascii_case("auto-fit") => Count::Fit,
                    _ => return None,
                };
                *at += 1;
                if tokens.get(*at)? != &Token::Comma {
                    return None;
                }
                *at += 1;
                let (list, _) = track_list(tokens, at, true)?;
                if tokens.get(*at)? != &Token::CloseParen {
                    return None;
                }
                *at += 1;
                if !list.iter().any(|p| matches!(p, Part::Track(_))) {
                    return None;
                }
                if matches!(count, Count::Fill | Count::Fit) {
                    // An auto repeat: the tracks inside are fixed, and there is only one.
                    if auto || !fixed_only(&list) {
                        return None;
                    }
                    auto = true;
                }
                parts.push(Part::Repeat(count, list));
            }
            _ => parts.push(Part::Track(track_size(tokens, at)?)),
        }
    }
    Some((parts, auto))
}

/// Is every track fixed (a length or a percent on at least one side, and no `fr`)? A `repeat(<int>)` inside counts by its tracks.
fn fixed_only(parts: &[Part]) -> bool {
    parts.iter().all(|p| match p {
        Part::Names(_) => true,
        Part::Track(t) => !matches!(t.min, Bound::Fr(_) | Bound::Fit(_)) && !matches!(t.max, Bound::Fr(_) | Bound::Fit(_)) && (matches!(t.min, Bound::Len(_)) || matches!(t.max, Bound::Len(_))),
        Part::Repeat(_, inner) => fixed_only(inner),
    })
}

/// Words that a name of a line or an area cannot be.
fn is_reserved(name: &str) -> bool {
    ["span", "auto", "inherit", "initial", "unset", "default"].iter().any(|w| name.eq_ignore_ascii_case(w))
}

/// One track size: a breadth, `minmax(a, b)` or `fit-content(L)`.
fn track_size(tokens: &[Token], at: &mut usize) -> Option<Track> {
    let token = tokens.get(*at)?;
    *at += 1;
    match token {
        Token::Function(f) if f.eq_ignore_ascii_case("minmax") => {
            let min = breadth(tokens.get(*at)?)?;
            *at += 1;
            if tokens.get(*at)? != &Token::Comma {
                return None;
            }
            *at += 1;
            let max = breadth(tokens.get(*at)?)?;
            *at += 1;
            if tokens.get(*at)? != &Token::CloseParen {
                return None;
            }
            *at += 1;
            // The start side takes no `fr`.
            (!matches!(min, Bound::Fr(_))).then_some(Track { min, max })
        }
        Token::Function(f) if f.eq_ignore_ascii_case("fit-content") => {
            let Bound::Len(l) = breadth(tokens.get(*at)?)? else { return None };
            *at += 1;
            if tokens.get(*at)? != &Token::CloseParen {
                return None;
            }
            *at += 1;
            Some(Track { min: Bound::Auto, max: Bound::Fit(l) })
        }
        t => match breadth(t)? {
            Bound::Fr(x) => Some(Track { min: Bound::Auto, max: Bound::Fr(x) }),
            b => Some(Track { min: b, max: b }),
        },
    }
}

fn breadth(token: &Token) -> Option<Bound> {
    Some(match token {
        Token::Ident(w) if w.eq_ignore_ascii_case("auto") => Bound::Auto,
        Token::Ident(w) if w.eq_ignore_ascii_case("min-content") => Bound::MinContent,
        Token::Ident(w) if w.eq_ignore_ascii_case("max-content") => Bound::MaxContent,
        Token::Number { value: 0.0, .. } => Bound::Len(Length::Px(0.0)),
        Token::Percentage(p) if *p >= 0.0 => Bound::Len(Length::Percent(*p)),
        Token::Dimension { value, unit, .. } if *value >= 0.0 && unit.eq_ignore_ascii_case("fr") => Bound::Fr(*value),
        Token::Dimension { value, unit, .. } if *value >= 0.0 => Bound::Len(length(*value, unit)?),
        _ => return None,
    })
}

fn auto_flow(tokens: &[Token]) -> Option<f32> {
    let (mut axis, mut dense) = (Option::None, false);
    for t in tokens {
        let Token::Ident(w) = t else { return None };
        match w.to_ascii_lowercase().as_str() {
            "row" if axis.is_none() => axis = Some(0.0),
            "column" if axis.is_none() => axis = Some(FLOW_COLUMN),
            "dense" if !dense => dense = true,
            _ => return None,
        }
    }
    (!tokens.is_empty()).then(|| axis.unwrap_or(0.0) + if dense { FLOW_DENSE } else { 0.0 })
}

/// One line: `auto`, `3`, `-1`, `name`, `name 2`, `2 name`, `span 2`, `span name`, `span 2 name`, `span name 2`.
fn line(tokens: &[Token]) -> Option<Line> {
    let mut span = false;
    let (mut int, mut name): (Option<i32>, Option<Box<str>>) = (Option::None, Option::None);
    for t in tokens {
        match t {
            Token::Ident(w) if w.eq_ignore_ascii_case("span") && !span && int.is_none() && name.is_none() => span = true,
            Token::Ident(w) if w.eq_ignore_ascii_case("auto") && tokens.len() == 1 => return Some(Line::Auto),
            Token::Ident(w) if !is_reserved(w) && name.is_none() => name = Some(w.as_ref().into()),
            Token::Number { value, is_int: true } if int.is_none() && *value != 0.0 && value.abs() < 1e9 => int = Some(*value as i32),
            _ => return None,
        }
    }
    if span {
        // A span is at least 1, and takes no sign.
        let n = int.unwrap_or(1);
        return (n >= 1 && (int.is_some() || name.is_some())).then(|| Line::Span(name, n as u32));
    }
    match (int, name) {
        (Some(n), None) => Some(Line::Num(n)),
        (n, Some(name)) => Some(Line::Name(name, n.unwrap_or(1))),
        _ => Option::None,
    }
}

/// `none`, or strings. `Some(None)` is `none`.
fn areas(tokens: &[Token]) -> Option<Option<Areas>> {
    if let [Token::Ident(w)] = tokens {
        return w.eq_ignore_ascii_case("none").then_some(None);
    }
    let mut cells: Vec<Option<&str>> = Vec::new();
    let (mut rows, mut columns) = (0u32, 0usize);
    for t in tokens {
        let Token::String(row) = t else { return None };
        let before = cells.len();
        for word in row.split_ascii_whitespace() {
            if word.bytes().all(|b| b == b'.') {
                cells.push(Option::None);
            } else if word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80) {
                cells.push(Some(word));
            } else {
                return None;
            }
        }
        let n = cells.len() - before;
        if n == 0 || (rows > 0 && n != columns) || rows as usize >= MAX_TRACKS {
            return None;
        }
        columns = n;
        rows += 1;
    }
    if rows == 0 || columns > MAX_TRACKS {
        return None;
    }
    // The box of each name: [row start, row end, column start, column end], and how many cells it has.
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut found: Vec<(&str, [u32; 4], u32)> = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        let Some(name) = cell else { continue };
        let (r, c) = ((i / columns) as u32, (i % columns) as u32);
        let at = *index.entry(name).or_insert_with(|| {
            found.push((name, [r, r, c, c], 0));
            found.len() - 1
        });
        let (_, b, n) = &mut found[at];
        b[0] = b[0].min(r);
        b[1] = b[1].max(r);
        b[2] = b[2].min(c);
        b[3] = b[3].max(c);
        *n += 1;
    }
    // Each area is a rectangle with no hole: its cells are all its box.
    let mut names = Vec::with_capacity(found.len());
    for (name, b, n) in found {
        if n != (b[1] - b[0] + 1) * (b[3] - b[2] + 1) {
            return None;
        }
        names.push((name.into(), [b[0] + 1, b[1] + 2, b[2] + 1, b[3] + 2]));
    }
    Some(Some(Areas { rows, columns: columns as u32, names }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::parse_declaration;

    fn read(name: &str, value: &str) -> (Vec<(Property, Value)>, Side) {
        let (mut side, mut out) = (Side::default(), Vec::new());
        parse_declaration(name, value, false, &mut side, &mut out);
        (out.into_iter().map(|d| (d.property, d.value)).collect(), side)
    }

    fn tracks(value: &str) -> Option<Vec<Part>> {
        let (out, side) = read("grid-template-columns", value);
        match out.first()?.1 {
            Value::Grid(i) => match &side.grids[i as usize] {
                Grid::Tracks(t) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn px(v: f32) -> Bound {
        Bound::Len(Length::Px(v))
    }

    fn t(min: Bound, max: Bound) -> Part {
        Part::Track(Track { min, max })
    }

    #[test]
    fn track_sizes() {
        assert_eq!(tracks("100px 1fr auto"), Some(vec![t(px(100.0), px(100.0)), t(Bound::Auto, Bound::Fr(1.0)), t(Bound::Auto, Bound::Auto)]));
        assert_eq!(tracks("minmax(10px, 2fr) 50% min-content max-content 0"), Some(vec![t(px(10.0), Bound::Fr(2.0)), t(Bound::Len(Length::Percent(50.0)), Bound::Len(Length::Percent(50.0))), t(Bound::MinContent, Bound::MinContent), t(Bound::MaxContent, Bound::MaxContent), t(px(0.0), px(0.0))]));
        assert_eq!(tracks("fit-content(30px)"), Some(vec![t(Bound::Auto, Bound::Fit(Length::Px(30.0)))]));
        // Fixed units are pixels already. `em` waits for the cascade.
        assert_eq!(tracks("1in 2em"), Some(vec![t(px(96.0), px(96.0)), t(Bound::Len(Length::Em(2.0)), Bound::Len(Length::Em(2.0)))]));
        let (out, _) = read("grid-template-columns", "none");
        assert_eq!(out, vec![(Property::GridTemplateColumns, Value::Keyword(Keyword::None))]);
        for bad in ["", "1", "-1px", "-1fr", "minmax(1fr, 2px)", "minmax(1px)", "fit-content(1fr)", "fit-content(auto)", "repeat(0, 1px)", "repeat(2)", "calc(1px + 1%)", "subgrid", "none 1px", "[a]", "1px 2px 3", "minmax(1px, 2px", "repeat(auto-fill, 1fr)", "repeat(auto-fill, auto)", "repeat(auto-fill, 1px) repeat(auto-fit, 1px)", "1fr repeat(auto-fill, 1px)", "repeat(2, repeat(2, 1px))", "repeat(2, [a])"] {
            assert_eq!(tracks(bad), None, "{bad}");
        }
    }

    #[test]
    fn names_and_repeats() {
        let names = |n: &[&str]| Part::Names(n.iter().map(|&s| s.into()).collect());
        assert_eq!(tracks("[a b] 1fr [c] [d] 2fr [e]"), Some(vec![names(&["a", "b"]), t(Bound::Auto, Bound::Fr(1.0)), names(&["c", "d"]), t(Bound::Auto, Bound::Fr(2.0)), names(&["e"])]));
        assert_eq!(tracks("repeat(3, [x] 10px 1fr)"), Some(vec![Part::Repeat(Count::Times(3), vec![names(&["x"]), t(px(10.0), px(10.0)), t(Bound::Auto, Bound::Fr(1.0))])]));
        assert_eq!(tracks("10px repeat(auto-fill, minmax(50px, 100px))"), Some(vec![t(px(10.0), px(10.0)), Part::Repeat(Count::Fill, vec![t(px(50.0), px(100.0))])]));
        assert_eq!(tracks("repeat(AUTO-FIT, 5px)"), Some(vec![Part::Repeat(Count::Fit, vec![t(px(5.0), px(5.0))])]));
        // A count over the limit stops at the limit.
        assert_eq!(tracks("repeat(99999999, 1px)"), Some(vec![Part::Repeat(Count::Times(MAX_TRACKS as u32), vec![t(px(1.0), px(1.0))])]));
        // `span` and `auto` are not names.
        assert_eq!(tracks("[span] 1px"), None);
    }

    #[test]
    fn auto_tracks_and_flow() {
        let (out, side) = read("grid-auto-rows", "10px minmax(1px, auto)");
        assert_eq!(out, vec![(Property::GridAutoRows, Value::Grid(0))]);
        assert_eq!(side.grids[0], Grid::Tracks(vec![t(px(10.0), px(10.0)), t(px(1.0), Bound::Auto)]));
        assert_eq!(read("grid-auto-columns", "auto").0, vec![(Property::GridAutoColumns, Value::Keyword(Keyword::Auto))]);
        assert_eq!(read("grid-auto-columns", "repeat(2, 1px)").0, vec![]);
        assert_eq!(read("grid-auto-columns", "[a] 1px").0, vec![]);
        let flow = |v| read("grid-auto-flow", v).0.first().map(|p| p.1);
        assert_eq!(flow("row"), Some(Value::Number(0.0)));
        assert_eq!(flow("column"), Some(Value::Number(FLOW_COLUMN)));
        assert_eq!(flow("dense"), Some(Value::Number(FLOW_DENSE)));
        assert_eq!(flow("dense column"), Some(Value::Number(FLOW_COLUMN + FLOW_DENSE)));
        assert_eq!(flow("row dense"), Some(Value::Number(FLOW_DENSE)));
        for bad in ["", "row column", "dense dense", "auto", "1"] {
            assert_eq!(flow(bad), None, "{bad}");
        }
    }

    fn lines(name: &str, value: &str) -> Vec<(Property, Option<Line>)> {
        let (out, side) = read(name, value);
        out.into_iter()
            .map(|(p, v)| match v {
                Value::Keyword(Keyword::Auto) => (p, Some(Line::Auto)),
                Value::Grid(i) => match &side.grids[i as usize] {
                    Grid::Line(l) => (p, Some(l.clone())),
                    _ => (p, Option::None),
                },
                _ => (p, Option::None),
            })
            .collect()
    }

    #[test]
    fn line_placement() {
        use Property::*;
        let one = |v| lines("grid-row-start", v).into_iter().next().and_then(|x| x.1);
        assert_eq!(one("auto"), Some(Line::Auto));
        assert_eq!(one("3"), Some(Line::Num(3)));
        assert_eq!(one("-2"), Some(Line::Num(-2)));
        assert_eq!(one("span 2"), Some(Line::Span(Option::None, 2)));
        assert_eq!(one("span"), Option::None);
        assert_eq!(one("span a"), Some(Line::Span(Some("a".into()), 1)));
        assert_eq!(one("span a 3"), Some(Line::Span(Some("a".into()), 3)));
        assert_eq!(one("3 span"), None);
        assert_eq!(one("a"), Some(Line::Name("a".into(), 1)));
        assert_eq!(one("2 a"), Some(Line::Name("a".into(), 2)));
        assert_eq!(one("a -1"), Some(Line::Name("a".into(), -1)));
        for bad in ["0", "span 0", "span -1", "1.5", "a b", "1px", "span span", "2 3"] {
            assert_eq!(one(bad), None, "{bad}");
        }
        let l = |n| Some(Line::Num(n));
        assert_eq!(lines("grid-column", "2 / span 3"), vec![(GridColumnStart, l(2)), (GridColumnEnd, Some(Line::Span(Option::None, 3)))]);
        assert_eq!(lines("grid-row", "2"), vec![(GridRowStart, l(2)), (GridRowEnd, Some(Line::Auto))]);
        // A name alone is both lines.
        let n = Some(Line::Name("a".into(), 1));
        assert_eq!(lines("grid-row", "a"), vec![(GridRowStart, n.clone()), (GridRowEnd, n.clone())]);
        assert_eq!(lines("grid-area", "a"), vec![(GridRowStart, n.clone()), (GridColumnStart, n.clone()), (GridRowEnd, n.clone()), (GridColumnEnd, n.clone())]);
        assert_eq!(
            lines("grid-area", "1 / 2 / 3 / 4"),
            vec![(GridRowStart, l(1)), (GridColumnStart, l(2)), (GridRowEnd, l(3)), (GridColumnEnd, l(4))]
        );
        assert_eq!(lines("grid-area", "1 / b"), vec![(GridRowStart, l(1)), (GridColumnStart, Some(Line::Name("b".into(), 1))), (GridRowEnd, Some(Line::Auto)), (GridColumnEnd, Some(Line::Name("b".into(), 1)))]);
        assert_eq!(lines("grid-area", "1 / 2 / 3 / 4 / 5"), vec![]);
        assert_eq!(lines("grid-row", "1 / 2 / 3"), vec![]);
        assert_eq!(lines("grid-row", "1 /"), vec![]);
        assert_eq!(read("grid-row", "inherit").0, vec![(GridRowStart, Value::Inherit), (GridRowEnd, Value::Inherit)]);
        assert_eq!(read("grid-area", "initial").0.len(), 4);
    }

    #[test]
    fn areas_need_rectangles() {
        let a = |v| {
            let (out, side) = read("grid-template-areas", v);
            match out.first()?.1 {
                Value::Grid(i) => match &side.grids[i as usize] {
                    Grid::Areas(a) => Some(a.clone()),
                    _ => None,
                },
                _ => None,
            }
        };
        let got = a("'h h h' 'n . m' 'n f f'").unwrap();
        assert_eq!((got.rows, got.columns), (3, 3));
        let find = |n: &str| got.names.iter().find(|x| &*x.0 == n).unwrap().1;
        assert_eq!(find("h"), [1, 2, 1, 4]);
        assert_eq!(find("n"), [2, 4, 1, 2]);
        assert_eq!(find("m"), [2, 3, 3, 4]);
        assert_eq!(find("f"), [3, 4, 2, 4]);
        // `...` is one empty cell. An L shape, a hole, rows of different length, no cell: not good.
        assert!(a("'a ...' 'b b'").is_some());
        for bad in ["'a a' 'a b' 'b b'", "'a a a' 'a . a'", "'a b' 'a'", "''", "'a' x", "'a$'", "1"] {
            assert!(a(bad).is_none(), "{bad}");
        }
        assert_eq!(read("grid-template-areas", "none").0, vec![(Property::GridTemplateAreas, Value::Keyword(Keyword::None))]);
    }

    #[test]
    fn a_kept_list_is_kept_once() {
        let mut kept = Kept::default();
        let a = Grid::Tracks(vec![t(px(1.0), px(1.0))]);
        let b = Grid::Tracks(vec![t(px(2.0), px(2.0))]);
        assert_eq!((kept.keep(a.clone()), kept.keep(b.clone()), kept.keep(a), kept.keep(b)), (0, 1, 0, 1));
    }

    #[test]
    fn random_input_never_panics() {
        let alphabet: Vec<char> = "ab1.0%-/ (),[]\"frspan autominmaxrepeat".chars().collect();
        let names = ["grid-template-columns", "grid-template-areas", "grid-area", "grid-row", "grid-auto-flow", "grid-auto-rows"];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = next() % 24;
            let text: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let mut out = Vec::new();
            let _ = parse_declaration(names[(next() % names.len() as u64) as usize], &text, false, &mut Side::default(), &mut out);
        }
    }
}

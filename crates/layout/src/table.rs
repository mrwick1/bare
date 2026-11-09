//! Table layout (CSS 2.1, chapter 17, and CSS Tables 3), as Chrome does it (TablesNG).
//!
//! A table is laid out in four steps:
//! 1. Structure (`Plan::new`): the children of the table become captions, row groups, rows and cells, with a column and a
//!    row for each cell. `rowspan` and `colspan` are placed with one number for each column (the row where it is free
//!    again), so no cell is looked at twice. A part that is missing gets an anonymous one (CSS 2.1, 17.2.1): a cell
//!    in a table gets an anonymous row, and text or a block gets an anonymous cell. Anonymous rows, row groups and
//!    cells make no fragment: their content goes in the fragment of the box around them.
//! 2. Columns (`Plan::new`): each column gets a min and a max width from its cells, and a fixed width or a percent
//!    if a cell or a `<col>` says so. A cell that spans columns gives its widths to them after the others, with the
//!    same distribution as for the whole table. With `table-layout: fixed` and a `width`, only the first row counts.
//! 3. The width of the table: `width`, or with `auto` as wide as its content wants (the max-content width) but
//!    not wider than the room, and never narrower than the min-content width. `distribute` shares it between the
//!    columns, as Firefox and Chrome do: it goes from "all columns at their min width" to "percent columns at their
//!    percent", "fixed columns at their width" and "all at their max width", and a column gets the part of each step that fits.
//!    What is left after that goes to the auto columns, in proportion to their max width.
//! 4. Rows (`layout`): each cell is laid out once, at the width of its columns. A row is as high as its tallest
//!    cell (or the height that it asks for). A cell that spans rows gives its height to the rows after the rows of one
//!    row. Then each cell is stretched to its rows, and its content moves down for `vertical-align` (so no cell is laid out twice).
//!
//! Time: the structure and the columns cost one step for each cell, plus one for each column that a cell spans (at most
//! 1000) and for each row that it spans. A table inside a cell is measured once (`Cx::sizes`).
//!
//! Collapsed borders (`border-collapse: collapse`): the two borders at one edge become one (the widest wins, `hidden`
//! wins over all), and each cell and the table get half of it on each side. The edges are two lists of numbers, one
//! step for each edge.
//!
//! Not built: `visibility: collapse`, `empty-cells` (paint), a percent `height` of a cell, `rowspan="0"`, row group
//! and column borders in the collapsed model, the table wrapper that CSS makes around loose cells in a block.

use crate::Cx;
use crate::block::{self, Kids, Pos, Sides, Space};
use crate::boxes::{Child, Kind, Tree};
use crate::fragment::{self, Fragment, Rect};
use crate::inline;
use crate::intrinsic;
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::{Atom, NodeId};

/// The most columns: a cell that starts after them is not laid out. Chrome stops at 1,000,000. One of our columns
/// costs about 30 bytes in a table that is being laid out.
const MAX_COLS: usize = 16_384;
/// The most that `colspan` and `rowspan` say (HTML standard).
const MAX_COLSPAN: u32 = 1000;
const MAX_ROWSPAN: u32 = 65_534;
/// The most edges (cells of the grid) that a table with collapsed borders resolves. A bigger table uses the borders of its cells.
const MAX_EDGES: usize = 4_000_000;
/// A `hidden` border in an edge list. It beats all, with a width of 0.
const HIDDEN: f32 = -1.0;

/// Where the content of a cell comes from.
#[derive(Clone, Copy, Debug)]
enum Src {
    /// A `display: table-cell` element.
    Cell(NodeId),
    /// An anonymous cell: the siblings `first` to `last` under `parent` (CSS 2.1, 17.2.1).
    Run { parent: NodeId, first: NodeId, last: NodeId },
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    src: Src,
    /// The row (from the top of the table, in the order that they are shown) and the column where the cell starts.
    row: u32,
    col: u32,
    rows: u32,
    cols: u32,
    /// The used borders: a half edge in the collapsed model, else the borders of the style. Zero for an anonymous cell.
    border: Sides,
    /// The least height of the border box, from `height`.
    min_h: f32,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    node: Option<NodeId>,
    /// The cells of the row, in `Plan::cells`.
    cells: (u32, u32),
    /// The row in the table, in the order that they are shown.
    global: u32,
    /// The height that the row asks for (`height`), or 0.
    min_h: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Header,
    Body,
    Footer,
}

#[derive(Clone, Copy, Debug)]
struct Group {
    node: Option<NodeId>,
    rows: (u32, u32),
    cells: (u32, u32),
    place: Place,
}

/// What the columns need to know about one column.
#[derive(Clone, Copy, Debug, Default)]
struct Col {
    min: f32,
    max: f32,
    /// The percent that a cell or a `<col>` asks for: 0 for none.
    pct: f32,
    /// The width of the border box that a cell or a `<col>` asks for.
    fixed: Option<f32>,
}

/// Everything that layout and the sizes need to know about a table, found before any cell is laid out.
pub(crate) struct Plan {
    node: NodeId,
    cells: Vec<Cell>,
    rows: Vec<Row>,
    groups: Vec<Group>,
    /// The groups in the order that they are shown: the header, the bodies, the footer.
    order: Vec<u32>,
    /// The captions, with their side: `true` for the bottom.
    caps: Vec<(NodeId, bool)>,
    cols: Vec<Col>,
    nrows: u32,
    fixed: bool,
    /// The space between cells: (across, down). 0 in the collapsed model.
    spacing: (f32, f32),
    border: Sides,
    padding: Sides,
    /// The least and the most width of the content box of the table (the grid with its spaces, and the captions).
    min: f32,
    max: f32,
}

/// The cell, row, group or caption that a child of a table is.
#[derive(PartialEq, Clone, Copy)]
enum Part {
    Caption,
    Group(Place),
    Row,
    Cell,
    Other,
}

fn part(tree: &Tree, node: NodeId) -> Part {
    match tree.style(node).get(Property::Display) {
        Value::Keyword(Keyword::TableCaption) => Part::Caption,
        Value::Keyword(Keyword::TableHeaderGroup) => Part::Group(Place::Header),
        Value::Keyword(Keyword::TableRowGroup) => Part::Group(Place::Body),
        Value::Keyword(Keyword::TableFooterGroup) => Part::Group(Place::Footer),
        Value::Keyword(Keyword::TableRow) => Part::Row,
        Value::Keyword(Keyword::TableCell) => Part::Cell,
        _ => Part::Other,
    }
}

/// Finds the cells, rows and groups of a table.
struct Builder<'a> {
    tree: Tree<'a>,
    colspan: Option<Atom>,
    rowspan: Option<Atom>,
    cells: Vec<Cell>,
    rows: Vec<Row>,
    groups: Vec<Group>,
    caps: Vec<NodeId>,
    group: bool,
    row: bool,
    /// The rows of the open group so far.
    local: u32,
    /// The next column of the open row.
    col: usize,
    /// For each column: the row where it is free again, from the start of the group.
    busy: Vec<u32>,
    /// How far `busy` was written, so that a new group clears only that.
    touched: usize,
    ncols: usize,
    header: bool,
    footer: bool,
    /// The run of loose children that waits for its anonymous cell.
    run: Option<(NodeId, NodeId, NodeId)>,
}

impl Builder<'_> {
    fn span(&self, node: NodeId, name: Option<Atom>, max: u32) -> u32 {
        let v = name.and_then(|a| self.tree.doc.attr(node, a)).unwrap_or("");
        let v = v.trim_start();
        let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
        v[..end].parse::<u32>().unwrap_or(1).clamp(1, max)
    }

    fn open_group(&mut self, node: Option<NodeId>, place: Place) {
        self.close_group();
        let place = match place {
            Place::Header if !std::mem::replace(&mut self.header, true) => Place::Header,
            Place::Footer if !std::mem::replace(&mut self.footer, true) => Place::Footer,
            _ => Place::Body,
        };
        let (r, c) = (self.rows.len() as u32, self.cells.len() as u32);
        self.groups.push(Group { node, rows: (r, r), cells: (c, c), place });
        self.group = true;
        self.local = 0;
        self.busy[..self.touched].fill(0);
        self.touched = 0;
    }

    fn close_group(&mut self) {
        if !self.group {
            return;
        }
        self.flush();
        self.close_row();
        self.group = false;
        let g = self.groups.last_mut().expect("an open group");
        g.rows.1 = self.rows.len() as u32;
        g.cells.1 = self.cells.len() as u32;
        // A cell does not go past the end of its group.
        let total = self.local;
        for c in &mut self.cells[g.cells.0 as usize..] {
            c.rows = c.rows.min(total - c.row);
        }
    }

    fn open_row(&mut self, node: Option<NodeId>, min_h: f32) {
        if !self.group {
            self.open_group(None, Place::Body);
        }
        self.close_row();
        let c = self.cells.len() as u32;
        self.rows.push(Row { node, cells: (c, c), global: self.local, min_h });
        self.row = true;
        self.local += 1;
        self.col = 0;
    }

    fn close_row(&mut self) {
        if self.row {
            self.row = false;
            let n = self.cells.len() as u32;
            self.rows.last_mut().expect("an open row").cells.1 = n;
        }
    }

    fn add_cell(&mut self, src: Src, cols: u32, rows: u32) {
        if !self.row {
            self.open_row(None, 0.0);
        }
        while self.col < self.busy.len() && self.busy[self.col] > self.local - 1 {
            self.col += 1;
        }
        if self.col >= MAX_COLS {
            return;
        }
        let cols = cols.min((MAX_COLS - self.col) as u32);
        let (row, col) = (self.local - 1, self.col);
        if rows > 1 {
            if self.busy.len() < col + cols as usize {
                self.busy.resize(col + cols as usize, 0);
            }
            self.busy[col..col + cols as usize].fill(row + rows);
            self.touched = self.touched.max(col + cols as usize);
        }
        self.cells.push(Cell { src, row, col: col as u32, rows, cols, border: Sides::default(), min_h: 0.0 });
        self.col += cols as usize;
        self.ncols = self.ncols.max(self.col);
    }

    /// The loose children so far become an anonymous cell.
    fn flush(&mut self) {
        if let Some((parent, first, last)) = self.run.take() {
            self.add_cell(Src::Run { parent, first, last }, 1, 1);
        }
    }

    fn loose(&mut self, parent: NodeId, first: NodeId, last: NodeId) {
        match &mut self.run {
            Some((p, _, l)) if *p == parent => *l = last,
            _ => self.run = Some((parent, first, last)),
        }
    }

    /// Is this run of inline content only spaces that CSS throws away? Then it makes no cell.
    fn only_space(&self, first: NodeId, last: NodeId) -> bool {
        let tree = &self.tree;
        let mut n = first;
        loop {
            match tree.kind(n) {
                Kind::Text if tree.is_collapsible_space(n) => {}
                Kind::None => {}
                _ => return false,
            }
            if n == last {
                return true;
            }
            match tree.doc.flat_next(n) {
                Some(next) => n = next,
                None => return true,
            }
        }
    }

    /// The children of a row.
    fn row_children(&mut self, row: NodeId) {
        let tree = self.tree;
        for child in tree.children(row) {
            match child {
                Child::Lines(a, b) if !self.only_space(a, b) => self.loose(row, a, b),
                Child::Lines(..) => {}
                Child::Block(n) if part(&tree, n) == Part::Cell => {
                    self.flush();
                    let (c, r) = (self.span(n, self.colspan, MAX_COLSPAN), self.span(n, self.rowspan, MAX_ROWSPAN));
                    self.add_cell(Src::Cell(n), c, r);
                }
                Child::Block(n) => self.loose(row, n, n),
            }
        }
        self.flush();
    }

    /// The children of a row group, or of the table (`parent`).
    fn group_children(&mut self, parent: NodeId, in_table: bool) {
        let tree = self.tree;
        for child in tree.children(parent) {
            match child {
                Child::Lines(a, b) if !self.only_space(a, b) => self.loose(parent, a, b),
                Child::Lines(..) => {}
                Child::Block(n) => match part(&tree, n) {
                    Part::Caption if in_table => self.caps.push(n),
                    Part::Group(place) if in_table => {
                        self.flush();
                        self.open_group(Some(n), place);
                        self.group_children(n, false);
                        self.close_group();
                    }
                    Part::Row => {
                        self.flush();
                        let h = block_height(&tree, n);
                        self.open_row(Some(n), h);
                        self.row_children(n);
                        self.close_row();
                    }
                    Part::Cell => {
                        self.flush();
                        let (c, r) = (self.span(n, self.colspan, MAX_COLSPAN), self.span(n, self.rowspan, MAX_ROWSPAN));
                        self.add_cell(Src::Cell(n), c, r);
                    }
                    _ => self.loose(parent, n, n),
                },
            }
        }
        self.flush();
    }
}

/// The `height` of a row, if it is a length in pixels.
fn block_height(tree: &Tree, node: NodeId) -> f32 {
    match tree.style(node).get(Property::Height) {
        Value::Length(Length::Px(h)) => h.max(0.0),
        _ => 0.0,
    }
}

/// One border for the edge lists: `HIDDEN` for `hidden`, else the width (0 for `none`).
fn edge(style: &ComputedStyle, width: Property, line: Property) -> f32 {
    if style.get(line) == Value::Keyword(Keyword::Hidden) { HIDDEN } else { style.px(width) }
}

/// The four borders of a box: top, right, bottom, left.
fn edges(style: &ComputedStyle) -> [f32; 4] {
    use Property::*;
    [edge(style, BorderTopWidth, BorderTopStyle), edge(style, BorderRightWidth, BorderRightStyle), edge(style, BorderBottomWidth, BorderBottomStyle), edge(style, BorderLeftWidth, BorderLeftStyle)]
}

/// Two borders on one edge: the wider wins, and `hidden` beats all (CSS 2.1, 17.6.2.1).
fn merge(slot: &mut f32, w: f32) {
    if *slot != HIDDEN && (w == HIDDEN || w > *slot) {
        *slot = w;
    }
}

/// The width that an edge list entry gives: `hidden` is 0.
fn width_of(e: f32) -> f32 {
    e.max(0.0)
}

impl Plan {
    /// Find the structure and the columns of the table `node`. `cb`: the width of its containing block, for percent padding.
    pub(crate) fn new(cx: &mut Cx, node: NodeId, cb: f32) -> Plan {
        let tree = cx.tree;
        let style = tree.style(node);
        let mut b = Builder {
            tree,
            colspan: tree.doc.find_atom("colspan"),
            rowspan: tree.doc.find_atom("rowspan"),
            cells: Vec::new(),
            rows: Vec::new(),
            groups: Vec::new(),
            caps: Vec::new(),
            group: false,
            row: false,
            local: 0,
            col: 0,
            busy: Vec::new(),
            touched: 0,
            ncols: 0,
            header: false,
            footer: false,
            run: None,
        };
        b.group_children(node, true);
        b.close_group();
        let Builder { mut cells, mut rows, groups, caps, ncols, .. } = b;
        // The order that the groups are shown in, and the number of each row in it.
        let mut order: Vec<u32> = Vec::with_capacity(groups.len());
        order.extend((0..groups.len() as u32).filter(|&g| groups[g as usize].place == Place::Header));
        order.extend((0..groups.len() as u32).filter(|&g| groups[g as usize].place == Place::Body));
        order.extend((0..groups.len() as u32).filter(|&g| groups[g as usize].place == Place::Footer));
        let mut base = 0u32;
        for &g in &order {
            let g = groups[g as usize];
            for r in &mut rows[g.rows.0 as usize..g.rows.1 as usize] {
                r.global += base;
            }
            for c in &mut cells[g.cells.0 as usize..g.cells.1 as usize] {
                c.row += base;
            }
            base += g.rows.1 - g.rows.0;
        }
        let collapse = style.get(Property::BorderCollapse) == Value::Keyword(Keyword::Collapse) && (base as usize + 1).saturating_mul(ncols + 1) <= MAX_EDGES;
        let caps = caps.into_iter().map(|c| (c, tree.style(c).get(Property::CaptionSide) == Value::Keyword(Keyword::Bottom))).collect();
        let spacing = if collapse { (0.0, 0.0) } else { (style.px(Property::BorderSpacingH), style.px(Property::BorderSpacingV)) };
        let (border, padding) = block::border_padding(cx, style, cb);
        let mut plan = Plan { node, cells, rows, groups, order, caps, cols: vec![Col::default(); ncols], nrows: base, fixed: false, spacing, border, padding, min: 0.0, max: 0.0 };
        if collapse {
            plan.collapse_borders(cx, node);
        } else {
            for c in &mut plan.cells {
                if let Src::Cell(n) = c.src {
                    let (border, _) = block::border_padding(cx, tree.style(n), 0.0);
                    c.border = border;
                }
            }
        }
        plan.columns(cx, node);
        plan
    }

    /// The borders and the padding of the table box.
    pub(crate) fn sides(&self) -> (Sides, Sides) {
        (self.border, self.padding)
    }

    /// Resolve the borders of the collapsed model: the border of each cell, and of the table, are half of the edge around them.
    fn collapse_borders(&mut self, cx: &mut Cx, table: NodeId) {
        let tree = cx.tree;
        let (nr, nc) = (self.nrows as usize, self.cols.len());
        // `h[r * nc + c]`: the edge above the cell at row `r`, column `c`. `v[r * (nc + 1) + c]`: the edge to its left.
        let mut h = vec![0.0f32; (nr + 1) * nc];
        let mut v = vec![0.0f32; nr * (nc + 1)];
        let paint_h = |h: &mut [f32], line: usize, c0: usize, c1: usize, w: f32| h[line * nc + c0..line * nc + c1].iter_mut().for_each(|s| merge(s, w));
        let paint_v = |v: &mut [f32], r0: usize, r1: usize, line: usize, w: f32| (r0..r1).for_each(|r| merge(&mut v[r * (nc + 1) + line], w));
        for &g in &self.order {
            let g = self.groups[g as usize];
            let rows = &self.rows[g.rows.0 as usize..g.rows.1 as usize];
            if let (Some(n), Some(first), Some(last)) = (g.node, rows.first(), rows.last()) {
                let [t, r, b, l] = edges(tree.style(n));
                let (r0, r1) = (first.global as usize, last.global as usize + 1);
                paint_h(&mut h, r0, 0, nc, t);
                paint_h(&mut h, r1, 0, nc, b);
                paint_v(&mut v, r0, r1, 0, l);
                paint_v(&mut v, r0, r1, nc, r);
            }
            for row in rows {
                let r = row.global as usize;
                if let Some(n) = row.node {
                    let [t, rt, b, l] = edges(tree.style(n));
                    paint_h(&mut h, r, 0, nc, t);
                    paint_h(&mut h, r + 1, 0, nc, b);
                    paint_v(&mut v, r, r + 1, 0, l);
                    paint_v(&mut v, r, r + 1, nc, rt);
                }
            }
        }
        for c in &self.cells {
            let Src::Cell(n) = c.src else { continue };
            let [t, r, b, l] = edges(tree.style(n));
            let (r0, c0) = (c.row as usize, c.col as usize);
            let (r1, c1) = (r0 + c.rows as usize, c0 + c.cols as usize);
            paint_h(&mut h, r0, c0, c1, t);
            paint_h(&mut h, r1, c0, c1, b);
            paint_v(&mut v, r0, r1, c0, l);
            paint_v(&mut v, r0, r1, c1, r);
        }
        let [t, r, b, l] = edges(tree.style(table));
        if nr > 0 && nc > 0 {
            paint_h(&mut h, 0, 0, nc, t);
            paint_h(&mut h, nr, 0, nc, b);
            paint_v(&mut v, 0, nr, 0, l);
            paint_v(&mut v, 0, nr, nc, r);
        }
        // The widest edge along one side of a cell.
        let along_h = |line: usize, c0: usize, c1: usize| h[line * nc + c0..line * nc + c1].iter().copied().map(width_of).fold(0.0, f32::max);
        let along_v = |r0: usize, r1: usize, line: usize| (r0..r1).map(|r| width_of(v[r * (nc + 1) + line])).fold(0.0, f32::max);
        for c in &mut self.cells {
            if matches!(c.src, Src::Run { .. }) {
                continue;
            }
            let (r0, c0) = (c.row as usize, c.col as usize);
            let (r1, c1) = (r0 + c.rows as usize, c0 + c.cols as usize);
            // The half that goes inside the cell (Chrome keeps halves of a pixel).
            c.border = Sides { top: along_h(r0, c0, c1) / 2.0, bottom: along_h(r1, c0, c1) / 2.0, left: along_v(r0, r1, c0) / 2.0, right: along_v(r0, r1, c1) / 2.0 };
        }
        if nr > 0 && nc > 0 {
            self.border = Sides { top: along_h(0, 0, nc) / 2.0, bottom: along_h(nr, 0, nc) / 2.0, left: along_v(0, nr, 0) / 2.0, right: along_v(0, nr, nc) / 2.0 };
        } else {
            self.border = Sides::default();
        }
        // The padding of a table is not used in the collapsed model.
        self.padding = Sides::default();
    }

    /// The least and the most width of a cell, with its borders and padding, and the width and percent that it asks for.
    fn measure(&self, cx: &mut Cx, cell: &Cell) -> (f32, f32, f32, Option<f32>) {
        match cell.src {
            Src::Run { parent, first, last } => {
                let (min, max) = run_sizes(cx, parent, first, last);
                (min, max, 0.0, None)
            }
            Src::Cell(n) => {
                let style = cx.tree.style(n);
                let (_, padding) = block::border_padding(cx, style, 0.0);
                let bp = cell.border.horizontal() + padding.horizontal();
                let (min, max) = intrinsic::sizes(cx, n);
                let (min, max) = (min + bp, max + bp);
                let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
                match style.get(Property::Width) {
                    Value::Length(Length::Percent(p)) => (min, max, p, None),
                    v => match block::resolve(cx, v, None) {
                        // A cell with a width is as wide as it asks for, or as its longest word needs. Its text on
                        // one line does not count (as in Chrome and Firefox).
                        Some(w) => {
                            let w = if border_box { w.max(bp) } else { w + bp };
                            (min, w.max(min), 0.0, Some(w))
                        }
                        None => (min, max, 0.0, None),
                    },
                }
            }
        }
    }

    /// The widths of the columns, and with them the least and the most width of the table.
    fn columns(&mut self, cx: &mut Cx, table: NodeId) {
        let tree = cx.tree;
        let style = tree.style(table);
        let fixed_width = style.get(Property::Width) != Value::Keyword(Keyword::Auto);
        self.fixed = style.get(Property::TableLayout) == Value::Keyword(Keyword::Fixed) && fixed_width;
        let n = self.cols.len();
        // The widths that `<col>` asks for.
        let mut at = 0usize;
        let give = |cols: &mut [Col], node: NodeId, span: usize, at: &mut usize| {
            let (lo, hi) = ((*at).min(n), (*at + span).min(n));
            *at += span;
            let w = tree.style(node).get(Property::Width);
            for c in &mut cols[lo..hi] {
                match w {
                    Value::Length(Length::Percent(p)) if c.pct == 0.0 => c.pct = p,
                    Value::Length(Length::Px(px)) if c.fixed.is_none() => c.fixed = Some(px),
                    _ => {}
                }
            }
        };
        if n > 0 {
            let span_of = |node: NodeId| {
                let v = tree.doc.find_atom("span").and_then(|a| tree.doc.attr(node, a)).unwrap_or("").trim_start();
                let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
                v[..end].parse::<usize>().unwrap_or(1).clamp(1, MAX_COLSPAN as usize)
            };
            for child in tree.doc.flat_children(table) {
                match tree.styles.style(child).map(|s| s.get(Property::Display)) {
                    Some(Value::Keyword(Keyword::TableColumnGroup)) => {
                        let mut any = false;
                        for c in tree.doc.flat_children(child) {
                            if tree.styles.style(c).map(|s| s.get(Property::Display)) == Some(Value::Keyword(Keyword::TableColumn)) {
                                any = true;
                                give(&mut self.cols, c, span_of(c), &mut at);
                            }
                        }
                        if !any {
                            give(&mut self.cols, child, span_of(child), &mut at);
                        }
                    }
                    Some(Value::Keyword(Keyword::TableColumn)) => give(&mut self.cols, child, span_of(child), &mut at),
                    _ => {}
                }
            }
        }
        let hs = self.spacing.0;
        if self.fixed {
            // Only the first row says the widths. A cell over several columns shares its width between those without one.
            for i in 0..self.cells.len() {
                let c = self.cells[i];
                if c.row != 0 {
                    continue;
                }
                let (_, _, pct, fixed) = self.measure(cx, &c);
                let (lo, hi) = (c.col as usize, (c.col + c.cols) as usize);
                let w = fixed.map(|w| ((w - (c.cols - 1) as f32 * hs) / c.cols as f32).max(0.0));
                for col in &mut self.cols[lo..hi] {
                    if col.fixed.is_none() && col.pct == 0.0 {
                        if pct > 0.0 {
                            col.pct = pct / c.cols as f32;
                        } else {
                            col.fixed = w;
                        }
                    }
                }
            }
            for col in &mut self.cols {
                col.min = col.fixed.unwrap_or(0.0);
                col.max = col.min;
            }
        } else {
            let mut spans = Vec::new();
            for i in 0..self.cells.len() {
                let c = self.cells[i];
                let (min, max, pct, fixed) = self.measure(cx, &c);
                if c.cols == 1 {
                    let col = &mut self.cols[c.col as usize];
                    col.min = col.min.max(min);
                    col.max = col.max.max(max);
                    col.pct = col.pct.max(pct);
                    if let Some(f) = fixed {
                        col.fixed = Some(col.fixed.map_or(f, |x| x.max(f)));
                    }
                } else {
                    spans.push((i, min, max));
                }
            }
            for col in &mut self.cols {
                col.max = col.max.max(col.min);
            }
            // The cells that span columns come after, one at a time, and give what the columns do not have.
            let mut scratch: Vec<f32> = Vec::new();
            for (i, min, max) in spans {
                let c = self.cells[i];
                let (lo, hi) = (c.col as usize, (c.col + c.cols) as usize);
                let gaps = (c.cols - 1) as f32 * hs;
                scratch.resize(hi - lo, 0.0);
                if self.cols[lo..hi].iter().map(|c| c.min).sum::<f32>() + gaps < min {
                    distribute(min - gaps, &self.cols[lo..hi], &mut scratch);
                    for (col, &w) in self.cols[lo..hi].iter_mut().zip(&scratch) {
                        col.min = col.min.max(w);
                        col.max = col.max.max(col.min);
                    }
                }
                if self.cols[lo..hi].iter().map(|c| c.max).sum::<f32>() + gaps < max {
                    distribute(max - gaps, &self.cols[lo..hi], &mut scratch);
                    for (col, &w) in self.cols[lo..hi].iter_mut().zip(&scratch) {
                        col.max = col.max.max(w);
                    }
                }
            }
            for col in &mut self.cols {
                if let Some(f) = col.fixed {
                    col.max = f.max(col.min);
                }
            }
        }
        // The cells need their least height now, and the table its least and most width.
        for c in &mut self.cells {
            if let Src::Cell(n) = c.src {
                let style = tree.style(n);
                if let Value::Length(Length::Px(hh)) = style.get(Property::Height) {
                    let (_, padding) = block::border_padding(cx, style, 0.0);
                    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
                    c.min_h = if border_box { hh } else { hh + c.border.vertical() + padding.vertical() };
                }
            }
        }
        let gaps = if n == 0 { 0.0 } else { (n + 1) as f32 * hs };
        let min: f32 = self.cols.iter().map(|c| c.min).sum();
        // With percent columns, the table must be wide enough that each one has its percent of it (Firefox and Chrome).
        let (mut pct_total, mut rest, mut max) = (0.0f32, 0.0f32, 0.0f32);
        for c in &self.cols {
            if c.pct > 0.0 {
                pct_total += c.pct;
            } else {
                rest += c.max;
            }
        }
        let plain: f32 = self.cols.iter().map(|c| c.max).sum();
        max = max.max(plain);
        if pct_total > 0.0 {
            for c in self.cols.iter().filter(|c| c.pct > 0.0) {
                max = max.max(c.max * 100.0 / c.pct);
            }
            if pct_total < 100.0 {
                max = max.max(rest * 100.0 / (100.0 - pct_total));
            }
        }
        let mut cap_min = 0.0f32;
        for &(c, _) in &self.caps {
            cap_min = cap_min.max(intrinsic::outer(cx, c).0);
        }
        let bp = self.border.horizontal() + self.padding.horizontal();
        self.min = (min + gaps).max(cap_min - bp);
        self.max = (max + gaps).max(self.min);
    }

    /// The width of the content box of the table, and its margins. `forced`: what the caller decided (a box that shrinks
    /// to fit): it is not less than the least width of the table. `cb`: the width of the containing block. `bp`: the
    /// borders and padding of both sides.
    pub(crate) fn widths(&self, cx: &Cx, forced: Option<(f32, f32, f32)>, cb: f32, bp: f32) -> (f32, f32, f32) {
        if let Some((w, l, r)) = forced {
            return (w.max(self.min), l, r);
        }
        let style = cx.tree.style(self.node);
        let margin = |p| block::resolve(cx, style.get(p), Some(cb));
        let (ml, mr) = (margin(Property::MarginLeft), margin(Property::MarginRight));
        let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
        let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
        // With `width: auto`: as wide as the content wants, in the room that is left.
        let room = (cb - ml.unwrap_or(0.0) - mr.unwrap_or(0.0) - bp).max(0.0);
        let mut w = match block::resolve(cx, style.get(Property::Width), Some(cb)) {
            Some(w) => content(w),
            None => self.max.min(self.min.max(room)),
        };
        if let Some(hi) = block::resolve(cx, style.get(Property::MaxWidth), Some(cb)).map(content) {
            w = w.min(hi);
        }
        if let Some(lo) = block::resolve(cx, style.get(Property::MinWidth), Some(cb)).map(content) {
            w = w.max(lo);
        }
        block::solve_margins(ml, mr, cb, bp, Some(w.max(self.min)))
    }
}

/// How the extra width of a table goes to its columns. Four sizes for each column, and a column gets the part of each
/// step that fits, as in Firefox (`BasicTableLayoutStrategy`) and Chrome:
/// 0. every column at its least width;
/// 1. a column with a percent at that percent of the table;
/// 2. a column with a width at that width;
/// 3. every column at its most width.
///
/// Past 3, the extra goes to the auto columns in proportion to their most width (else to the fixed ones, else to the percent ones).
fn distribute(target: f32, cols: &[Col], out: &mut [f32]) {
    let total: f32 = cols.iter().map(|c| c.pct).sum();
    let scale = if total > 100.0 { 100.0 / total } else { 1.0 };
    let want = |c: &Col| if c.pct > 0.0 { (c.pct * scale * target / 100.0).max(c.min) } else { c.min };
    let level = |c: &Col, k: u8| match k {
        0 => c.min,
        1 => want(c),
        2 if c.pct > 0.0 => want(c),
        2 => c.fixed.map_or(c.min, |f| f.max(c.min)),
        _ if c.pct > 0.0 => want(c),
        _ => c.max.max(c.min),
    };
    let sum = |k: u8| cols.iter().map(|c| level(c, k)).sum::<f32>();
    let mut k = 0;
    let mut lo = sum(0);
    if target <= lo {
        for (o, c) in out.iter_mut().zip(cols) {
            *o = c.min;
        }
        return;
    }
    while k < 3 {
        let hi = sum(k + 1);
        if target <= hi {
            let t = if hi > lo { (target - lo) / (hi - lo) } else { 0.0 };
            for (o, c) in out.iter_mut().zip(cols) {
                let a = level(c, k);
                *o = a + (level(c, k + 1) - a) * t;
            }
            return;
        }
        k += 1;
        lo = hi;
    }
    // More than every column needs.
    let extra = target - lo;
    let weight = |c: &Col| if c.pct == 0.0 && c.fixed.is_none() { 1 } else if c.pct == 0.0 { 2 } else { 3 };
    let class = cols.iter().map(weight).min().unwrap_or(1);
    let share = |c: &Col| if weight(c) != class { 0.0 } else if class == 3 { c.pct } else { c.max };
    let mut all: f32 = cols.iter().map(share).sum();
    let equal = all <= 0.0;
    if equal {
        all = cols.iter().filter(|c| weight(c) == class).count() as f32;
    }
    for (o, c) in out.iter_mut().zip(cols) {
        let part = if equal { (weight(c) == class) as u8 as f32 } else { share(c) };
        *o = level(c, 3) + if all > 0.0 { extra * part / all } else { 0.0 };
    }
}

/// The least and the most width of an anonymous cell: the blocks and runs of lines of its siblings.
fn run_sizes(cx: &mut Cx, _parent: NodeId, first: NodeId, last: NodeId) -> (f32, f32) {
    let tree = cx.tree;
    let (mut min, mut max) = (0.0f32, 0.0f32);
    let mut each = |cx: &mut Cx, a: NodeId, b: NodeId, block: bool| {
        let (x, y) = if block { intrinsic::outer(cx, a) } else { intrinsic::lines(cx, a, b) };
        min = min.max(x);
        max = max.max(y);
    };
    let mut run: Option<(NodeId, NodeId)> = None;
    let mut n = Some(first);
    while let Some(c) = n {
        let kind = tree.kind(c);
        if kind.is_block() {
            if let Some((a, b)) = run.take() {
                each(cx, a, b, false);
            }
            each(cx, c, c, true);
        } else if kind.is_inline() {
            run = Some((run.map_or(c, |r| r.0), c));
        }
        n = if c == last { None } else { tree.doc.flat_next(c) };
    }
    if let Some((a, b)) = run {
        each(cx, a, b, false);
    }
    (min, max)
}

/// Lay out an anonymous cell, `width` wide: its blocks one under the other, and runs of lines. The fragments go in
/// `cx.out`, from the top-left of the cell. Gives the height and the baseline of the first line.
fn run_layout(cx: &mut Cx, parent: NodeId, first: NodeId, last: NodeId, width: f32) -> (f32, Option<f32>) {
    let tree = cx.tree;
    // The cell is its own BFC: it has its own floats.
    let outer = std::mem::take(&mut cx.floats);
    let (mut y, mut baseline) = (0.0f32, None);
    let lines = |cx: &mut Cx, a: NodeId, b: NodeId, y: &mut f32, baseline: &mut Option<f32>| {
        let from = cx.out.len();
        let l = inline::lines(cx, parent, a, b, width, (0.0, *y));
        if l.any {
            fragment::shift(&mut cx.out, from, 0.0, *y);
            if baseline.is_none() {
                *baseline = l.baseline.map(|b| *y + b);
            }
            *y += l.height;
        }
    };
    let mut run: Option<(NodeId, NodeId)> = None;
    let mut n = Some(first);
    while let Some(c) = n {
        let kind = tree.kind(c);
        if kind.is_block() {
            if let Some((a, b)) = run.take() {
                lines(cx, a, b, &mut y, &mut baseline);
            }
            let done = block::block(cx, c, Space { width, height: None }, Pos { x: 0.0, y, strut: Default::default() });
            cx.out[done.fragment].rect.y = y + done.margin_top;
            if baseline.is_none() {
                baseline = done.baseline.map(|b| y + done.margin_top + b);
            }
            y += done.margin_top + done.height + done.margin_bottom;
        } else if kind.is_inline() {
            run = Some((run.map_or(c, |r| r.0), c));
        }
        n = if c == last { None } else { tree.doc.flat_next(c) };
    }
    if let Some((a, b)) = run {
        lines(cx, a, b, &mut y, &mut baseline);
    }
    let height = cx.floats.bottom().map_or(y, |f| y.max(f));
    cx.floats = outer;
    (height, baseline)
}

/// The baseline of the first line inside the fragment `at`, from the top of its border box. `None` with no line.
fn first_baseline(cx: &Cx, at: usize, end: usize) -> Option<f32> {
    fragment::first_baseline(&cx.tree, &cx.out, at, end, true)
}

/// Where the content of a cell sits in its rows.
#[derive(Clone, Copy, PartialEq)]
enum Align {
    Top,
    Middle,
    Bottom,
    Baseline,
}

fn align_of(cx: &Cx, cell: &Cell) -> Align {
    let node = match cell.src {
        Src::Cell(n) => n,
        Src::Run { parent, .. } => parent,
    };
    match cx.tree.style(node).get(Property::VerticalAlign) {
        Value::Keyword(Keyword::Top) => Align::Top,
        Value::Keyword(Keyword::Middle) => Align::Middle,
        Value::Keyword(Keyword::Bottom) => Align::Bottom,
        _ => Align::Baseline,
    }
}

/// What layout found for a cell.
#[derive(Clone, Copy, Default)]
struct Laid {
    /// The fragment of the cell (`usize::MAX` for an anonymous one), and where its fragments start and end.
    frag: usize,
    from: usize,
    end: usize,
    /// The height of the border box, and the baseline from its top.
    h: f32,
    base: Option<f32>,
}

/// Lay out the content of a table in `space` (its content box), at the places that `plan` found. The fragments are
/// relative to the border box of the table: the content box starts at `bp.left`, `bp.top`.
pub(crate) fn layout(cx: &mut Cx, node: NodeId, space: Space, bp: Sides, plan: &Plan) -> Kids {
    let marker = cx.marker.take();
    let kids = layout_inner(cx, node, space, bp, plan);
    cx.marker = marker;
    kids
}

fn layout_inner(cx: &mut Cx, node: NodeId, space: Space, bp: Sides, plan: &Plan) -> Kids {
    let tree = cx.tree;
    let n = plan.cols.len();
    let (hs, vs) = plan.spacing;
    // The columns: x[j] is where column j starts, from the left edge of the border box.
    let gaps = if n == 0 { 0.0 } else { (n + 1) as f32 * hs };
    let mut width = vec![0.0f32; n];
    distribute((space.width - gaps).max(0.0), &plan.cols, &mut width);
    let mut x = Vec::with_capacity(n + 1);
    x.push(bp.left + if n == 0 { 0.0 } else { hs });
    for w in &width {
        x.push(x[x.len() - 1] + w + hs);
    }
    let grid_x = x[0];
    let grid_w = if n == 0 { 0.0 } else { x[n] - hs - grid_x };
    let table_w = space.width + bp.horizontal();

    // The captions above.
    let mut top = 0.0f32;
    for &(cap, bottom) in &plan.caps {
        if !bottom {
            let done = block::block(cx, cap, Space { width: table_w, height: None }, Pos::default());
            cx.out[done.fragment].rect.y = top + done.margin_top;
            top += done.margin_top + done.height + done.margin_bottom;
        }
    }

    // Each cell, laid out once.
    let mut laid = vec![Laid::default(); plan.cells.len()];
    let mut row_frag = vec![usize::MAX; plan.nrows as usize];
    let mut group_frag = vec![usize::MAX; plan.groups.len()];
    let open = |cx: &mut Cx, node: Option<NodeId>| match node {
        Some(node) => {
            cx.out.push(Fragment { rect: Rect::default(), node, kind: fragment::Kind::Box, end: 0 });
            cx.out.len() - 1
        }
        None => usize::MAX,
    };
    let close = |cx: &mut Cx, at: usize| {
        if at != usize::MAX {
            cx.out[at].end = cx.out.len() as u32;
        }
    };
    for &g in &plan.order {
        let group = plan.groups[g as usize];
        group_frag[g as usize] = open(cx, group.node);
        for row in &plan.rows[group.rows.0 as usize..group.rows.1 as usize] {
            row_frag[row.global as usize] = open(cx, row.node);
            for ci in row.cells.0..row.cells.1 {
                let cell = plan.cells[ci as usize];
                let (c0, c1) = (cell.col as usize, (cell.col + cell.cols) as usize);
                let cw = x[c1] - hs - x[c0];
                let l = &mut laid[ci as usize];
                l.from = cx.out.len();
                match cell.src {
                    Src::Cell(cn) => {
                        let style = tree.style(cn);
                        let (_, padding) = block::border_padding(cx, style, space.width);
                        let content = (cw - cell.border.horizontal() - padding.horizontal()).max(0.0);
                        let done = block::sized_with(cx, cn, Space { width: space.width, height: None }, Some((content, 0.0, 0.0)), None, Pos::default(), Some((cell.border, padding)));
                        l.frag = done.fragment;
                        l.h = done.height;
                        l.base = done.baseline;
                    }
                    Src::Run { parent, first, last } => {
                        let (h, base) = run_layout(cx, parent, first, last, cw);
                        l.frag = usize::MAX;
                        l.h = h;
                        l.base = base;
                    }
                }
                l.end = cx.out.len();
            }
            close(cx, row_frag[row.global as usize]);
        }
        close(cx, group_frag[g as usize]);
    }

    // The captions below.
    let mut below = Vec::new();
    for &(cap, bottom) in &plan.caps {
        if bottom {
            let done = block::block(cx, cap, Space { width: table_w, height: None }, Pos::default());
            below.push(done);
        }
    }

    // The heights of the rows. A cell that is aligned on a baseline needs the line of its first row: the row is as
    // high as its highest baseline plus its deepest part below.
    let nrows = plan.nrows as usize;
    let mut height = vec![0.0f32; nrows];
    let mut above = vec![0.0f32; nrows];
    let mut under = vec![0.0f32; nrows];
    for &g in &plan.order {
        let group = plan.groups[g as usize];
        for row in &plan.rows[group.rows.0 as usize..group.rows.1 as usize] {
            height[row.global as usize] = row.min_h;
        }
    }
    let mut baseline_of = vec![None::<f32>; plan.cells.len()];
    for (ci, cell) in plan.cells.iter().enumerate() {
        let l = laid[ci];
        let r = cell.row as usize;
        if cell.rows > 1 {
            continue;
        }
        let h = l.h.max(cell.min_h);
        if align_of(cx, cell) == Align::Baseline {
            let base = if l.frag == usize::MAX { l.base } else { first_baseline(cx, l.frag, l.end) };
            // A cell with no line: its bottom edge is its baseline.
            let base = base.unwrap_or(h);
            baseline_of[ci] = Some(base);
            above[r] = above[r].max(base);
            under[r] = under[r].max(h - base);
        } else {
            height[r] = height[r].max(h);
        }
    }
    for r in 0..nrows {
        height[r] = height[r].max(above[r] + under[r]);
    }
    // A cell over several rows: what its rows lack is shared between them in proportion to their height.
    for (ci, cell) in plan.cells.iter().enumerate() {
        if cell.rows <= 1 {
            continue;
        }
        let (r0, r1) = (cell.row as usize, (cell.row + cell.rows) as usize);
        let have = height[r0..r1].iter().sum::<f32>() + (cell.rows - 1) as f32 * vs;
        let need = laid[ci].h.max(cell.min_h);
        if need > have {
            let sum: f32 = height[r0..r1].iter().sum();
            let extra = need - have;
            for h in &mut height[r0..r1] {
                *h += if sum > 0.0 { extra * *h / sum } else { extra / cell.rows as f32 };
            }
        }
    }
    // The height of the table: it asks for a least height, and the rows share what they lack.
    let rows_h = |height: &[f32]| if nrows == 0 { 0.0 } else { height.iter().sum::<f32>() + (nrows + 1) as f32 * vs };
    let style = tree.style(node);
    let wanted = block::resolve(cx, style.get(Property::Height), space.height).map(|h| if style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox) { (h - bp.vertical()).max(0.0) } else { h });
    if let Some(want) = wanted {
        let have = rows_h(&height);
        let want = want - top - below.iter().map(|d| d.margin_top + d.height + d.margin_bottom).sum::<f32>();
        if nrows > 0 && want > have {
            let sum: f32 = height.iter().sum();
            let extra = want - have;
            for h in &mut height {
                *h += if sum > 0.0 { extra * *h / sum } else { extra / nrows as f32 };
            }
        }
    }
    // Where each row starts, from the top of the border box of the table.
    let mut y = Vec::with_capacity(nrows);
    let mut at = top + bp.top + if nrows == 0 { 0.0 } else { vs };
    for h in &height {
        y.push(at);
        at += h + vs;
    }
    let grid_h = rows_h(&height);

    // The rectangles of the groups, rows and cells, and the content of the cells in them.
    let mut table_baseline = None;
    for &g in &plan.order {
        let group = plan.groups[g as usize];
        let rows = &plan.rows[group.rows.0 as usize..group.rows.1 as usize];
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else { continue };
        let (gy, gend) = (y[first.global as usize], y[last.global as usize] + height[last.global as usize]);
        // The place that the cells of a row hang from: the row, else the group, else the table.
        let gf = group_frag[g as usize];
        if gf != usize::MAX {
            cx.out[gf].rect = Rect { x: grid_x, y: gy, w: grid_w, h: gend - gy };
        }
        let (gox, goy) = if gf != usize::MAX { (grid_x, gy) } else { (0.0, 0.0) };
        for row in rows {
            let r = row.global as usize;
            let rf = row_frag[r];
            if rf != usize::MAX {
                cx.out[rf].rect = Rect { x: grid_x - gox, y: y[r] - goy, w: grid_w, h: height[r] };
            }
            let (ox, oy) = if rf != usize::MAX { (grid_x, y[r]) } else { (gox, goy) };
            for ci in row.cells.0..row.cells.1 {
                let cell = plan.cells[ci as usize];
                let l = laid[ci as usize];
                let (r0, r1) = (cell.row as usize, (cell.row + cell.rows) as usize);
                let h = height[r0..r1].iter().sum::<f32>() + (cell.rows - 1) as f32 * vs;
                let natural = l.h.max(cell.min_h);
                let dy = match align_of(cx, &cell) {
                    Align::Top => 0.0,
                    Align::Middle => (h - natural) / 2.0,
                    Align::Bottom => h - natural,
                    Align::Baseline if cell.rows == 1 => baseline_of[ci as usize].map_or(0.0, |b| above[r0] - b),
                    Align::Baseline => 0.0,
                };
                let (cx0, cy0) = (x[cell.col as usize] - ox, y[r0] - oy);
                if l.frag != usize::MAX {
                    // The content moves down by `dy`; the cell fills its rows.
                    if dy != 0.0 {
                        fragment::shift(&mut cx.out[..l.end], l.frag + 1, 0.0, dy);
                    }
                    let f = &mut cx.out[l.frag];
                    f.rect.x = cx0;
                    f.rect.y = cy0;
                    f.rect.h = h;
                } else {
                    fragment::shift(&mut cx.out[..l.end], l.from, cx0, cy0 + dy);
                }
                // The baseline of the table is that of the first row with a cell on a baseline (Chrome). With none, the
                // table has no baseline, and an inline table sits on its bottom edge.
                if table_baseline.is_none() && baseline_of[ci as usize].is_some() {
                    table_baseline = Some(y[r0] + above[r0]);
                }
            }
        }
    }
    // The captions below go under the table.
    let box_bottom = top + bp.top + grid_h + bp.bottom;
    let mut under_h = 0.0;
    for d in &below {
        cx.out[d.fragment].rect.y = box_bottom + under_h + d.margin_top;
        under_h += d.margin_top + d.height + d.margin_bottom;
    }
    let content = top + grid_h + under_h;
    Kids::of(content, table_baseline.map(|b| b - bp.top))
}

/// The least and the most width of the content box of a table.
pub(crate) fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    let plan = Plan::new(cx, node, 0.0);
    // The parent counts the table with the borders and padding of its style: the collapsed model has other ones.
    let (b, p) = block::border_padding(cx, cx.tree.style(node), 0.0);
    let own = b.horizontal() + p.horizontal();
    let used = plan.border.horizontal() + plan.padding.horizontal();
    ((plan.min + used - own).max(0.0), (plan.max + used - own).max(0.0))
}

#[cfg(test)]
mod tests {
    use crate::block::tests::{dump, rect, run};
    use crate::fragment::Rect;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A box of a fixed size: the tests do not depend on the width of letters.
    const CSS: &str = "td { padding: 0 } .a { width: 30px; height: 10px } .b { width: 50px; height: 20px }";

    #[test]
    fn a_cell_width_caps_its_column_below_the_content() {
        // "wraps in sixty px" is 163.2 on one line. The cell asks for 60, and its longest word ("sixty", 48) fits:
        // the column is 60, also next to a column without a width.
        let html = "<table><tr><td id=c style='width:60px'>wraps in sixty px</td></tr></table>";
        assert_eq!(rect(html, "td { padding: 0 }", "c").w, 60.0);
        let html = "<table><tr><td>x</td><td id=c style='width:60px'>wraps in sixty px</td></tr></table>";
        assert_eq!(rect(html, "td { padding: 0 }", "c").w, 60.0);
    }

    #[test]
    fn auto_table_shrinks_to_its_cells_with_spacing() {
        // Spacing 2px: 2 + 30 + 2 + 50 + 2. The row is 20 high, so the table is 2 + 20 + 2.
        let html = "<table id=t><tr id=r><td id=a><div class=a></div><td id=b><div class=b></div></table>";
        assert_eq!(rect(html, CSS, "t"), r(8.0, 8.0, 86.0, 24.0));
        assert_eq!(rect(html, CSS, "r"), r(10.0, 10.0, 82.0, 20.0));
        assert_eq!(rect(html, CSS, "a"), r(10.0, 10.0, 30.0, 20.0));
        assert_eq!(rect(html, CSS, "b"), r(42.0, 10.0, 50.0, 20.0));
    }

    #[test]
    fn fixed_table_width_spreads_the_extra_over_the_columns() {
        // Chrome: width 200, two auto columns with max 30 and 50: the extra goes in proportion to the max widths.
        let html = "<table id=t style='width:200px'><tr><td id=a><div class=a></div><td id=b><div class=b></div></table>";
        let (a, b) = (rect(html, CSS, "a"), rect(html, CSS, "b"));
        assert_eq!(rect(html, CSS, "t").w, 200.0);
        assert!((a.w + b.w - 194.0).abs() < 0.01, "{a:?} {b:?}");
        assert!((a.w / b.w - 30.0 / 50.0).abs() < 0.01, "{a:?} {b:?}");
    }

    #[test]
    fn percent_and_fixed_columns() {
        // Chrome (a1/b1): a cell with 20% in a table of 600 gets 20% of the table, and the other columns share the rest.
        let html = "<table id=t style='width:600px;border-spacing:0'><tr><td id=a style='width:20%'>x<td id=b style='width:100px'>y<td id=c>z</table>";
        let (a, b, c) = (rect(html, CSS, "a"), rect(html, CSS, "b"), rect(html, CSS, "c"));
        assert_eq!(a.w, 120.0);
        assert_eq!(b.w, 100.0);
        assert_eq!(c.w, 380.0);
    }

    #[test]
    fn colspan_spreads_over_its_columns() {
        let html = "<table id=t style='border-spacing:0'><tr><td id=w colspan=2><div style='width:100px;height:5px'></div><tr><td id=a><div class=a></div><td id=b><div class=a></div></table>";
        let (w, a, b) = (rect(html, CSS, "w"), rect(html, CSS, "a"), rect(html, CSS, "b"));
        assert_eq!(w.w, 100.0);
        assert_eq!(a.w + b.w, 100.0);
        assert_eq!(a.w, b.w);
    }

    #[test]
    fn rowspan_shares_its_height_with_the_rows() {
        // The tall cell is 2 + 40 + 2 high at least: the two rows get 20 each, with 2px spacing between them.
        let html = "<table id=t><tr><td id=a rowspan=2><div style='height:42px;width:5px'></div><td id=b><div class=a></div><tr><td id=c><div class=a></div></table>";
        let (a, b, c) = (rect(html, CSS, "a"), rect(html, CSS, "b"), rect(html, CSS, "c"));
        assert_eq!(a.h, 42.0);
        assert_eq!(b.y, a.y);
        assert!((c.y + c.h - (a.y + a.h)).abs() < 0.01, "{a:?} {c:?}");
        assert!((b.h + 2.0 + c.h - 42.0).abs() < 0.01, "{b:?} {c:?}");
    }

    #[test]
    fn fixed_layout_ignores_the_content_of_later_rows() {
        // Chrome (a1/t5): `table-layout: fixed`, width 400, first cell 50 wide: the other two share the rest.
        let html = "<table id=t style='table-layout:fixed;width:400px;border-spacing:0'><tr><td id=a style='width:50px'>a<td id=b><div style='width:900px'></div><td id=c>c</table>";
        let (a, b, c) = (rect(html, CSS, "a"), rect(html, CSS, "b"), rect(html, CSS, "c"));
        assert_eq!((a.w, b.w, c.w), (50.0, 175.0, 175.0));
    }

    #[test]
    fn vertical_align_in_a_cell() {
        let html = "<table id=t style='border-spacing:0'><tr style='height:40px'><td id=a style='vertical-align:top'><div id=x class=a></div><td id=b style='vertical-align:bottom'><div id=y class=a></div><td id=c style='vertical-align:middle'><div id=z class=a></div></table>";
        let (x, y, z) = (rect(html, CSS, "x"), rect(html, CSS, "y"), rect(html, CSS, "z"));
        assert_eq!((x.y, y.y, z.y), (8.0, 38.0, 23.0));
    }

    #[test]
    fn caption_goes_above_or_below() {
        let html = "<table id=t><caption id=c style='height:10px'></caption><tr><td id=a><div class=a></div></table>";
        // The box of the table holds its captions. The cell is below the caption.
        let (t, c, a) = (rect(html, CSS, "t"), rect(html, CSS, "c"), rect(html, CSS, "a"));
        assert_eq!((t.y, c.y, a.y), (8.0, 8.0, 20.0));
        let html = "<table id=t style='caption-side:bottom'><caption id=c style='height:10px'></caption><tr><td><div class=a></div></table>";
        let (t, c) = (rect(html, CSS, "t"), rect(html, CSS, "c"));
        assert_eq!(t.y, 8.0);
        assert_eq!(c.y, t.y + t.h - 10.0);
    }

    #[test]
    fn thead_is_first_and_tfoot_is_last() {
        let html = "<table><tfoot><tr><td id=f>f</tfoot><tbody><tr><td id=b>b</tbody><thead><tr><td id=h>h</thead></table>";
        let (f, b, h) = (rect(html, CSS, "f"), rect(html, CSS, "b"), rect(html, CSS, "h"));
        assert!(h.y < b.y && b.y < f.y, "{h:?} {b:?} {f:?}");
    }

    #[test]
    fn colgroup_width_sets_the_column() {
        let html = "<table style='border-spacing:0'><colgroup><col style='width:70px'><col></colgroup><tr><td id=a>a<td id=b>b</table>";
        assert_eq!(rect(html, CSS, "a").w, 70.0);
    }

    #[test]
    fn collapsed_borders_use_half_edges() {
        // Chrome (a3): one 1px border on each side, the table has the half of it, and so has each cell: 0.5 px.
        let html = "<table id=t style='border-collapse:collapse'><tr><td id=a style='border:1px solid'><div class=a></div><td id=b style='border:1px solid'><div class=a></div></table>";
        let (t, a, b) = (rect(html, CSS, "t"), rect(html, CSS, "a"), rect(html, CSS, "b"));
        // Two cells of 30 and three borders of 1: 63 wide. The cells start after the half border of the table.
        assert_eq!(t.w, 63.0);
        assert_eq!(a.x, 8.5);
        assert_eq!(a.w, 31.0);
        assert_eq!(b.x, a.x + a.w);
    }

    #[test]
    fn collapsed_edge_takes_the_wider_border() {
        let html = "<table style='border-collapse:collapse'><tr><td id=a style='border:2px solid'><div class=a></div><td id=b style='border:6px solid'><div class=a></div></table>";
        let (a, b) = (rect(html, CSS, "a"), rect(html, CSS, "b"));
        // The shared edge is 6 wide: 3 inside each cell. The outer edge of `a` is 2: 1 inside the cell.
        assert_eq!(a.w, 1.0 + 30.0 + 3.0);
        assert_eq!(b.x, a.x + a.w);
    }

    #[test]
    fn border_spacing_of_two_values() {
        let html = "<table id=t style='border-spacing:10px 4px'><tr><td id=a><div class=a></div><td id=b><div class=a></div><tr><td id=c><div class=a></div></table>";
        let (a, b, c) = (rect(html, CSS, "a"), rect(html, CSS, "b"), rect(html, CSS, "c"));
        assert_eq!(b.x - a.x - a.w, 10.0);
        assert_eq!(c.y - a.y - a.h, 4.0);
    }

    #[test]
    fn table_height_grows_the_rows() {
        let html = "<table id=t style='height:200px'><tr><td id=a><div class=a></div><tr><td id=b><div class=a></div></table>";
        let (t, a, b) = (rect(html, CSS, "t"), rect(html, CSS, "a"), rect(html, CSS, "b"));
        assert_eq!(t.h, 200.0);
        assert_eq!(a.h, b.h);
    }

    #[test]
    fn loose_cells_get_an_anonymous_row() {
        let html = "<div id=d style='display:table'><div id=a style='display:table-cell'>a</div><div id=b style='display:table-cell'>b</div></div>";
        let (a, b) = (rect(html, CSS, "a"), rect(html, CSS, "b"));
        assert_eq!(a.y, b.y);
        assert!(b.x > a.x + a.w - 0.01);
    }

    #[test]
    fn inline_table_sits_in_a_line() {
        let html = "<div>x <table id=t style='display:inline-table'><tr><td><div class=a></div></table> y</div>";
        let (doc, layout) = run(html, CSS);
        let out = crate::dump(&doc, &layout);
        assert!(out.contains("table#t"), "{out}");
        let t = rect(html, CSS, "t");
        assert!(t.x > 8.0, "{t:?}");
    }

    #[test]
    fn spans_that_are_too_big_do_not_hang() {
        let out = dump("<table><tr><td colspan=999999 rowspan=999999>a<td>b</table>", "");
        assert!(out.contains("td"), "{out}");
    }
}

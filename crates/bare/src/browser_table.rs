//! The members of `HTMLTableElement`, `HTMLTableSectionElement`, `HTMLTableRowElement` and `HTMLTableCellElement` (HTML Standard "Tables").
//! Cause: pages read `table.rows` and `tr.cells`; they were `undefined`.
//!
//! - `table.rows`: the `tr` children of each `thead` child, then the `tr` children of the table and of each `tbody` child in tree order, then
//!   the `tr` children of each `tfoot` child. `table.tBodies`: the `tbody` children. `section.rows`: the `tr` children. `tr.cells`: the `td`
//!   and `th` children. Each is one live `HTMLCollection` (`browser_nodelist::of`).
//! - `tHead`, `tFoot`, `caption`: the first such child, else `null`. They have no setters (deviation).
//! - `insertRow`, `deleteRow`, `insertCell`, `deleteCell`, `createTHead`, `createTFoot`, `createTBody`, `createCaption`, `deleteTHead`,
//!   `deleteTFoot`, `deleteCaption` follow the standard steps. An index out of range is an `IndexSizeError`. The message text is bare's own.
//! - `rowIndex`, `sectionRowIndex`, `cellIndex`: the index in the collection of the parent, else -1.
//! - Cells reflect `colSpan` (clamped to 1..1000, default 1), `rowSpan` (0..65534, default 1), `headers`, `abbr` and `scope` (the four words,
//!   else `""`).

use crate::browser::*;
use crate::browser_platform::arg;
use crate::browser_reflect::{reflect, this_of};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

/// The kinds of `Src::Of` lists of this module. `FORM` is `form.elements` (`browser_form.rs`).
const ROWS: u8 = 0;
const BODIES: u8 = 1;
const SECTION_ROWS: u8 = 2;
const CELLS: u8 = 3;
pub(crate) const FORM: u8 = 4;

const SECTIONS: &[&str] = &["thead", "tbody", "tfoot"];

/// The HTML children of `n` whose tag is one of `tags`.
fn kids(doc: &Document, n: NodeId, tags: &[&str]) -> Vec<NodeId> {
    doc.children(n).filter(|&c| tags.iter().any(|t| doc.is_html(c, t))).collect()
}

fn table_rows(doc: &Document, t: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for c in kids(doc, t, &["thead"]) {
        out.extend(kids(doc, c, &["tr"]));
    }
    for c in doc.children(t) {
        if doc.is_html(c, "tr") {
            out.push(c);
        } else if doc.is_html(c, "tbody") {
            out.extend(kids(doc, c, &["tr"]));
        }
    }
    for c in kids(doc, t, &["tfoot"]) {
        out.extend(kids(doc, c, &["tr"]));
    }
    out
}

/// The members of the list `kind` of `n` (`browser_nodelist.rs` calls it to rebuild a stale list).
pub(crate) fn ids(d: &Dom, it: &mut Interp<'_>, n: NodeId, kind: u8) -> R<Vec<NodeId>> {
    if kind == FORM {
        return d.form_elements(it, n);
    }
    let doc = d.doc.borrow();
    // A table walks its children and the children of its sections; charge both.
    it.charge(1 + doc.children(n).count() as u64)?;
    Ok(match kind {
        ROWS => {
            it.charge(kids(&doc, n, SECTIONS).iter().map(|&s| doc.children(s).count() as u64).sum())?;
            table_rows(&doc, n)
        }
        BODIES => kids(&doc, n, &["tbody"]),
        SECTION_ROWS => kids(&doc, n, &["tr"]),
        _ => kids(&doc, n, &["td", "th"]),
    })
}

/// The `long index` argument (default -1).
fn index(it: &mut Interp<'_>, a: &[Value]) -> R<i32> {
    match arg(a, 0) {
        Value::Undefined => Ok(-1),
        v => Ok(bare_js::value::to_int32(it.to_js_number(v)?)),
    }
}

fn range(d: &Dom, it: &mut Interp<'_>, what: &str, on: &str, i: i32, len: usize, close: char) -> bare_js::eval::Error {
    d.dom_err(it, "IndexSizeError", &format!("Failed to execute '{what}' on '{on}': The index provided ({i}) is outside the range [-1, {len}{close}."))
}

/// A new HTML element `name`, inserted into `parent` before `before`.
fn add(d: &Dom, it: &mut Interp<'_>, parent: NodeId, name: &str, before: Option<NodeId>) -> R<NodeId> {
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, 0)?;
        doc.create_element(name)
    };
    d.insert_checked(it, parent, n, before, crate::browser_dom::NO_OP)?;
    Ok(n)
}

/// `insertRow` of a section and `insertCell`: a new `name` at `index` among `list`, children of `parent`.
#[allow(clippy::too_many_arguments)]
fn insert_at(d: &Dom, it: &mut Interp<'_>, parent: NodeId, list: &[NodeId], a: &[Value], name: &str, what: &str, on: &str) -> R<Value> {
    let i = index(it, a)?;
    if i < -1 || i > list.len() as i32 {
        return Err(range(d, it, what, on, i, list.len(), ']'));
    }
    let before = if i == -1 { None } else { list.get(i as usize).copied() };
    let n = add(d, it, parent, name, before)?;
    d.wrap(it, n)
}

/// `deleteRow` and `deleteCell`: remove member `index` of `list` (-1: the last one, if any).
fn delete_at(d: &Dom, it: &mut Interp<'_>, list: &[NodeId], a: &[Value], what: &str, on: &str) -> R<Value> {
    let i = index(it, a)?;
    let victim = if i == -1 { list.last().copied() } else if i < 0 || i >= list.len() as i32 { return Err(range(d, it, what, on, i, list.len(), ')')) } else { Some(list[i as usize]) };
    if let Some(v) = victim {
        d.doc.borrow_mut().detach(v);
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ table

fn table(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    this_of(d, it, this, &["table"])
}

fn first(d: &Dom, it: &mut Interp<'_>, this: Value, tag: &str) -> R<Value> {
    let t = table(d, it, this)?;
    let c = kids(&d.doc.borrow(), t, &[tag]).first().copied();
    d.wrap_opt(it, c)
}

fn insert_row(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let t = table(d, it, this)?;
    let i = index(it, a)?;
    let rows = ids(d, it, t, ROWS)?;
    if i < -1 || i > rows.len() as i32 {
        return Err(range(d, it, "insertRow", "HTMLTableElement", i, rows.len(), ']'));
    }
    let n = match rows.last() {
        None => {
            let body = kids(&d.doc.borrow(), t, &["tbody"]).last().copied();
            let body = match body {
                Some(b) => b,
                None => add(d, it, t, "tbody", None)?,
            };
            add(d, it, body, "tr", None)?
        }
        Some(&last) if i == -1 || i == rows.len() as i32 => {
            let p = d.doc.borrow().parent(last).expect("a row has a parent");
            add(d, it, p, "tr", None)?
        }
        Some(_) => {
            let at = rows[i as usize];
            let p = d.doc.borrow().parent(at).expect("a row has a parent");
            add(d, it, p, "tr", Some(at))?
        }
    };
    d.wrap(it, n)
}

fn delete_row(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let t = table(d, it, this)?;
    let rows = ids(d, it, t, ROWS)?;
    delete_at(d, it, &rows, a, "deleteRow", "HTMLTableElement")
}

/// `createTHead` (`FOOT` false) or `createTFoot`: the first such child, else a new one.
fn create_section<const FOOT: bool>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let t = table(d, it, this)?;
    let tag = if FOOT { "tfoot" } else { "thead" };
    let (found, before) = {
        let doc = d.doc.borrow();
        let found = kids(&doc, t, &[tag]).first().copied();
        // A new `thead` goes before the first child element that is not a `caption` or a `colgroup`.
        let before = doc.children(t).find(|&c| matches!(doc.data(c), NodeData::Element { .. }) && !doc.is_html(c, "caption") && !doc.is_html(c, "colgroup"));
        (found, before)
    };
    let n = match found {
        Some(n) => n,
        None => add(d, it, t, tag, if FOOT { None } else { before })?,
    };
    d.wrap(it, n)
}

fn create_tbody(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let t = table(d, it, this)?;
    let before = {
        let doc = d.doc.borrow();
        kids(&doc, t, &["tbody"]).last().and_then(|&b| doc.next_sibling(b))
    };
    let n = add(d, it, t, "tbody", before)?;
    d.wrap(it, n)
}

fn create_caption(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let t = table(d, it, this)?;
    let (found, head) = {
        let doc = d.doc.borrow();
        (kids(&doc, t, &["caption"]).first().copied(), doc.children(t).next())
    };
    let n = match found {
        Some(n) => n,
        None => add(d, it, t, "caption", head)?,
    };
    d.wrap(it, n)
}

/// `deleteTHead`, `deleteTFoot`, `deleteCaption`: remove the first child `tag`.
fn delete_first(d: &Dom, it: &mut Interp<'_>, this: Value, tag: &str) -> R<Value> {
    let t = table(d, it, this)?;
    let c = kids(&d.doc.borrow(), t, &[tag]).first().copied();
    if let Some(c) = c {
        d.doc.borrow_mut().detach(c);
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ section, row, cell

fn section_insert_row(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let s = this_of(d, it, this, SECTIONS)?;
    let rows = ids(d, it, s, SECTION_ROWS)?;
    insert_at(d, it, s, &rows, a, "tr", "insertRow", "HTMLTableSectionElement")
}

fn section_delete_row(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let s = this_of(d, it, this, SECTIONS)?;
    let rows = ids(d, it, s, SECTION_ROWS)?;
    delete_at(d, it, &rows, a, "deleteRow", "HTMLTableSectionElement")
}

fn insert_cell(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let r = this_of(d, it, this, &["tr"])?;
    let cells = ids(d, it, r, CELLS)?;
    insert_at(d, it, r, &cells, a, "td", "insertCell", "HTMLTableRowElement")
}

fn delete_cell(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let r = this_of(d, it, this, &["tr"])?;
    let cells = ids(d, it, r, CELLS)?;
    delete_at(d, it, &cells, a, "deleteCell", "HTMLTableRowElement")
}

fn position(list: &[NodeId], n: NodeId) -> Value {
    Value::Number(list.iter().position(|&x| x == n).map_or(-1.0, |i| i as f64))
}

/// `rowIndex` (`SECTION` false: in the rows of the table) or `sectionRowIndex` (in the rows of the parent).
fn row_index<const SECTION: bool>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let r = this_of(d, it, this, &["tr"])?;
    let (p, gp) = {
        let doc = d.doc.borrow();
        let p = doc.parent(r);
        (p, p.and_then(|p| doc.parent(p)))
    };
    let Some(p) = p else { return Ok(Value::Number(-1.0)) };
    let (is_table, is_section, gp_table) = {
        let doc = d.doc.borrow();
        (doc.is_html(p, "table"), SECTIONS.iter().any(|t| doc.is_html(p, t)), gp.is_some_and(|g| doc.is_html(g, "table")))
    };
    let list = if is_table {
        ids(d, it, p, ROWS)?
    } else if is_section && SECTION {
        ids(d, it, p, SECTION_ROWS)?
    } else if is_section && gp_table {
        ids(d, it, gp.expect("checked"), ROWS)?
    } else {
        Vec::new()
    };
    Ok(position(&list, r))
}

fn cell_index(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let c = this_of(d, it, this, &["td", "th"])?;
    let p = d.doc.borrow().parent(c).filter(|&p| d.doc.borrow().is_html(p, "tr"));
    let Some(p) = p else { return Ok(Value::Number(-1.0)) };
    let cells = ids(d, it, p, CELLS)?;
    Ok(position(&cells, c))
}

// ------------------------------------------------------------------------------------------ install

/// Which prototype `members` fills.
pub(crate) enum Proto {
    Table,
    Section,
    Row,
    Cell,
}

pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, o: ObjId, which: Proto) -> R<()> {
    match which {
        Proto::Table => {
            d.def_get(it, o, "rows", |d, it, t, _| { let n = table(d, it, t)?; crate::browser_nodelist::of(d, it, n, ROWS) }, None)?;
            d.def_get(it, o, "tBodies", |d, it, t, _| { let n = table(d, it, t)?; crate::browser_nodelist::of(d, it, n, BODIES) }, None)?;
            d.def_get(it, o, "tHead", |d, it, t, _| first(d, it, t, "thead"), None)?;
            d.def_get(it, o, "tFoot", |d, it, t, _| first(d, it, t, "tfoot"), None)?;
            d.def_get(it, o, "caption", |d, it, t, _| first(d, it, t, "caption"), None)?;
            d.def_method(it, o, "insertRow", 0, insert_row)?;
            d.def_method(it, o, "deleteRow", 1, delete_row)?;
            d.def_method(it, o, "createTHead", 0, create_section::<false>)?;
            d.def_method(it, o, "createTFoot", 0, create_section::<true>)?;
            d.def_method(it, o, "createTBody", 0, create_tbody)?;
            d.def_method(it, o, "createCaption", 0, create_caption)?;
            d.def_method(it, o, "deleteTHead", 0, |d, it, t, _| delete_first(d, it, t, "thead"))?;
            d.def_method(it, o, "deleteTFoot", 0, |d, it, t, _| delete_first(d, it, t, "tfoot"))?;
            d.def_method(it, o, "deleteCaption", 0, |d, it, t, _| delete_first(d, it, t, "caption"))?;
        }
        Proto::Section => {
            d.def_get(it, o, "rows", |d, it, t, _| { let n = this_of(d, it, t, SECTIONS)?; crate::browser_nodelist::of(d, it, n, SECTION_ROWS) }, None)?;
            d.def_method(it, o, "insertRow", 0, section_insert_row)?;
            d.def_method(it, o, "deleteRow", 1, section_delete_row)?;
        }
        Proto::Row => {
            d.def_get(it, o, "cells", |d, it, t, _| { let n = this_of(d, it, t, &["tr"])?; crate::browser_nodelist::of(d, it, n, CELLS) }, None)?;
            d.def_get(it, o, "rowIndex", row_index::<false>, None)?;
            d.def_get(it, o, "sectionRowIndex", row_index::<true>, None)?;
            d.def_method(it, o, "insertCell", 0, insert_cell)?;
            d.def_method(it, o, "deleteCell", 1, delete_cell)?;
        }
        Proto::Cell => {
            d.def_get(it, o, "cellIndex", cell_index, None)?;
            reflect!(d, it, o, &["td", "th"], {
                "colSpan" => "colspan": Clamp(1, 1, 1000),
                "rowSpan" => "rowspan": Clamp(1, 0, 65534),
                "headers" => "headers": Str,
                "abbr" => "abbr": Str,
                "scope" => "scope": Enum(&["row", "col", "rowgroup", "colgroup"], ""),
            });
        }
    }
    Ok(())
}

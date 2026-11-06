//! `Range`, `document.createRange()`, `Selection` and `window.getSelection()` / `document.getSelection()`. Source: DOM Standard
//! ("Interface Range", "boundary points") and the Selection API, read by hand, then compared with headless Chrome
//! (`tools/fixtures/ui/range-cases.txt`, `tools/ui/chrome_topic.mjs`).
//!
//! What is built
//! - `Range` (also `new Range()`): `startContainer`, `startOffset`, `endContainer`, `endOffset`, `collapsed`, `commonAncestorContainer`,
//!   `setStart`, `setEnd`, `setStartBefore`, `setStartAfter`, `setEndBefore`, `setEndAfter`, `selectNode`, `selectNodeContents`, `collapse`,
//!   `cloneRange`, `detach` (no effect, as the standard says) and `toString` (the text of the Text nodes in the range, not rendered text).
//!   Offsets are UTF-16 units in a Text or Comment, and child indexes in any other node. The boundary rules are the standard's: a start after
//!   the end (or in another tree) moves the end, and the other way round.
//! - `Selection` (one per realm): `rangeCount`, `addRange`, `removeAllRanges`, `empty`, `removeRange`, `getRangeAt`, `collapse`,
//!   `setPosition`, `collapseToStart`, `collapseToEnd`, `selectAllChildren`, `isCollapsed`, `anchorNode`, `anchorOffset`, `focusNode`,
//!   `focusOffset`, `type`, `direction` and `toString`. The selection holds the range object that was added, as Chrome does, so a change of
//!   that object is a change of the selection. `collapse`, `setPosition` and `selectAllChildren` make a new range object.
//!   A range whose root is not the document is not added, and `collapse` of such a node does nothing (the standard).
//!
//! Gates (`Error::Unsupported`, none is faked)
//! - `deleteContents`, `extractContents`, `cloneContents`, `insertNode`, `surroundContents`, `createContextualFragment`,
//!   `getBoundingClientRect`, `getClientRects`, `compareBoundaryPoints`, `comparePoint`, `isPointInRange`, `intersectsNode` and the selection
//!   methods `extend`, `setBaseAndExtent`, `containsNode`, `modify`, `deleteFromDocument`.
//!
//! Known gaps (on purpose, listed)
//! - A range does not update when the DOM changes: after `removeChild` or `textContent` a boundary point keeps its node and offset, as if
//!   nothing happened (the standard moves it). A read through a stale offset is clamped, so it never panics.
//! - `Selection.toString()` is the text of the Text nodes, as `Range.toString()`. Chrome gives rendered text for a selection (line breaks
//!   between blocks, no hidden text).
//! - No caret, no focus, no `selectionchange` event, no selection by mouse, and the selection of `<input>` (`browser_selection.rs`) is not
//!   linked with this one. A lone surrogate made by cutting a surrogate pair in `toString` is U+FFFD.
//!
//! Bound (`Limit(Host(name))`): `ranges` (live ranges, set free by the collector).

use crate::browser::*;
use crate::browser_lazy::lazy_global;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{to_uint32, ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;

type V = R<Value>;

const MAX_RANGES: usize = 1 << 16;

#[derive(Clone, Copy)]
struct Rng {
    sc: NodeId,
    so: u32,
    ec: NodeId,
    eo: u32,
}

#[derive(Default)]
struct State {
    /// Range, Selection and AbstractRange: prototype and constructor of each, once made.
    made: Option<[ObjId; 6]>,
    ranges: HashMap<ObjId, Rng>,
    /// The selection object and its range.
    sel: Option<(ObjId, Option<ObjId>)>,
}

#[derive(Default)]
pub(crate) struct Ranges {
    st: OnceCell<RefCell<State>>,
}

#[cfg(test)]
impl Ranges {
    pub(crate) fn is_made(&self) -> bool {
        self.st.get().is_some()
    }
}

fn state(d: &Dom) -> &RefCell<State> {
    d.range.st.get_or_init(Default::default)
}

// ------------------------------------------------------------ the tree

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count().min(u32::MAX as usize) as u32
}

/// `s[from..to]` in UTF-16 units, clamped.
fn slice16(s: &str, from: u32, to: u32) -> String {
    let u: Vec<u16> = s.encode_utf16().collect();
    let to = (to as usize).min(u.len());
    let from = (from as usize).min(to);
    String::from_utf16_lossy(&u[from..to])
}

/// The length of a node: its data in a Text or a Comment, else its children.
fn length(doc: &Document, n: NodeId) -> u32 {
    match *doc.data(n) {
        NodeData::Text(s) | NodeData::Comment(s) | NodeData::Doctype(s) => utf16_len(doc.text(s)),
        _ => doc.children(n).count() as u32,
    }
}

fn index(doc: &Document, n: NodeId) -> u32 {
    let (mut i, mut c) = (0, n);
    while let Some(p) = doc.prev_sibling(c) {
        i += 1;
        c = p;
    }
    i
}

fn root_of(doc: &Document, mut n: NodeId) -> NodeId {
    while let Some(p) = doc.parent(n) {
        n = p;
    }
    n
}

/// The nodes from the root down to `n`.
fn path(doc: &Document, n: NodeId) -> Vec<NodeId> {
    let mut v = vec![n];
    let mut c = n;
    while let Some(p) = doc.parent(c) {
        v.push(p);
        c = p;
    }
    v.reverse();
    v
}

/// Tree order of two nodes of one tree: an ancestor is before its descendants.
fn order(doc: &Document, a: NodeId, b: NodeId) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (pa, pb) = (path(doc, a), path(doc, b));
    let k = pa.iter().zip(&pb).take_while(|(x, y)| x == y).count();
    if k == pa.len() {
        return Ordering::Less;
    }
    if k == pb.len() {
        return Ordering::Greater;
    }
    // Two children of one parent: walk from the first one.
    let mut c = doc.next_sibling(pa[k]);
    while let Some(x) = c {
        if x == pb[k] {
            return Ordering::Less;
        }
        c = doc.next_sibling(x);
    }
    Ordering::Greater
}

/// The position of boundary point `a` against `b` (the standard's "position of a boundary point"). Both are in one tree.
fn position(doc: &Document, a: (NodeId, u32), b: (NodeId, u32)) -> Ordering {
    if a.0 == b.0 {
        return a.1.cmp(&b.1);
    }
    if order(doc, a.0, b.0) == Ordering::Greater {
        return position(doc, b, a).reverse();
    }
    if path(doc, b.0).contains(&a.0) {
        let mut child = b.0;
        while doc.parent(child) != Some(a.0) {
            child = doc.parent(child).expect("a is an ancestor");
        }
        if index(doc, child) < a.1 {
            return Ordering::Greater;
        }
    }
    Ordering::Less
}

impl Rng {
    fn start(&self) -> (NodeId, u32) {
        (self.sc, self.so)
    }
    fn end(&self) -> (NodeId, u32) {
        (self.ec, self.eo)
    }
}

// ------------------------------------------------------------ errors and arguments

fn few(it: &mut Interp<'_>, what: &str, need: usize, got: usize) -> Error {
    const SEL: [&str; 7] = ["addRange", "removeRange", "getRangeAt", "collapse", "setPosition", "selectAllChildren", "extend"];
    let iface = if SEL.contains(&what) { "Selection" } else { "Range" };
    it.type_error(&format!("Failed to execute '{what}' on '{iface}': {need} argument{} required, but only {got} present.", if need == 1 { "" } else { "s" }))
}

fn range_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(ObjId, Rng)> {
    if let Value::Object(o) = this
        && let Some(&r) = state(d).borrow().ranges.get(&o)
    {
        return Ok((o, r));
    }
    Err(it.type_error("Illegal invocation"))
}

fn offset_arg(it: &mut Interp<'_>, v: Value) -> R<u32> {
    Ok(to_uint32(it.to_js_number(v)?))
}

fn set(d: &Dom, o: ObjId, r: Rng) {
    state(d).borrow_mut().ranges.insert(o, r);
}

fn is_doctype(d: &Dom, n: NodeId) -> bool {
    matches!(d.data(n), NodeData::Doctype(_))
}

/// The name of a doctype, for the messages (Chrome writes it where it writes a node type).
fn doctype_name(d: &Dom, n: NodeId) -> String {
    match d.data(n) {
        NodeData::Doctype(s) => d.doc.borrow().text(s).to_string(),
        _ => String::new(),
    }
}

/// Check a node and an offset as `setStart` does. `who` is the interface and method for the message.
fn check(d: &Dom, it: &mut Interp<'_>, who: &str, n: NodeId, off: u32) -> R<()> {
    if is_doctype(d, n) {
        return Err(d.dom_err(it, "InvalidNodeTypeError", &format!("Failed to execute '{who}': The node provided is of type '{}'.", doctype_name(d, n))));
    }
    let len = length(&d.doc.borrow(), n);
    if off > len {
        let m = if matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) {
            format!("Failed to execute '{who}': The offset {off} is larger than the node's length ({len}).")
        } else {
            format!("Failed to execute '{who}': There is no child at offset {off}.")
        };
        return Err(d.dom_err(it, "IndexSizeError", &m));
    }
    Ok(())
}

// ------------------------------------------------------------ Range

fn new_range(d: &Dom, it: &mut Interp<'_>, r: Rng) -> V {
    ensure(d, it)?;
    if state(d).borrow().ranges.len() >= MAX_RANGES {
        return Err(d.lim("ranges"));
    }
    let proto = state(d).borrow().made.expect("made")[0];
    let o = d.obj(it, proto)?;
    set(d, o, r);
    Ok(Value::Object(o))
}

fn construct(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let root = d.doc.borrow().root();
    new_range(d, it, Rng { sc: root, so: 0, ec: root, eo: 0 })
}

fn create_range(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    new_range(d, it, Rng { sc: n, so: 0, ec: n, eo: 0 })
}

/// Set one boundary point as the standard says: the other point follows when the order or the tree would be wrong.
fn bound(d: &Dom, o: ObjId, mut r: Rng, start: bool, n: NodeId, off: u32) {
    let doc = d.doc.borrow();
    if start {
        let other = if root_of(&doc, r.ec) != root_of(&doc, n) { None } else { Some(r.end()) };
        r.sc = n;
        r.so = off;
        if other.is_none_or(|e| position(&doc, (n, off), e) == Ordering::Greater) {
            r.ec = n;
            r.eo = off;
        }
    } else {
        let other = if root_of(&doc, r.sc) != root_of(&doc, n) { None } else { Some(r.start()) };
        r.ec = n;
        r.eo = off;
        if other.is_none_or(|s| position(&doc, (n, off), s) == Ordering::Less) {
            r.sc = n;
            r.so = off;
        }
    }
    drop(doc);
    set(d, o, r);
}

fn node_arg(d: &Dom, it: &mut Interp<'_>, who: &str, a: &[Value], i: usize) -> R<NodeId> {
    match d.node_of(a.get(i).copied().unwrap_or(Value::Undefined)) {
        Some(n) => Ok(n),
        None => Err(it.type_error(&format!("Failed to execute '{who}' on 'Range': parameter {} is not of type 'Node'.", i + 1))),
    }
}

fn set_start_end(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], start: bool) -> V {
    let who = if start { "setStart" } else { "setEnd" };
    if a.len() < 2 {
        return Err(few(it, who, 2, a.len()));
    }
    let (o, r) = range_of(d, it, this)?;
    let n = node_arg(d, it, who, a, 0)?;
    let off = offset_arg(it, a[1])?;
    check(d, it, &format!("{who}' on 'Range"), n, off)?;
    bound(d, o, r, start, n, off);
    Ok(Value::Undefined)
}

fn set_start(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_start_end(d, it, this, a, true)
}

fn set_end(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_start_end(d, it, this, a, false)
}

/// `setStartBefore` and the three like it: the parent and the index of the node, plus 1 for "after".
fn set_rel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], who: &str, start: bool, after: bool) -> V {
    if a.is_empty() {
        return Err(few(it, who, 1, 0));
    }
    let (o, r) = range_of(d, it, this)?;
    let n = node_arg(d, it, who, a, 0)?;
    let (p, i) = {
        let doc = d.doc.borrow();
        (doc.parent(n), index(&doc, n))
    };
    let Some(p) = p else {
        return Err(d.dom_err(it, "InvalidNodeTypeError", &format!("Failed to execute '{who}' on 'Range': the given Node has no parent.")));
    };
    bound(d, o, r, start, p, i + after as u32);
    Ok(Value::Undefined)
}

fn set_start_before(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_rel(d, it, this, a, "setStartBefore", true, false)
}
fn set_start_after(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_rel(d, it, this, a, "setStartAfter", true, true)
}
fn set_end_before(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_rel(d, it, this, a, "setEndBefore", false, false)
}
fn set_end_after(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_rel(d, it, this, a, "setEndAfter", false, true)
}

fn select_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(few(it, "selectNode", 1, 0));
    }
    let (o, _) = range_of(d, it, this)?;
    let n = node_arg(d, it, "selectNode", a, 0)?;
    let (p, i) = {
        let doc = d.doc.borrow();
        (doc.parent(n), index(&doc, n))
    };
    let Some(p) = p else {
        return Err(d.dom_err(it, "InvalidNodeTypeError", "Failed to execute 'selectNode' on 'Range': the given Node has no parent."));
    };
    set(d, o, Rng { sc: p, so: i, ec: p, eo: i + 1 });
    Ok(Value::Undefined)
}

fn select_node_contents(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(few(it, "selectNodeContents", 1, 0));
    }
    let (o, _) = range_of(d, it, this)?;
    let n = node_arg(d, it, "selectNodeContents", a, 0)?;
    if is_doctype(d, n) {
        return Err(d.dom_err(it, "InvalidNodeTypeError", &format!("Failed to execute 'selectNodeContents' on 'Range': The node provided is of type '{}'.", doctype_name(d, n))));
    }
    let len = length(&d.doc.borrow(), n);
    set(d, o, Rng { sc: n, so: 0, ec: n, eo: len });
    Ok(Value::Undefined)
}

fn collapse(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (o, mut r) = range_of(d, it, this)?;
    let to_start = match a.first() {
        Some(&v) => it.heap.to_boolean(v).map_err(|e| it.heap_error(e))?,
        None => false,
    };
    if to_start {
        (r.ec, r.eo) = r.start();
    } else {
        (r.sc, r.so) = r.end();
    }
    set(d, o, r);
    Ok(Value::Undefined)
}

fn clone_range(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    new_range(d, it, r)
}

fn detach(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    range_of(d, it, this)?;
    Ok(Value::Undefined)
}

fn start_container(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    d.wrap(it, r.sc)
}
fn end_container(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    d.wrap(it, r.ec)
}
fn start_offset(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(Value::Number(range_of(d, it, this)?.1.so as f64))
}
fn end_offset(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(Value::Number(range_of(d, it, this)?.1.eo as f64))
}
fn collapsed(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let r = range_of(d, it, this)?.1;
    Ok(Value::Bool(r.start() == r.end()))
}

fn common_ancestor(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    let doc = d.doc.borrow();
    let (a, b) = (path(&doc, r.sc), path(&doc, r.ec));
    let k = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    // The two points are in one tree, so the roots are equal and `k` is at least 1.
    let n = a[k.max(1) - 1];
    drop(doc);
    d.wrap(it, n)
}

/// The text of a range: the Text nodes it holds, cut at the two ends.
fn range_text(d: &Dom, it: &mut Interp<'_>, r: Rng) -> R<String> {
    let doc = d.doc.borrow();
    let mut out = String::new();
    if let NodeData::Text(s) = *doc.data(r.sc)
        && r.sc == r.ec
    {
        return Ok(slice16(doc.text(s), r.so, r.eo));
    }
    // The nodes under the common ancestor, in tree order.
    let (a, b) = (path(&doc, r.sc), path(&doc, r.ec));
    let k = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if k == 0 {
        return Ok(out);
    }
    let top = a[k - 1];
    let mut stack = vec![top];
    while let Some(n) = stack.pop() {
        it.charge(1)?;
        if let NodeData::Text(s) = *doc.data(n) {
            let t = doc.text(s);
            if n == r.sc {
                out += &slice16(t, r.so, u32::MAX);
            } else if n == r.ec {
                out += &slice16(t, 0, r.eo);
            } else {
                let len = utf16_len(t);
                if position(&doc, (n, 0), r.start()) == Ordering::Greater && position(&doc, (n, len), r.end()) == Ordering::Less {
                    out += t;
                }
            }
        }
        let first = stack.len();
        stack.extend(doc.children(n));
        stack[first..].reverse();
    }
    Ok(out)
}

fn range_to_string(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    let s = range_text(d, it, r)?;
    d.js_str(it, &s)
}

fn gate(what: &'static str) -> Error {
    Error::Unsupported(unsupported(what))
}

macro_rules! gated {
    ($($f:ident => $what:literal),* $(,)?) => {$(
        fn $f(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            range_of(d, it, this)?;
            Err(gate($what))
        }
    )*};
}
gated! {
    delete_contents => "Range.deleteContents",
    extract_contents => "Range.extractContents",
    clone_contents => "Range.cloneContents",
    insert_node => "Range.insertNode",
    surround_contents => "Range.surroundContents",
    contextual_fragment => "Range.createContextualFragment",
    bounding_rect => "Range.getBoundingClientRect",
    client_rects => "Range.getClientRects",
    compare_boundary => "Range.compareBoundaryPoints",
    compare_point => "Range.comparePoint",
    in_range => "Range.isPointInRange",
    intersects => "Range.intersectsNode",
}

fn illegal_selection(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'Selection': Illegal constructor"))
}

fn illegal_abstract(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Illegal constructor"))
}

// ------------------------------------------------------------ Selection

fn sel_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Option<ObjId>> {
    match (this, state(d).borrow().sel) {
        (Value::Object(o), Some((s, r))) if o == s => Ok(r),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn set_sel(d: &Dom, r: Option<ObjId>) {
    if let Some(s) = state(d).borrow_mut().sel.as_mut() {
        s.1 = r;
    }
}

fn sel_range(d: &Dom, r: Option<ObjId>) -> Option<Rng> {
    r.and_then(|o| state(d).borrow().ranges.get(&o).copied())
}

fn get_selection(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    ensure(d, it)?;
    if let Some((s, _)) = state(d).borrow().sel {
        return Ok(Value::Object(s));
    }
    let proto = state(d).borrow().made.expect("made")[2];
    let o = d.obj(it, proto)?;
    state(d).borrow_mut().sel = Some((o, None));
    Ok(Value::Object(o))
}

fn range_count(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(Value::Number(sel_of(d, it, this)?.is_some() as u8 as f64))
}

fn add_range(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    let Some(&v) = a.first() else { return Err(few(it, "addRange", 1, 0)) };
    let Value::Object(o) = v else { return Err(it.type_error("Failed to execute 'addRange' on 'Selection': parameter 1 is not of type 'Range'.")) };
    let Some(r) = state(d).borrow().ranges.get(&o).copied() else {
        return Err(it.type_error("Failed to execute 'addRange' on 'Selection': parameter 1 is not of type 'Range'."));
    };
    let doc = d.doc.borrow();
    if cur.is_none() && root_of(&doc, r.sc) == doc.root() {
        drop(doc);
        set_sel(d, Some(o));
    }
    Ok(Value::Undefined)
}

fn remove_all(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    sel_of(d, it, this)?;
    set_sel(d, None);
    Ok(Value::Undefined)
}

fn remove_range(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    let Some(&v) = a.first() else { return Err(few(it, "removeRange", 1, 0)) };
    let o = match v {
        Value::Object(o) if state(d).borrow().ranges.contains_key(&o) => o,
        _ => return Err(it.type_error("Failed to execute 'removeRange' on 'Selection': parameter 1 is not of type 'Range'.")),
    };
    if cur != Some(o) {
        return Err(d.dom_err(it, "NotFoundError", "Failed to execute 'removeRange' on 'Selection': Range not found."));
    }
    set_sel(d, None);
    Ok(Value::Undefined)
}

fn get_range_at(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    let Some(&v) = a.first() else { return Err(few(it, "getRangeAt", 1, 0)) };
    let i = offset_arg(it, v)?;
    match cur {
        Some(r) if i == 0 => Ok(Value::Object(r)),
        _ => Err(d.dom_err(it, "IndexSizeError", &format!("Failed to execute 'getRangeAt' on 'Selection': {i} is not a valid index."))),
    }
}

/// `collapse(node, offset)` and `setPosition`.
fn sel_collapse(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], who: &str) -> V {
    sel_of(d, it, this)?;
    let node = match a.first().copied().unwrap_or(Value::Undefined) {
        Value::Undefined if a.is_empty() => return Err(few(it, who, 1, 0)),
        Value::Null | Value::Undefined => return remove_all(d, it, this, &[]),
        v => match d.node_of(v) {
            Some(n) => n,
            None => return Err(it.type_error(&format!("Failed to execute '{who}' on 'Selection': parameter 1 is not of type 'Node'."))),
        },
    };
    let off = match a.get(1) {
        Some(&v) => offset_arg(it, v)?,
        None => 0,
    };
    check(d, it, &format!("{who}' on 'Selection"), node, off)?;
    let in_doc = {
        let doc = d.doc.borrow();
        root_of(&doc, node) == doc.root()
    };
    if in_doc {
        let r = new_range(d, it, Rng { sc: node, so: off, ec: node, eo: off })?;
        let Value::Object(o) = r else { unreachable!() };
        set_sel(d, Some(o));
    }
    Ok(Value::Undefined)
}

fn collapse_sel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    sel_collapse(d, it, this, a, "collapse")
}

fn set_position(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    sel_collapse(d, it, this, a, "setPosition")
}

fn collapse_to(d: &Dom, it: &mut Interp<'_>, this: Value, who: &str, start: bool) -> V {
    let cur = sel_of(d, it, this)?;
    let Some(r) = sel_range(d, cur) else {
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to execute '{who}' on 'Selection': there is no selection.")));
    };
    let (n, o) = if start { r.start() } else { r.end() };
    let v = new_range(d, it, Rng { sc: n, so: o, ec: n, eo: o })?;
    let Value::Object(o) = v else { unreachable!() };
    set_sel(d, Some(o));
    Ok(Value::Undefined)
}

fn collapse_to_start(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    collapse_to(d, it, this, "collapseToStart", true)
}

fn collapse_to_end(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    collapse_to(d, it, this, "collapseToEnd", false)
}

fn select_all_children(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    sel_of(d, it, this)?;
    if a.is_empty() {
        return Err(few(it, "selectAllChildren", 1, 0));
    }
    let Some(n) = d.node_of(a[0]) else {
        return Err(it.type_error("Failed to execute 'selectAllChildren' on 'Selection': parameter 1 is not of type 'Node'."));
    };
    if is_doctype(d, n) {
        return Err(d.dom_err(it, "InvalidNodeTypeError", &format!("Failed to execute 'selectAllChildren' on 'Selection': The node provided is of type '{}'.", doctype_name(d, n))));
    }
    let (in_doc, len) = {
        let doc = d.doc.borrow();
        (root_of(&doc, n) == doc.root(), doc.children(n).count() as u32)
    };
    if in_doc {
        let v = new_range(d, it, Rng { sc: n, so: 0, ec: n, eo: len })?;
        let Value::Object(o) = v else { unreachable!() };
        set_sel(d, Some(o));
    }
    Ok(Value::Undefined)
}

fn is_collapsed(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    Ok(Value::Bool(sel_range(d, cur).is_none_or(|r| r.start() == r.end())))
}

fn sel_point(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl Fn(Rng) -> (NodeId, u32), node: bool) -> V {
    let cur = sel_of(d, it, this)?;
    match sel_range(d, cur).map(f) {
        None if node => Ok(Value::Null),
        None => Ok(Value::Number(0.0)),
        Some((n, _)) if node => d.wrap(it, n),
        Some((_, o)) => Ok(Value::Number(o as f64)),
    }
}

fn anchor_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    sel_point(d, it, this, |r| r.start(), true)
}
fn anchor_offset(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    sel_point(d, it, this, |r| r.start(), false)
}
fn focus_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    sel_point(d, it, this, |r| r.end(), true)
}
fn focus_offset(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    sel_point(d, it, this, |r| r.end(), false)
}

fn sel_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    d.js_str(it, match sel_range(d, cur) {
        None => "None",
        Some(r) if r.start() == r.end() => "Caret",
        Some(_) => "Range",
    })
}

fn sel_direction(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    d.js_str(it, match sel_range(d, cur) {
        Some(r) if r.start() != r.end() => "forward",
        _ => "none",
    })
}

fn sel_to_string(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let cur = sel_of(d, it, this)?;
    let s = match sel_range(d, cur) {
        Some(r) => range_text(d, it, r)?,
        None => String::new(),
    };
    d.js_str(it, &s)
}

macro_rules! sel_gated {
    ($($f:ident => $what:literal),* $(,)?) => {$(
        fn $f(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            sel_of(d, it, this)?;
            Err(gate($what))
        }
    )*};
}
sel_gated! {
    extend => "Selection.extend",
    base_extent => "Selection.setBaseAndExtent",
    contains_node => "Selection.containsNode",
    modify => "Selection.modify",
    delete_from_document => "Selection.deleteFromDocument",
}

// ------------------------------------------------------------ install

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if state(d).borrow().made.is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let (ap, sp) = (d.obj(it, op)?, d.obj(it, op)?);
    let rp = d.obj(it, ap)?;
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let rc = d.add_fn(it, construct, "Range", 0, true)?;
    let sc = d.add_fn(it, illegal_selection, "Selection", 0, true)?;
    let ac = d.add_fn(it, illegal_abstract, "AbstractRange", 0, true)?;
    it.heap.set_prototype_of(rc, Some(ac)).map_err(|e| it.heap_error(e))?;
    for (f, proto, tag) in [(rc, rp, "Range"), (sc, sp, "Selection"), (ac, ap, "AbstractRange")] {
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        crate::browser_lazy::tag(d, it, proto, tag)?;
    }
    for (n, v) in [("START_TO_START", 0.0), ("START_TO_END", 1.0), ("END_TO_END", 2.0), ("END_TO_START", 3.0)] {
        let k = d.key(it, n)?;
        for t in [rc, rp] {
            it.define_data(t, k, Value::Number(v), false, true, false)?;
        }
    }
    let gets: [(&str, H); 5] = [("startContainer", start_container), ("startOffset", start_offset), ("endContainer", end_container), ("endOffset", end_offset), ("collapsed", collapsed)];
    for (n, h) in gets {
        d.def_get(it, ap, n, h, None)?;
    }
    d.def_get(it, rp, "commonAncestorContainer", common_ancestor, None)?;
    let methods: [(&str, u32, H); 24] = [
        ("setStart", 2, set_start),
        ("setEnd", 2, set_end),
        ("setStartBefore", 1, set_start_before),
        ("setStartAfter", 1, set_start_after),
        ("setEndBefore", 1, set_end_before),
        ("setEndAfter", 1, set_end_after),
        ("selectNode", 1, select_node),
        ("selectNodeContents", 1, select_node_contents),
        ("collapse", 0, collapse),
        ("cloneRange", 0, clone_range),
        ("detach", 0, detach),
        ("toString", 0, range_to_string),
        ("deleteContents", 0, delete_contents),
        ("extractContents", 0, extract_contents),
        ("cloneContents", 0, clone_contents),
        ("insertNode", 1, insert_node),
        ("surroundContents", 1, surround_contents),
        ("createContextualFragment", 1, contextual_fragment),
        ("getBoundingClientRect", 0, bounding_rect),
        ("getClientRects", 0, client_rects),
        ("compareBoundaryPoints", 2, compare_boundary),
        ("comparePoint", 2, compare_point),
        ("isPointInRange", 2, in_range),
        ("intersectsNode", 1, intersects),
    ];
    for (n, l, h) in methods {
        d.def_method(it, rp, n, l, h)?;
    }
    let gets: [(&str, H); 8] = [("rangeCount", range_count), ("isCollapsed", is_collapsed), ("anchorNode", anchor_node), ("anchorOffset", anchor_offset), ("focusNode", focus_node), ("focusOffset", focus_offset), ("type", sel_type), ("direction", sel_direction)];
    for (n, h) in gets {
        d.def_get(it, sp, n, h, None)?;
    }
    let methods: [(&str, u32, H); 16] = [
        ("addRange", 1, add_range),
        ("removeAllRanges", 0, remove_all),
        ("empty", 0, remove_all),
        ("removeRange", 1, remove_range),
        ("getRangeAt", 1, get_range_at),
        ("collapse", 0, collapse_sel),
        ("setPosition", 0, set_position),
        ("collapseToStart", 0, collapse_to_start),
        ("collapseToEnd", 0, collapse_to_end),
        ("selectAllChildren", 1, select_all_children),
        ("toString", 0, sel_to_string),
        ("extend", 1, extend),
        ("setBaseAndExtent", 4, base_extent),
        ("containsNode", 1, contains_node),
        ("modify", 3, modify),
        ("deleteFromDocument", 0, delete_from_document),
    ];
    for (n, l, h) in methods {
        d.def_method(it, sp, n, l, h)?;
    }
    state(d).borrow_mut().made = Some([rp, rc, sp, sc, ap, ac]);
    Ok(())
}

lazy_global!(lazy_range, "Range", ensure, |d| state(d).borrow().made.expect("made")[1]);
lazy_global!(lazy_abstract, "AbstractRange", ensure, |d| state(d).borrow().made.expect("made")[5]);
lazy_global!(lazy_selection, "Selection", ensure, |d| state(d).borrow().made.expect("made")[3]);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    d.def_method(it, g, "getSelection", 0, get_selection)?;
    d.def_method(it, d.p().document, "getSelection", 0, get_selection)?;
    d.def_method(it, d.p().document, "createRange", 0, create_range)?;
    crate::browser_lazy::install(d, it, &[("Range", lazy_range as H), ("Selection", lazy_selection), ("AbstractRange", lazy_abstract)])
}

impl Dom {
    pub(crate) fn range_roots(&self, out: &mut Vec<Value>) {
        if let Some(st) = self.range.st.get() {
            let s = st.borrow();
            out.extend(s.made.iter().flatten().copied().map(Value::Object));
            out.extend(s.sel.map(|(o, _)| Value::Object(o)));
        }
    }

    pub(crate) fn range_trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if let Some(st) = self.range.st.get()
            && let Some((o, Some(r))) = st.borrow().sel
            && o == owner
        {
            out.push(Value::Object(r));
        }
    }

    pub(crate) fn range_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        if let Some(st) = self.range.st.get() {
            st.borrow_mut().ranges.retain(|&o, _| live(o));
        }
    }
}

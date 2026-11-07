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
//! - The selection methods `extend`, `setBaseAndExtent`, `containsNode`, `modify` and `deleteFromDocument`.
//!
//! Live ranges (DOM Standard, "live range")
//! - A range follows the tree. The document watcher (see `Dom::sync_watcher`) tells the ranges of each node linked or unlinked: the "insert" and
//!   "remove" steps of the standard move the boundary points. The text steps ("replace data", `splitText`, `normalize`) are called by the host
//!   methods that make them (`Dom::range_data`, `range_split`, `range_merge`). The watcher is set when the first range is made and goes when
//!   the collector has freed the last one, so a page that makes no range pays nothing at a change. With a range, a change costs one pass over
//!   the live ranges, and a node removal also the depth of each boundary point.
//!
//! Known gaps (on purpose, listed)
//! - The text that `outerHTML` joins to its neighbours (a Chrome step the standard does not have) moves no range. A read through a stale
//!   offset is clamped, so it never panics.
//! - `Selection.toString()` is the text of the Text nodes, as `Range.toString()`. Chrome gives rendered text for a selection (line breaks
//!   between blocks, no hidden text).
//! - No caret, no focus, no `selectionchange` event, no selection by mouse, and the selection of `<input>` (`browser_selection.rs`) is not
//!   linked with this one. A lone surrogate made by cutting a surrogate pair in `toString` is U+FFFD.
//!
//! Bound (`Limit(Host(name))`): `ranges` (live ranges, set free by the collector).

use crate::browser::*;
use crate::browser_scheduler::Task;
use crate::browser_chardata::{replace, split_at};
use crate::browser_dom::{clone_tree, name_of, Op};
use crate::browser_lazy::lazy_global;
use bare_html::dom::{Change, Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{to_uint32, ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

type V = R<Value>;

const MAX_RANGES: usize = 1 << 16;

#[derive(Clone, Copy, PartialEq)]
struct Rng {
    sc: NodeId,
    so: u32,
    ec: NodeId,
    eo: u32,
}

#[derive(Default)]
pub(crate) struct State {
    /// Range, Selection and AbstractRange: prototype and constructor of each, once made.
    made: Option<[ObjId; 6]>,
    /// The method `item` that each list of rectangles holds (`browser_rects.rs`), once made.
    rect_item: Option<ObjId>,
    ranges: HashMap<ObjId, Rng>,
    /// The selection object and its range.
    sel: Option<(ObjId, Option<ObjId>)>,
    /// A `Task::DocSelectionChange` is queued (Selection API "has scheduled selectionchange event").
    sc_pending: bool,
}

#[derive(Default)]
pub(crate) struct Ranges {
    st: OnceCell<Rc<RefCell<State>>>,
}

impl Ranges {
    /// The state, when a range is alive (for the watcher).
    pub(crate) fn live(&self) -> Option<Rc<RefCell<State>>> {
        self.st.get().filter(|s| !s.borrow().ranges.is_empty()).cloned()
    }

    fn any(&self) -> bool {
        self.st.get().is_some_and(|s| !s.borrow().ranges.is_empty())
    }
}

#[cfg(test)]
impl Ranges {
    pub(crate) fn is_made(&self) -> bool {
        self.st.get().is_some()
    }
}

fn state(d: &Dom) -> &Rc<RefCell<State>> {
    d.range.st.get_or_init(Default::default)
}

/// Is `n` the node `anc` or inside it?
fn inside(doc: &Document, mut n: NodeId, anc: NodeId) -> bool {
    loop {
        if n == anc {
            return true;
        }
        match doc.parent(n) {
            Some(p) => n = p,
            None => return false,
        }
    }
}

impl State {
    /// The range of the selection, as it is now.
    fn sel_rng(&self) -> Option<Rng> {
        self.sel.and_then(|(_, r)| r).and_then(|o| self.ranges.get(&o).copied())
    }

    /// The "insert" and "remove" steps of the standard for the live ranges, given the change of the tree.
    pub(crate) fn on_change(&mut self, doc: &Document, ch: Change) {
        if self.ranges.is_empty() {
            return;
        }
        match ch {
            // A node put before another one: the points after its place shift. An append moves nothing.
            Change::Link { parent, node, next: Some(_), .. } => {
                let mut at = None;
                for r in self.ranges.values_mut() {
                    r.so += (r.sc == parent && r.so > *at.get_or_insert_with(|| index(doc, node))) as u32;
                    r.eo += (r.ec == parent && r.eo > *at.get_or_insert_with(|| index(doc, node))) as u32;
                }
            }
            // A node to leave: the points in it go to its place in the parent, the points after it shift back.
            Change::Unlink { parent, node, .. } => {
                let mut at = None;
                for r in self.ranges.values_mut() {
                    if inside(doc, r.sc, node) {
                        (r.sc, r.so) = (parent, *at.get_or_insert_with(|| index(doc, node)));
                    } else if r.sc == parent && r.so > *at.get_or_insert_with(|| index(doc, node)) {
                        r.so -= 1;
                    }
                    if inside(doc, r.ec, node) {
                        (r.ec, r.eo) = (parent, *at.get_or_insert_with(|| index(doc, node)));
                    } else if r.ec == parent && r.eo > *at.get_or_insert_with(|| index(doc, node)) {
                        r.eo -= 1;
                    }
                }
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------ the tree

pub(crate) fn utf16_len(s: &str) -> u32 {
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
    let moved = {
        let mut st = state(d).borrow_mut();
        st.ranges.insert(o, r).is_some_and(|old| old != r) && st.sel.is_some_and(|s| s.1 == Some(o))
    };
    if moved {
        d.schedule_doc_selectionchange();
    }
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
    if state(d).borrow().ranges.len() == 1 {
        d.sync_watcher();
    }
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
    let (o, _) = range_of(d, it, this)?;
    let n = node_arg(d, it, who, a, 0)?;
    let off = offset_arg(it, a[1])?;
    check(d, it, &format!("{who}' on 'Range"), n, off)?;
    // The offset may have run script that changed the tree: read the points now.
    let r = range_of(d, it, this)?.1;
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

// ------------------------------------------------------------ compare, point and intersect

/// -1, 0 or 1 for an order.
fn sign(o: Ordering) -> Value {
    Value::Number(o as i8 as f64)
}

fn compare_boundary(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    if a.len() < 2 {
        return Err(few(it, "compareBoundaryPoints", 2, a.len()));
    }
    // `unsigned short`: the number modulo 65536.
    let how = to_uint32(it.to_js_number(a[0])?) as u16;
    let src = match a[1] {
        Value::Object(o) => state(d).borrow().ranges.get(&o).copied(),
        _ => None,
    };
    let Some(src) = src else {
        return Err(it.type_error("Failed to execute 'compareBoundaryPoints' on 'Range': parameter 2 is not of type 'Range'."));
    };
    if how > 3 {
        return Err(d.dom_err(it, "NotSupportedError", "Failed to execute 'compareBoundaryPoints' on 'Range': The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'."));
    }
    let doc = d.doc.borrow();
    if root_of(&doc, r.sc) != root_of(&doc, src.sc) {
        drop(doc);
        return Err(d.dom_err(it, "WrongDocumentError", "Failed to execute 'compareBoundaryPoints' on 'Range': The source range is in a different document than this range."));
    }
    let (mine, other) = match how {
        0 => (r.start(), src.start()),
        1 => (r.end(), src.start()),
        2 => (r.end(), src.end()),
        _ => (r.start(), src.end()),
    };
    Ok(sign(position(&doc, mine, other)))
}

/// The arguments of `comparePoint` and `isPointInRange`: the node and the offset. `Ok(None)`: the node is in another tree.
fn point_args(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], who: &str) -> R<(Rng, NodeId, u32)> {
    let (_, r) = range_of(d, it, this)?;
    if a.len() < 2 {
        return Err(few(it, who, 2, a.len()));
    }
    let n = node_arg(d, it, who, a, 0)?;
    Ok((r, n, offset_arg(it, a[1])?))
}

fn same_tree(d: &Dom, r: Rng, n: NodeId) -> bool {
    let doc = d.doc.borrow();
    root_of(&doc, r.sc) == root_of(&doc, n)
}

/// -1 when the point is before the range, 1 when it is after, else 0.
fn against(d: &Dom, r: Rng, p: (NodeId, u32)) -> i32 {
    let doc = d.doc.borrow();
    if position(&doc, p, r.start()) == Ordering::Less {
        -1
    } else if position(&doc, p, r.end()) == Ordering::Greater {
        1
    } else {
        0
    }
}

fn compare_point(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (r, n, off) = point_args(d, it, this, a, "comparePoint")?;
    if !same_tree(d, r, n) {
        return Err(d.dom_err(it, "WrongDocumentError", "Failed to execute 'comparePoint' on 'Range': The node provided and the Range are not in the same tree."));
    }
    check(d, it, "comparePoint' on 'Range", n, off)?;
    Ok(Value::Number(against(d, r, (n, off)) as f64))
}

fn in_range(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (r, n, off) = point_args(d, it, this, a, "isPointInRange")?;
    if !same_tree(d, r, n) {
        return Ok(Value::Bool(false));
    }
    check(d, it, "isPointInRange' on 'Range", n, off)?;
    Ok(Value::Bool(against(d, r, (n, off)) == 0))
}

fn intersects(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, r) = range_of(d, it, this)?;
    if a.is_empty() {
        return Err(few(it, "intersectsNode", 1, 0));
    }
    let n = node_arg(d, it, "intersectsNode", a, 0)?;
    let doc = d.doc.borrow();
    if root_of(&doc, r.sc) != root_of(&doc, n) {
        return Ok(Value::Bool(false));
    }
    let Some(p) = doc.parent(n) else { return Ok(Value::Bool(true)) };
    let at = index(&doc, n);
    Ok(Value::Bool(position(&doc, (p, at), r.end()) == Ordering::Less && position(&doc, (p, at + 1), r.start()) == Ordering::Greater))
}

// ------------------------------------------------------------ delete, extract, clone, insert, surround

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Delete,
    Extract,
    Clone,
}

/// How deep `cut` may go (it calls itself once for each level of a boundary point). A deeper tree is a limit, not a stack overflow.
const MAX_CUT_DEPTH: u32 = 4096;

fn text_of(doc: &Document, n: NodeId) -> Option<String> {
    match *doc.data(n) {
        NodeData::Text(s) | NodeData::Comment(s) => Some(doc.text(s).to_string()),
        _ => None,
    }
}

/// The "extract" steps of the standard for the Text or Comment `n` (its text is `text`), units `from` to `to`: a clone of that part goes to `frag`
/// (`Extract`, `Clone`), and the part goes from `n` (`Extract`, `Delete`).
#[allow(clippy::too_many_arguments)]
fn cut_data(d: &Dom, it: &mut Interp<'_>, m: Mode, frag: NodeId, n: NodeId, text: &str, from: u32, to: u32) -> R<()> {
    if m != Mode::Delete {
        let c = clone_tree(d, it, n, false, false)?;
        let mut doc = d.doc.borrow_mut();
        doc.set_text(c, &slice16(text, from, to));
        doc.move_to(c, frag, None);
    }
    if m != Mode::Clone {
        replace(d, it, "replaceData", (n, text), from, to.saturating_sub(from), "")?;
    }
    Ok(())
}

/// The steps "extract a live range" and "clone the contents" of the standard, for the points `r`, into `frag`. `Delete` makes no clones. Gives the
/// point that the range gets when it is done. The order of the changes is the standard's, and so the order of the records.
fn cut(d: &Dom, it: &mut Interp<'_>, m: Mode, who: &str, r: Rng, frag: NodeId, depth: u32) -> R<(NodeId, u32)> {
    it.charge(1)?;
    if depth > MAX_CUT_DEPTH {
        return Err(d.lim("dom-depth"));
    }
    let (sc, so, ec, eo) = (r.sc, r.so, r.ec, r.eo);
    if r.start() == r.end() {
        return Ok((sc, so));
    }
    if sc == ec {
        let t = text_of(&d.doc.borrow(), sc);
        if let Some(t) = t {
            cut_data(d, it, m, frag, sc, &t, so, eo)?;
            return Ok((sc, so));
        }
    }
    let (first, last, contained, new) = {
        let doc = d.doc.borrow();
        let (a, b) = (path(&doc, sc), path(&doc, ec));
        let k = a.iter().zip(&b).take_while(|(x, y)| x == y).count().max(1);
        let common = a[k - 1];
        let (first, last) = ((a.len() > k).then(|| a[k]), (b.len() > k).then(|| b[k]));
        let from = first.map_or(so as usize, |f| index(&doc, f) as usize + 1);
        let to = last.map_or(eo as usize, |l| index(&doc, l) as usize);
        let contained: Vec<NodeId> = doc.children(common).skip(from).take(to.saturating_sub(from)).collect();
        let new = if first.is_none() { (sc, so) } else { (common, index(&doc, a[k]) + 1) };
        (first, last, contained, new)
    };
    it.charge(contained.len() as u64)?;
    if m != Mode::Delete
        && let Some(&dt) = contained.iter().find(|&&c| is_doctype(d, c))
    {
        let msg = if m == Mode::Extract {
            format!("Failed to execute '{who}' on 'Range': The Range contains a doctype node.")
        } else {
            format!("Failed to execute '{who}' on 'Range': Nodes of type '{}' may not be inserted inside nodes of type '#document-fragment'.", doctype_name(d, dt))
        };
        return Err(d.dom_err(it, "HierarchyRequestError", &msg));
    }
    // A partly selected node: its shallow clone goes to `frag`, and what the range holds of it goes into the clone.
    let partial = |it: &mut Interp<'_>, n: NodeId, sub: Rng| -> R<()> {
        let into = if m == Mode::Delete {
            frag
        } else {
            let c = clone_tree(d, it, n, false, false)?;
            d.doc.borrow_mut().move_to(c, frag, None);
            c
        };
        cut(d, it, m, who, sub, into, depth + 1).map(drop)
    };
    if let Some(f) = first {
        let t = text_of(&d.doc.borrow(), f);
        match t {
            Some(t) => cut_data(d, it, m, frag, f, &t, so, utf16_len(&t))?,
            None => {
                let eo = length(&d.doc.borrow(), f);
                partial(it, f, Rng { sc, so, ec: f, eo })?
            }
        }
    }
    for c in contained {
        match m {
            Mode::Delete => d.doc.borrow_mut().detach(c),
            Mode::Extract => d.doc.borrow_mut().move_to(c, frag, None),
            Mode::Clone => {
                let cc = clone_tree(d, it, c, true, false)?;
                d.doc.borrow_mut().move_to(cc, frag, None);
            }
        }
    }
    if let Some(l) = last {
        let t = text_of(&d.doc.borrow(), l);
        match t {
            Some(t) => cut_data(d, it, m, frag, l, &t, 0, eo)?,
            None => partial(it, l, Rng { sc: l, so: 0, ec, eo })?,
        }
    }
    Ok(new)
}

/// `deleteContents`, `extractContents` and `cloneContents`. The fragment (not for `Delete`) or `undefined`.
fn cut_range(d: &Dom, it: &mut Interp<'_>, this: Value, m: Mode, who: &str) -> V {
    let (o, r) = range_of(d, it, this)?;
    let frag = if m == Mode::Delete { Value::Undefined } else { crate::browser_dom::new_fragment(d, it, None)? };
    let f = d.node_of(frag).unwrap_or(r.sc);
    let (n, off) = cut(d, it, m, who, r, f, 0)?;
    if m != Mode::Clone && r.start() != r.end() {
        set(d, o, Rng { sc: n, so: off, ec: n, eo: off });
    }
    Ok(frag)
}

fn delete_contents(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    cut_range(d, it, this, Mode::Delete, "deleteContents")
}

fn extract_contents(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    cut_range(d, it, this, Mode::Extract, "extractContents")
}

fn clone_contents(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    cut_range(d, it, this, Mode::Clone, "cloneContents")
}

/// The steps of `insertNode` for the range `o` (`op`: the method for the messages, `insertNode` or `surroundContents`).
fn insert_into(d: &Dom, it: &mut Interp<'_>, o: ObjId, node: NodeId, op: Op) -> R<()> {
    let cur = |d: &Dom| state(d).borrow().ranges[&o];
    let r = cur(d);
    let (sc, so) = (r.sc, r.so);
    let (kind, up) = {
        let doc = d.doc.borrow();
        (*doc.data(sc), doc.parent(sc))
    };
    let bad = |it: &mut Interp<'_>, why: &str| d.refuse(it, op, sc, "HierarchyRequestError", why);
    if matches!(kind, NodeData::Comment(_)) {
        let (a, b) = (name_of(&d.doc.borrow(), node), name_of(&d.doc.borrow(), sc));
        return Err(bad(it, &format!("Nodes of type '{a}' may not be inserted inside nodes of type '{b}'.")));
    }
    let text = matches!(kind, NodeData::Text(_));
    if text && up.is_none() {
        return Err(bad(it, "This operation would split a text node, but there's no parent into which to insert."));
    }
    if node == sc {
        return Err(bad(it, "Unable to insert a node into a Range starting from the node itself."));
    }
    let (parent, mut reference) = match up {
        Some(p) if text => (p, Some(sc)),
        _ => (sc, d.doc.borrow().children(sc).nth(so as usize)),
    };
    if is_doctype(d, node) && !matches!(d.data(parent), NodeData::Document) {
        let (a, b) = (name_of(&d.doc.borrow(), node), name_of(&d.doc.borrow(), parent));
        return Err(d.refuse(it, op, parent, "HierarchyRequestError", &format!("Nodes of type '{a}' may not be inserted inside nodes of type '{b}'.")));
    }
    let frag = d.check_insert(it, parent, node, reference, None, op)?;
    if text {
        let t = text_of(&d.doc.borrow(), sc).unwrap_or_default();
        reference = Some(split_at(d, sc, &t, so.min(utf16_len(&t)))?);
    }
    if reference == Some(node) {
        reference = d.doc.borrow().next_sibling(node);
    }
    d.insert_valid(it, parent, node, reference, frag, op)?;
    let r = cur(d);
    if r.start() == r.end() {
        let doc = d.doc.borrow();
        let off = reference.map_or_else(|| length(&doc, parent), |x| index(&doc, x));
        drop(doc);
        set(d, o, Rng { ec: parent, eo: off, ..r });
    }
    Ok(())
}

fn insert_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (o, _) = range_of(d, it, this)?;
    if a.is_empty() {
        return Err(few(it, "insertNode", 1, 0));
    }
    let node = node_arg(d, it, "insertNode", a, 0)?;
    insert_into(d, it, o, node, Op("insertNode", "Range"))?;
    Ok(Value::Undefined)
}

fn surround_contents(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (o, r) = range_of(d, it, this)?;
    if a.is_empty() {
        return Err(few(it, "surroundContents", 1, 0));
    }
    let np = node_arg(d, it, "surroundContents", a, 0)?;
    let op = Op("surroundContents", "Range");
    let partial = {
        let doc = d.doc.borrow();
        let (x, y) = (path(&doc, r.sc), path(&doc, r.ec));
        let k = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
        // The nodes below the common ancestor on the two paths are the partly selected ones.
        x[k..].iter().chain(&y[k..]).any(|&n| !matches!(doc.data(n), NodeData::Text(_)))
    };
    if partial {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to execute 'surroundContents' on 'Range': The Range has partially selected a non-Text node."));
    }
    if matches!(d.data(np), NodeData::Document | NodeData::Doctype(_) | NodeData::ShadowRoot) {
        let name = name_of(&d.doc.borrow(), np);
        return Err(d.dom_err(it, "InvalidNodeTypeError", &format!("Failed to execute 'surroundContents' on 'Range': The node provided is of type '{name}'.")));
    }
    let frag = cut_range(d, it, this, Mode::Extract, "surroundContents")?;
    let frag = d.node_of(frag).expect("a fragment");
    if d.doc.borrow().children(np).next().is_some() {
        let res = d.mo_group(Some(np), || {
            let mut doc = d.doc.borrow_mut();
            d.iter_told_all(&doc, np);
            while let Some(c) = doc.children(np).next() {
                it.charge(1)?;
                doc.detach(c);
            }
            Ok(())
        });
        d.iter_told_end();
        res?;
    }
    insert_into(d, it, o, np, op)?;
    d.insert_checked(it, np, frag, None, op)?;
    let (p, i) = {
        let doc = d.doc.borrow();
        (doc.parent(np).expect("inserted"), index(&doc, np))
    };
    set(d, o, Rng { sc: p, so: i, ec: p, eo: i + 1 });
    Ok(Value::Undefined)
}

fn contextual_fragment(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    range_of(d, it, this)?;
    if a.is_empty() {
        return Err(few(it, "createContextualFragment", 1, 0));
    }
    // ToString may run page code that moves the range: the start is read after it.
    let html = d.string_arg(it, a[0])?;
    let (_, r) = range_of(d, it, this)?;
    crate::browser_fragment::contextual_fragment(d, it, r.sc, &html)
}

fn gate(what: &'static str) -> Error {
    Error::Unsupported(unsupported(what))
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

/// Change the range of the selection. Chrome sends `selectionchange` when the selected range differs, not for an equal new range object.
fn set_sel(d: &Dom, r: Option<ObjId>) {
    let old = match state(d).borrow_mut().sel.as_mut() {
        Some(s) => std::mem::replace(&mut s.1, r),
        None => return,
    };
    if old != r && (old.is_none() || r.is_none() || sel_range(d, old) != sel_range(d, r)) {
        d.schedule_doc_selectionchange();
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
        ("getBoundingClientRect", 0, crate::browser_rects::range_bounding_rect),
        ("getClientRects", 0, crate::browser_rects::range_client_rects),
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

/// The boundary points of a range: start container and offset, end container and offset.
pub(crate) fn bounds(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(NodeId, u32, NodeId, u32)> {
    let (_, r) = range_of(d, it, this)?;
    Ok((r.sc, r.so, r.ec, r.eo))
}

impl Dom {
    pub(crate) fn range_rect_item(&self) -> Option<ObjId> {
        self.range.st.get().and_then(|s| s.borrow().rect_item)
    }

    pub(crate) fn set_range_rect_item(&self, f: ObjId) {
        state(self).borrow_mut().rect_item = Some(f);
    }

    pub(crate) fn range_roots(&self, out: &mut Vec<Value>) {
        if let Some(st) = self.range.st.get() {
            let s = st.borrow();
            out.extend(s.made.iter().flatten().copied().map(Value::Object));
            out.extend(s.rect_item.map(Value::Object));
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
            let mut s = st.borrow_mut();
            let had = !s.ranges.is_empty();
            s.ranges.retain(|&o, _| live(o));
            let gone = had && s.ranges.is_empty();
            drop(s);
            if gone {
                self.sync_watcher();
            }
        }
    }

    /// The "replace data" steps for all the text of `node`, which `new` is about to replace (`data = ...`). No range: no work.
    pub(crate) fn range_set_data(&self, doc: &Document, node: NodeId, new: &str) {
        if self.range.any()
            && let NodeData::Text(s) | NodeData::Comment(s) = *doc.data(node)
        {
            self.range_data(node, 0, utf16_len(doc.text(s)), utf16_len(new));
        }
    }

    /// The "replace data" steps for `count` UTF-16 units at `offset` of the text `node`, replaced by `len` units. Call it before the change.
    pub(crate) fn range_data(&self, node: NodeId, offset: u32, count: u32, len: u32) {
        let Some(st) = self.range.st.get() else { return };
        let before = st.borrow().sel_rng();
        for r in st.borrow_mut().ranges.values_mut() {
            for (n, o) in [(r.sc, &mut r.so), (r.ec, &mut r.eo)] {
                if n == node && *o > offset {
                    *o = if *o <= offset.saturating_add(count) { offset } else { (*o - count).saturating_add(len) };
                }
            }
        }
        self.moved_selection(st, before);
    }

    /// The range steps of `splitText(offset)`: `new` is in the tree after `node`. The points in `node` after `offset` go to `new`, and the points
    /// of the parent that sat just after `node` move on by one.
    pub(crate) fn range_split(&self, doc: &Document, node: NodeId, new: NodeId, offset: u32) {
        let Some(st) = self.range.st.get() else { return };
        let parent = doc.parent(node);
        let after = index(doc, node) + 1;
        let before = st.borrow().sel_rng();
        for r in st.borrow_mut().ranges.values_mut() {
            for (n, o) in [(&mut r.sc, &mut r.so), (&mut r.ec, &mut r.eo)] {
                if *n == node && *o > offset {
                    (*n, *o) = (new, *o - offset);
                } else if Some(*n) == parent && *o == after {
                    *o += 1;
                }
            }
        }
        self.moved_selection(st, before);
    }

    /// The range steps of `normalize` for the text node `cur`, which joins `node` after its first `len` units.
    pub(crate) fn range_merge(&self, doc: &Document, node: NodeId, cur: NodeId, len: u32) {
        let Some(st) = self.range.st.get() else { return };
        let (parent, at) = (doc.parent(cur), index(doc, cur));
        let before = st.borrow().sel_rng();
        for r in st.borrow_mut().ranges.values_mut() {
            for (n, o) in [(&mut r.sc, &mut r.so), (&mut r.ec, &mut r.eo)] {
                if *n == cur {
                    (*n, *o) = (node, *o + len);
                } else if Some(*n) == parent && *o == at {
                    (*n, *o) = (node, len);
                }
            }
        }
        self.moved_selection(st, before);
    }

    /// A text change moved the boundary of the selected range: Chrome sends `selectionchange` (a node insert or remove does not).
    fn moved_selection(&self, st: &RefCell<State>, before: Option<Rng>) {
        if before != st.borrow().sel_rng() {
            self.schedule_doc_selectionchange();
        }
    }

    /// Selection API "schedule a selectionchange event" for the document: one task at a time, due now (Chrome, `selchange-cases.txt`).
    fn schedule_doc_selectionchange(&self) {
        let st = state(self);
        if !std::mem::replace(&mut st.borrow_mut().sc_pending, true) && self.schedule(self.now_ms(), Task::DocSelectionChange).is_err() {
            st.borrow_mut().sc_pending = false;
        }
    }

    /// The task runs: the flag is cleared before the event, so a listener's own change queues another.
    pub(crate) fn range_task_done(&self) {
        state(self).borrow_mut().sc_pending = false;
    }
}

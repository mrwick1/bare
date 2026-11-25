//! `HTMLSelectElement`, `HTMLOptionElement`, `HTMLOptGroupElement`, `HTMLOptionsCollection`, `HTMLCollection` and `new Option(...)`.
//! Sources: HTML Standard "The select element", "The option element", "The optgroup element", "The Option() constructor"; the cases are
//! compared with headless Chrome (`tools/fixtures/ui/select-cases.txt`). React DOM's `updateOptions` is one of the cases.
//!
//! What is built
//! - `select`: `options`, `selectedOptions`, `selectedIndex`, `value`, `multiple`, `size`, `required`, `disabled`, `name` (reflected), `type`,
//!   `length` (get and set), `form`, `item`, `namedItem`, `add(element, before)`, `remove()` (no argument: the select leaves its parent) and
//!   `remove(index)`.
//! - `option`: `value` (the attribute, else the stripped and collapsed text), `text` (get and set), `label`, `selected`, `defaultSelected`,
//!   `disabled`, `index`, `form`. `optgroup`: `disabled`, `label`.
//! - `options` is a live `HTMLOptionsCollection` (`length` get and set, `item`, `namedItem`, `add`, `remove`, `selectedIndex`, iteration, index
//!   reads). `selectedOptions` is a live `HTMLCollection`. Both are the host-indexed lists of `browser_nodelist.rs`.
//! - The list of options is the `option` descendants of the select, in tree order. A nested `select` is not entered. (Chrome 155 does this
//!   too, so an `option` in a `div` or in an `optgroup` in an `optgroup` is in the list.)
//!
//! Selectedness
//! - An option has selectedness and dirtiness (`Opt`). The state is native and kept outside the tree: the `selected` attribute is only the
//!   default (`defaultSelected`). While an option is clean, selectedness follows the attribute.
//! - The selectedness setting algorithm is run lazily, not in each tree change. `sel_sync` runs at the start of each API call that reads or
//!   writes a select. It reads the list again when `Document::revision` moved (the revision moves for each attribute and each insert or
//!   remove). If the list, `multiple`, the display size or a `selected` attribute changed, it applies the standard's rules: an option that
//!   was inserted with selectedness true, or that got the attribute, wins in a single select; else more than one selected keeps the first
//!   (Chrome does this when `multiple` goes away); then a single select with display size 1 and no selected option selects the first
//!   option that is not disabled. A change that the script makes through the API keeps the rules at once and moves the revision.
//!   `value` and `selectedIndex` with no match leave no option selected, and nothing selects the first until the list changes.
//! - `input` and `change` are not fired for a script change.
//!
//! Deviations (on purpose)
//! - The rules run at the next API call, not at each insertion. Two inserts of selected options with no API call between them keep the
//!   last in tree order, not the last inserted.
//! - A write to an index of a collection (`select.options[i] = o`, `select[i] = o`, `= null`) is not built: the engine's host-indexed
//!   objects have a read side only (`IndexedSource` has `indexed_len` and `indexed_item`, both `&self`, and no write), so the write is
//!   dropped as for any other read-only exotic object. `select[i]` as a read is not built either.
//! - `autocomplete` and `selectedOptions` of a `select` in a shadow tree being special: not built. Painting a closed select reads the
//!   same state (`Browser::selected_option`).
//!
//! Also here: the reset of the selectedness (`Dom::sel_reset`, for `form.reset()`) and the `valueMissing` test of a select (`value_missing`).
//! `labels`, `validity`, `willValidate`, `validationMessage`, `setCustomValidity()`, `checkValidity()` and `reportValidity()` of a select, and
//! the whole `HTMLTextAreaElement`, are in `browser_controls.rs` and `browser_textarea.rs`.
//!
//! Bounds: none on the options of one select or on the lists.

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_lazy::lazy_global;
use crate::browser_nodelist::{self as nl, Src};
use crate::browser_scoped::*;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use crate::js_num::{to_int32, to_uint32};
use bare_js_api::{Args, HostFn, Local, Type};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

/// What the standard says about the length of the options list: more than this is ignored.
const MAX_LENGTH: u32 = 100_000;

#[derive(Clone, Copy)]
struct Opt {
    selected: bool,
    dirty: bool,
    /// The `selected` attribute when `sel_sync` last looked.
    attr: bool,
    /// The index in the list of the select that last synced this option.
    idx: u32,
}

struct Cache<E: Eng> {
    rev: u64,
    opts: Vec<NodeId>,
    attrs: Vec<bool>,
    /// The indexes (increasing) of the selected options.
    sel: Vec<u32>,
    multiple: bool,
    size1: bool,
    options: Option<E::Obj>,
    chosen: Option<E::Obj>,
}

#[derive(Default)]
struct State<E: Eng> {
    opt: IdTab<NodeId, Opt>,
    cache: IdMap<NodeId, Cache<E>>,
}

#[derive(Clone, Copy)]
struct Made<E: Eng> {
    select: E::Obj,
    option: E::Obj,
    optgroup: E::Obj,
    collection: E::Obj,
    options: E::Obj,
    textarea: E::Obj,
    ctors: [E::Obj; 7],
}

#[derive(Default)]
pub(crate) struct SelHost<E: Eng> {
    st: RefCell<State<E>>,
    made: OnceCell<Made<E>>,
    /// The objects of `made`, in persistent handles: the engine roots them.
    keep: RefCell<Vec<bare_js_api::Persistent>>,
    /// The members of `HTMLTextAreaElement.prototype` are made at the first textarea wrapper or the first read of the global.
    area: Cell<bool>,
    /// The atoms of `value`, `selected` and `disabled`, with the size of the name table (plus one) when the name was last missing.
    atoms: [Cell<(usize, Option<Atom>)>; 3],
}

macro_rules! ch {
    ($it:expr, $n:expr) => {
        if let Some(i) = $it.as_deref_mut() {
            i.charge_o($n)?;
        }
    };
}

/// The atom of an attribute name. The three names that option properties read at each access come from `SelHost::atoms`, so a
/// read does not hash the name. A name that the document lacks stays missing until the name table grows, because a name is never
/// removed. The document of a `Dom` never changes, so a cached atom stays valid.
fn atom<E: Eng>(d: &Dom<E>, doc: &Document, name: &str) -> Option<Atom> {
    let i = match name {
        "value" => 0,
        "selected" => 1,
        "disabled" => 2,
        _ => return doc.find_atom(name),
    };
    let (seen, a) = d.sel.atoms[i].get();
    if a.is_some() || seen == doc.name_count() + 1 {
        return a;
    }
    let a = doc.find_atom(name);
    d.sel.atoms[i].set((doc.name_count() + 1, a));
    a
}

/// `has` and `get` for the reads that run at each option property access.
pub(crate) fn has_in<E: Eng>(d: &Dom<E>, doc: &Document, n: NodeId, name: &str) -> bool {
    atom(d, doc, name).is_some_and(|a| doc.attr(n, a).is_some())
}

fn get_in<'a, E: Eng>(d: &Dom<E>, doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    atom(d, doc, name).and_then(|a| doc.attr(n, a))
}

pub(crate) fn has(doc: &Document, n: NodeId, name: &str) -> bool {
    doc.find_atom(name).is_some_and(|a| doc.attr(n, a).is_some())
}

pub(crate) fn get<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

/// The rules for parsing non-negative integers: leading spaces, an optional `+`, then digits. A value over `i32::MAX` is not valid.
pub(crate) fn parse_u(s: &str) -> Option<u32> {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let s = s.strip_prefix('+').unwrap_or(s);
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    s[..digits].parse::<u32>().ok().filter(|&v| v <= i32::MAX as u32)
}

/// The display size of a select: `size` if it is a number above 0, else 4 for `multiple` and 1.
fn display_size(doc: &Document, select: NodeId) -> u32 {
    match get(doc, select, "size").and_then(parse_u) {
        Some(v) if v > 0 => v,
        _ if has(doc, select, "multiple") => 4,
        _ => 1,
    }
}

/// The options of `select` in tree order. A nested `select` is not entered and a foreign element is skipped.
fn collect<E: Eng>(doc: &Document, it: &mut Option<&mut E::Vm>, select: NodeId) -> R<E, Vec<NodeId>> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = doc.children(select).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        ch!(it, 1);
        if !matches!(*doc.data(n), NodeData::Element { .. }) || doc.is_foreign(n) {
            continue;
        }
        match doc.element_name(n) {
            "option" => {
                out.push(n);
            }
            "select" => {}
            _ => {
                let at = stack.len();
                stack.extend(doc.children(n));
                stack[at..].reverse();
            }
        }
    }
    Ok(out)
}

/// An option is disabled when it has `disabled` or its parent is a disabled `optgroup`.
pub(crate) fn is_disabled(doc: &Document, o: NodeId) -> bool {
    has(doc, o, "disabled") || doc.parent(o).is_some_and(|p| doc.is_html(p, "optgroup") && has(doc, p, "disabled"))
}

fn first_enabled<E: Eng>(d: &Dom<E>, doc: &Document, opts: &[NodeId]) -> Option<usize> {
    let off = |o: NodeId| !has_in(d, doc, o, "disabled") && !doc.parent(o).is_some_and(|p| doc.is_html(p, "optgroup") && has_in(d, doc, p, "disabled"));
    opts.iter().position(|&o| off(o))
}

/// The text of an option: the text of its descendants but not of `script` elements, with the ASCII white space stripped and collapsed.
fn text_of(doc: &Document, o: NodeId) -> String {
    let mut raw = String::new();
    let mut stack: Vec<NodeId> = doc.children(o).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        match *doc.data(n) {
            NodeData::Text(sp) => raw.push_str(doc.text(sp)),
            NodeData::Element { .. } if doc.element_name(n) != "script" => {
                let at = stack.len();
                stack.extend(doc.children(n));
                stack[at..].reverse();
            }
            _ => {}
        }
    }
    let mut out = String::with_capacity(raw.len());
    for w in raw.split_ascii_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

pub(crate) fn value_of(doc: &Document, o: NodeId) -> String {
    get(doc, o, "value").map_or_else(|| text_of(doc, o), str::to_string)
}

fn value_is<E: Eng>(d: &Dom<E>, doc: &Document, o: NodeId, want: &str) -> bool {
    match get_in(d, doc, o, "value") {
        Some(v) => v == want,
        None => text_of(doc, o) == want,
    }
}

impl<E: Eng> Dom<E> {
    /// The state of the select `select` is read again if the document changed since the last call; the rules of the standard run when the
    /// list or a default changed. `it` pays for the walk; the painter has none.
    pub(crate) fn sel_sync(&self, mut it: Option<&mut E::Vm>, select: NodeId) -> R<E, ()> {
        let rev = self.doc.borrow().revision();
        if self.sel.st.borrow().cache.get(&select).is_some_and(|c| c.rev == rev) {
            return Ok(());
        }
        let (list, attrs, multiple, size1) = {
            let doc = self.doc.borrow();
            let list = collect(&doc, &mut it, select)?;
            let attrs: Vec<bool> = list.iter().map(|&o| has_in(self, &doc, o, "selected")).collect();
            let multiple = has(&doc, select, "multiple");
            (list, attrs, multiple, !multiple && display_size(&doc, select) == 1)
        };
        let mut guard = self.sel.st.borrow_mut();
        let st = &mut *guard;
        let mut old = st.cache.remove(&select);
        if let Some(c) = old.as_mut().filter(|c| c.multiple == multiple && c.size1 == size1 && c.opts == list && c.attrs == attrs) {
            c.rev = rev;
            st.cache.insert(select, old.take().expect("some"));
            return Ok(());
        }
        let before: HashSet<NodeId> = old.as_ref().map(|c| c.opts.iter().copied().collect()).unwrap_or_default();
        let (mut newest, mut sel) = (None, Vec::new());
        for (i, &o) in list.iter().enumerate() {
            ch!(it, 1);
            let a = attrs[i];
            let e = st.opt.get_or_insert(o, Opt { selected: a, dirty: false, attr: a, idx: 0 });
            e.idx = i as u32;
            if e.attr != a {
                e.attr = a;
                if !e.dirty {
                    e.selected = a;
                    if a {
                        newest = Some(i as u32);
                    }
                }
            }
            if e.selected {
                if !before.contains(&o) {
                    newest = Some(i as u32);
                }
                sel.push(i as u32);
            }
        }
        if !multiple {
            if sel.len() > 1 {
                let keep = newest.filter(|n| sel.contains(n)).unwrap_or(sel[0]);
                for &j in &sel {
                    if j != keep {
                        st.opt.get_mut(&list[j as usize]).expect("synced").selected = false;
                    }
                }
                sel = vec![keep];
            }
            if sel.is_empty() && size1 {
                let doc = self.doc.borrow();
                if let Some(i) = first_enabled(self, &doc, &list) {
                    st.opt.get_mut(&list[i]).expect("synced").selected = true;
                    sel.push(i as u32);
                }
            }
        }
        let (options, chosen) = old.map_or((None, None), |c| (c.options, c.chosen));
        st.cache.insert(select, Cache { rev, opts: list, attrs, sel, multiple, size1, options, chosen });
        Ok(())
    }

    /// The nearest `select` ancestor of `n`.
    fn owner_select(&self, it: &mut E::Vm, n: NodeId) -> R<E, Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = doc.parent(n);
        while let Some(p) = cur {
            it.charge_o(1)?;
            if doc.is_html(p, "select") {
                return Ok(Some(p));
            }
            cur = doc.parent(p);
        }
        Ok(None)
    }

    /// The state of the option `o`: its select, if it is in the list of one, and its `Opt`.
    fn opt_state(&self, it: &mut E::Vm, o: NodeId) -> R<E, (Option<NodeId>, Opt)> {
        let owner = self.owner_select(it, o)?;
        if let Some(s) = owner {
            self.sel_sync(Some(&mut *it), s)?;
            let st = self.sel.st.borrow();
            if let Some(e) = st.opt.get(&o)
                && st.cache.get(&s).is_some_and(|c| c.opts.get(e.idx as usize) == Some(&o))
            {
                return Ok((Some(s), *e));
            }
        }
        let a = has_in(self, &self.doc.borrow(), o, "selected");
        let mut st = self.sel.st.borrow_mut();
        let e = st.opt.get_or_insert(o, Opt { selected: a, dirty: false, attr: a, idx: 0 });
        if e.attr != a {
            e.attr = a;
            if !e.dirty {
                e.selected = a;
            }
        }
        Ok((None, *e))
    }

    /// The state changed in a way that the tree cannot show: move the revision, and keep the cache of `select` fresh.
    fn sel_commit(&self, select: NodeId) {
        let rev = {
            let mut doc = self.doc.borrow_mut();
            doc.touch();
            doc.revision()
        };
        if let Some(c) = self.sel.st.borrow_mut().cache.get_mut(&select) {
            c.rev = rev;
        }
    }

    /// `value` and `selectedIndex` setters: no option is selected, then option `idx` is, and dirty. Run `sel_sync` before.
    fn sel_only(&self, select: NodeId, idx: Option<usize>) {
        {
            let mut guard = self.sel.st.borrow_mut();
            let st = &mut *guard;
            let c = st.cache.get_mut(&select).expect("synced");
            for &j in &c.sel {
                st.opt.get_mut(&c.opts[j as usize]).expect("synced").selected = false;
            }
            c.sel.clear();
            if let Some(i) = idx {
                let e = st.opt.get_mut(&c.opts[i]).expect("synced");
                (e.selected, e.dirty) = (true, true);
                c.sel.push(i as u32);
            }
        }
        self.sel_commit(select);
    }

    /// The `selected` setter of the option at `idx` of `select`. Run `sel_sync` before.
    fn sel_set(&self, select: NodeId, idx: usize, on: bool) {
        {
            let mut guard = self.sel.st.borrow_mut();
            let st = &mut *guard;
            let c = st.cache.get_mut(&select).expect("synced");
            let e = st.opt.get_mut(&c.opts[idx]).expect("synced");
            (e.selected, e.dirty) = (on, true);
            if on {
                if !c.multiple {
                    for &j in &c.sel {
                        if j as usize != idx {
                            st.opt.get_mut(&c.opts[j as usize]).expect("synced").selected = false;
                        }
                    }
                    c.sel.clear();
                }
                if let Err(at) = c.sel.binary_search(&(idx as u32)) {
                    c.sel.insert(at, idx as u32);
                }
            } else {
                c.sel.retain(|&j| j as usize != idx);
                // "Ask for a reset": a single select with display size 1 selects the first option that is not disabled.
                if !c.multiple && c.size1 && c.sel.is_empty() {
                    let doc = self.doc.borrow();
                    if let Some(i) = first_enabled(self, &doc, &c.opts) {
                        st.opt.get_mut(&c.opts[i]).expect("synced").selected = true;
                        c.sel.push(i as u32);
                    }
                }
            }
        }
        self.sel_commit(select);
    }

    /// Pick the options of `select` that match `wanted` (each by label, else by value; a disabled option never matches), as a user
    /// does. Gives whether the selection changed, or why no option was picked.
    pub(crate) fn sel_pick(&self, it: &mut E::Vm, select: NodeId, wanted: &[String]) -> R<E, Result<bool, String>> {
        self.sel_sync(Some(&mut *it), select)?;
        let (opts, multiple, before) = {
            let st = self.sel.st.borrow();
            let c = &st.cache[&select];
            (c.opts.clone(), c.multiple, c.sel.clone())
        };
        if wanted.len() > 1 && !multiple {
            return Ok(Err("this select takes one option".into()));
        }
        let mut idx = Vec::new();
        {
            let doc = self.doc.borrow();
            let live: Vec<usize> = (0..opts.len()).filter(|&i| !is_disabled(&doc, opts[i])).collect();
            for w in wanted {
                let by = |f: &dyn Fn(NodeId) -> String| live.iter().copied().find(|&i| f(opts[i]) == *w);
                let label = |o: NodeId| get(&doc, o, "label").map_or_else(|| text_of(&doc, o), str::to_string);
                match by(&label).or_else(|| by(&|o| value_of(&doc, o))) {
                    Some(i) => idx.push(i),
                    None => {
                        let have: Vec<String> = live.iter().take(8).map(|&i| format!("{:?}", label(opts[i]))).collect();
                        return Ok(Err(format!("no option {w:?} (options: {}{})", have.join(", "), if live.len() > 8 { ", …" } else { "" })));
                    }
                }
            }
        }
        if multiple {
            for i in 0..opts.len() {
                self.sel_set(select, i, idx.contains(&i));
            }
        } else if let Some(&i) = idx.first() {
            self.sel_set(select, i, true);
        }
        let after = self.sel.st.borrow().cache[&select].sel.clone();
        Ok(Ok(after != before))
    }

    /// The labels of the selected options of `select`, in tree order. `None` when `select` is not a `<select>`.
    pub(crate) fn sel_labels(&self, select: NodeId) -> Option<Vec<String>> {
        if !self.doc.borrow().is_html(select, "select") || self.sel_sync(None, select).is_err() {
            return None;
        }
        let st = self.sel.st.borrow();
        let c = st.cache.get(&select)?;
        let doc = self.doc.borrow();
        Some(c.sel.iter().map(|&i| c.opts[i as usize]).map(|o| get(&doc, o, "label").map_or_else(|| text_of(&doc, o), str::to_string)).collect())
    }

    /// The option that a closed select shows: the selected one. The painter reads it.
    pub(crate) fn selected_option(&self, select: NodeId) -> Option<NodeId> {
        if !self.doc.borrow().is_html(select, "select") || self.sel_sync(None, select).is_err() {
            return None;
        }
        let st = self.sel.st.borrow();
        let c = st.cache.get(&select)?;
        c.sel.first().map(|&i| c.opts[i as usize])
    }

    /// `html_el` on a local.
    pub(crate) fn html_el_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>, tag: &str) -> Res<'s, NodeId> {
        let n = self.el_s(s, this)?;
        if self.doc.borrow().is_html(n, tag) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
    }

    /// `sel_sync` for a call of the host: `s` pays for the walk.
    fn sel_sync_s<'s>(&self, s: &mut Sc<'s, E>, select: NodeId) -> Res<'s, ()> {
        old::<E, _>(s, |it| self.sel_sync(Some(it), select))
    }

    /// The select of `this`, which is a select or one of its collections.
    fn select_or_list_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
        match obj_id::<E>(s, this).and_then(|o| nl::select_of(self, o)) {
            Some(sel) => Ok(sel),
            None => self.html_el_s(s, this, "select"),
        }
    }

    /// The select of `this` (a select or its collection), synced.
    fn select_this_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
        let sel = self.select_or_list_s(s, this)?;
        self.sel_sync_s(s, sel)?;
        Ok(sel)
    }

    fn sel_opts(&self, select: NodeId) -> Vec<NodeId> {
        self.sel.st.borrow().cache[&select].opts.clone()
    }

    fn sel_len(&self, select: NodeId) -> usize {
        self.sel.st.borrow().cache[&select].opts.len()
    }

    fn sel_first(&self, select: NodeId) -> Option<usize> {
        self.sel.st.borrow().cache[&select].sel.first().map(|&i| i as usize)
    }

    fn sel_at(&self, select: NodeId, i: usize) -> Option<NodeId> {
        self.sel.st.borrow().cache[&select].opts.get(i).copied()
    }

    /// The reset steps of `select`: each option takes its `selected` attribute and is clean, then the rules run again (the next `sel_sync`
    /// sees a changed list, and counts each selected option as new, so the last one wins).
    pub(crate) fn sel_reset(&self, it: &mut E::Vm, select: NodeId) -> R<E, ()> {
        self.sel_sync(Some(&mut *it), select)?;
        {
            let (mut guard, doc) = (self.sel.st.borrow_mut(), self.doc.borrow());
            let st = &mut *guard;
            let c = st.cache.get_mut(&select).expect("synced");
            for &o in &c.opts {
                let a = has(&doc, o, "selected");
                let e = st.opt.get_mut(&o).expect("synced");
                *e = Opt { selected: a, dirty: false, attr: a, idx: e.idx };
            }
            (c.rev, c.opts) = (0, Vec::new());
            c.attrs.clear();
        }
        self.doc.borrow_mut().touch();
        Ok(())
    }
}

/// Does the select suffer from being missing (`required` and nothing chosen, or only the placeholder label option)? `browser_controls.rs`.
pub(crate) fn value_missing<E: Eng>(d: &Dom<E>, it: &mut E::Vm, select: NodeId) -> R<E, bool> {
    d.sel_sync(Some(&mut *it), select)?;
    let (st, doc) = (d.sel.st.borrow(), d.doc.borrow());
    let c = &st.cache[&select];
    Ok(c.sel.is_empty() || (c.size1 && c.sel == [0] && doc.parent(c.opts[0]) == Some(select) && value_of(&doc, c.opts[0]).is_empty()))
}

/// The nodes for a list of `select`: all the options, or the selected ones. Called by `browser_nodelist.rs`.
pub(crate) fn list_ids<E: Eng>(d: &Dom<E>, it: &mut E::Vm, select: NodeId, chosen: bool) -> R<E, Vec<NodeId>> {
    d.sel_sync(Some(&mut *it), select)?;
    let st = d.sel.st.borrow();
    let c = &st.cache[&select];
    Ok(if chosen { c.sel.iter().map(|&i| c.opts[i as usize]).collect() } else { c.opts.clone() })
}

/// `HTMLCollection.prototype`, for the collections of other modules (`browser_nodelist::of`).
pub(crate) fn collection_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    Ok(ensure(d, it)?.collection)
}

/// The prototype of a new wrapper of an HTML `select`, `option` or `optgroup`.
pub(crate) fn proto_for<E: Eng>(d: &Dom<E>, it: &mut E::Vm, tag: &str) -> R<E, E::Obj> {
    let m = ensure(d, it)?;
    Ok(match tag {
        "select" => m.select,
        "option" => m.option,
        "textarea" => {
            fill_area(d, it, &m)?;
            m.textarea
        }
        _ => m.optgroup,
    })
}

fn int<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, i32> {
    Ok(to_int32(s.to_number(v)?))
}

fn uint<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, u32> {
    Ok(to_uint32(s.to_number(v)?))
}

fn missing<'s, E: Eng>(s: &mut Sc<'s, E>, on: &str, what: &str) -> bare_js_api::Error<'s> {
    s.type_error(&format!("Failed to execute '{what}' on '{on}': 1 argument required, but only 0 present."))
}

// ------------------------------------------------------------------------------------------ reflection

/// `reflect` for handlers of the new form.
macro_rules! reflect_s {
    ($tag:literal, $attr:literal, $get:ident, $set:ident, str) => {
        fn $get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_el_s(s, this, $tag)?;
            let t = get(&d.doc.borrow(), n, $attr).unwrap_or("").to_string();
            d.js_str_s(s, &t)
        }
        fn $set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_el_s(s, this, $tag)?;
            let t = d.arg_str_s(s, a, 0)?;
            crate::browser_dom::attr_set_s(d, s, n, $attr, &t)?;
            Ok(s.undefined())
        }
    };
    ($tag:literal, $attr:literal, $get:ident, $set:ident, bool) => {
        fn $get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_el_s(s, this, $tag)?;
            Ok(s.boolean($crate::browser_select::has_in(d, &d.doc.borrow(), n, $attr)))
        }
        fn $set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_el_s(s, this, $tag)?;
            let v = arg_s::<E>(s, a, 0);
            if s.to_boolean(v) {
                crate::browser_dom::attr_set_s(d, s, n, $attr, "")?;
            } else {
                d.doc.borrow_mut().remove_attr(n, $attr);
            }
            Ok(s.undefined())
        }
    };
}

pub(crate) use reflect_s;

reflect_s!("select", "multiple", select_multiple, set_select_multiple_attr, bool);
reflect_s!("select", "required", select_required, set_select_required, bool);
reflect_s!("select", "disabled", select_disabled, set_select_disabled, bool);
reflect_s!("select", "name", select_name, set_select_name, str);
reflect_s!("option", "disabled", option_disabled, set_option_disabled, bool);
reflect_s!("optgroup", "disabled", group_disabled, set_group_disabled, bool);
reflect_s!("optgroup", "label", group_label, set_group_label, str);

/// `multiple` changes the rules: the state is read before the write, so the rules see the change.
fn set_select_multiple<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    d.sel_sync_s(s, sel)?;
    set_select_multiple_attr(d, s, this, a)
}

fn select_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_el_s(s, this, "select")?;
    let v = get(&d.doc.borrow(), n, "size").and_then(parse_u).unwrap_or(0);
    Ok(s.number(v as f64))
}

fn set_select_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_el_s(s, this, "select")?;
    let arg0 = arg_s::<E>(s, a, 0);
    let v = uint::<E>(s, arg0)?;
    d.sel_sync_s(s, n)?;
    crate::browser_dom::attr_set_s(d, s, n, "size", &v.to_string())?;
    Ok(s.undefined())
}

fn select_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_el_s(s, this, "select")?;
    let m = has(&d.doc.borrow(), n, "multiple");
    d.js_str_s(s, if m { "select-multiple" } else { "select-one" })
}

fn select_form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_el_s(s, this, "select")?;
    let f = old::<E, _>(s, |it| d.form_owner(it, n))?;
    d.wrap_opt_s(s, f)
}

// ------------------------------------------------------------------------------------------ select

fn select_selected_index<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_this_s(s, this)?;
    Ok(s.number(d.sel_first(sel).map_or(-1.0, |i| i as f64)))
}

fn set_select_selected_index<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_this_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let i = int::<E>(s, arg0)?;
    let len = d.sel_len(sel);
    d.sel_only(sel, usize::try_from(i).ok().filter(|&i| i < len));
    Ok(s.undefined())
}

fn select_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    d.sel_sync_s(s, sel)?;
    let o = d.sel_first(sel).and_then(|i| d.sel_at(sel, i));
    let v = o.map(|o| value_of(&d.doc.borrow(), o)).unwrap_or_default();
    d.js_str_s(s, &v)
}

fn set_select_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    let want = d.arg_str_s(s, a, 0)?;
    d.sel_sync_s(s, sel)?;
    let opts = d.sel_opts(sel);
    let mut at = None;
    {
        let doc = d.doc.borrow();
        for (i, &o) in opts.iter().enumerate() {
            s.charge(1)?;
            if value_is(d, &doc, o, &want) {
                at = Some(i);
                break;
            }
        }
    }
    d.sel_only(sel, at);
    Ok(s.undefined())
}

fn select_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_this_s(s, this)?;
    Ok(s.number(d.sel_len(sel) as f64))
}

/// Grow the list with new empty options at the end of the select, or remove options from its end.
fn set_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_or_list_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let n = uint::<E>(s, arg0)?;
    d.sel_sync_s(s, sel)?;
    if n > MAX_LENGTH {
        return Ok(s.undefined());
    }
    let opts = d.sel_opts(sel);
    let n = n as usize;
    let mut doc = d.doc.borrow_mut();
    if n > opts.len() {
        let add = n - opts.len();
        for _ in 0..add {
            s.charge(1)?;
            let o = doc.create_element("option");
            doc.move_to(o, sel, None);
        }
    } else {
        for &o in opts[n..].iter().rev() {
            s.charge(1)?;
            doc.detach(o);
        }
    }
    Ok(s.undefined())
}

fn select_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    if a.is_empty() {
        return Err(missing::<E>(s, "HTMLSelectElement", "item"));
    }
    let arg0 = arg_s::<E>(s, a, 0);
    let i = uint::<E>(s, arg0)? as usize;
    d.sel_sync_s(s, sel)?;
    let o = d.sel_at(sel, i);
    d.wrap_opt_s(s, o)
}

/// `namedItem`: the first element with that `id` or `name`.
fn named<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, ids: &[NodeId], a: Args<'s>, on: &str) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(missing::<E>(s, on, "namedItem"));
    }
    let name = d.arg_str_s(s, a, 0)?;
    let found = if name.is_empty() {
        None
    } else {
        let doc = d.doc.borrow();
        ids.iter().copied().find(|&o| doc.attr(o, Atom::ID) == Some(&name) || get(&doc, o, "name") == Some(&name))
    };
    d.wrap_opt_s(s, found)
}

fn select_named_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    d.sel_sync_s(s, sel)?;
    let ids = d.sel_opts(sel);
    named(d, s, &ids, a, "HTMLSelectElement")
}

fn collection_named_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(ids) = obj_id::<E>(s, this).map(|o| nl::members(d, s, o)).transpose()?.flatten() else { return Err(s.type_error("Illegal invocation")) };
    named(d, s, &ids, a, "HTMLCollection")
}

fn add_to<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, sel: NodeId, a: Args<'s>, on: &str) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(missing::<E>(s, on, "add"));
    }
    let a0 = arg_s::<E>(s, a, 0);
    let el = d.node_of_s(s, a0).filter(|&n| {
        let doc = d.doc.borrow();
        doc.is_html(n, "option") || doc.is_html(n, "optgroup")
    });
    let Some(el) = el else {
        return Err(s.type_error(&format!("Failed to execute 'add' on '{on}': The provided value is not of type '(HTMLOptGroupElement or HTMLOptionElement)'.")));
    };
    let a1 = arg_s::<E>(s, a, 1);
    let before = if is_nullish::<E>(s, a1) {
        None
    } else {
        match d.node_of_s(s, a1) {
            Some(n) if d.is_element(n) => Some(n),
            Some(_) => return Err(s.type_error(&format!("Failed to execute 'add' on '{on}': The provided value is not of type '(HTMLElement or long)'."))),
            None => {
                let i = int::<E>(s, a1)?;
                d.sel_sync_s(s, sel)?;
                usize::try_from(i).ok().and_then(|i| d.sel_at(sel, i))
            }
        }
    };
    if before == Some(el) {
        return Ok(s.undefined());
    }
    d.insert_checked_s(s, sel, el, before, crate::browser_dom::NO_OP)?;
    Ok(s.undefined())
}

fn select_add<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    add_to(d, s, sel, a, "HTMLSelectElement")
}

fn collection_add<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_or_list_s(s, this)?;
    add_to(d, s, sel, a, "HTMLOptionsCollection")
}

fn remove_at<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, sel: NodeId, v: Local<'s>) -> Res<'s, Local<'s>> {
    let i = int::<E>(s, v)?;
    d.sel_sync_s(s, sel)?;
    if let Some(o) = usize::try_from(i).ok().and_then(|i| d.sel_at(sel, i)) {
        d.doc.borrow_mut().detach(o);
    }
    Ok(s.undefined())
}

/// `remove()` removes the select itself (`ChildNode.remove`); `remove(index)` removes an option.
fn select_remove<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    let Some(v) = a.get(0) else {
        d.doc.borrow_mut().detach(sel);
        return Ok(s.undefined());
    };
    remove_at(d, s, sel, v)
}

fn collection_remove<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sel = d.select_or_list_s(s, this)?;
    let Some(v) = a.get(0) else { return Err(missing::<E>(s, "HTMLOptionsCollection", "remove")) };
    remove_at(d, s, sel, v)
}

/// The cached collection of `select`: `chosen` for `selectedOptions`, else `options`.
fn collection<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, chosen: bool) -> Res<'s, Local<'s>> {
    let sel = d.html_el_s(s, this, "select")?;
    d.sel_sync_s(s, sel)?;
    let cached = {
        let st = d.sel.st.borrow();
        let c = &st.cache[&sel];
        if chosen { c.chosen } else { c.options }
    };
    if let Some(o) = cached {
        return Ok(d.obj_local(s, o));
    }
    let m = old::<E, _>(s, |it| ensure(d, it))?;
    let v = old_v::<E>(s, |it| nl::make_list(d, it, if chosen { m.collection } else { m.options }, if chosen { Src::Selected(sel) } else { Src::Options(sel) }))?;
    let Some(o) = obj_id::<E>(s, v) else { unreachable!() };
    if let Some(c) = d.sel.st.borrow_mut().cache.get_mut(&sel) {
        *(if chosen { &mut c.chosen } else { &mut c.options }) = Some(o);
    }
    Ok(v)
}

fn select_options<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    collection(d, s, this, false)
}

fn select_selected_options<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    collection(d, s, this, true)
}

// ------------------------------------------------------------------------------------------ option

fn option_this<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    d.html_el_s(s, this, "option")
}

fn option_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let doc = d.doc.borrow();
    match get_in(d, &doc, o, "value") {
        Some(v) => d.js_str_s(s, v),
        None => d.js_str_s(s, &text_of(&doc, o)),
    }
}

fn set_option_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let t = d.arg_str_s(s, a, 0)?;
    crate::browser_dom::attr_set_s(d, s, o, "value", &t)?;
    Ok(s.undefined())
}

fn option_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let t = text_of(&d.doc.borrow(), o);
    d.js_str_s(s, &t)
}

fn set_option_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    option_this(d, s, this)?;
    let t = d.arg_str_s(s, a, 0)?;
    let v = d.js_str_s(s, &t)?;
    crate::browser_dom::set_text_content_s(d, s, this, Args::from_slots(v.slot(), 1))
}

fn option_label<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let t = {
        let doc = d.doc.borrow();
        get(&doc, o, "label").map_or_else(|| text_of(&doc, o), str::to_string)
    };
    d.js_str_s(s, &t)
}

fn set_option_label<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let t = d.arg_str_s(s, a, 0)?;
    crate::browser_dom::attr_set_s(d, s, o, "label", &t)?;
    Ok(s.undefined())
}

fn option_selected<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let on = old::<E, _>(s, |it| d.opt_state(it, o))?.1.selected;
    Ok(s.boolean(on))
}

fn set_option_selected<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let on = s.to_boolean(arg0);
    match old::<E, _>(s, |it| d.opt_state(it, o))? {
        (Some(sel), e) => d.sel_set(sel, e.idx as usize, on),
        (None, _) => {
            let mut st = d.sel.st.borrow_mut();
            let e = st.opt.get_mut(&o).expect("made by opt_state");
            (e.selected, e.dirty) = (on, true);
        }
    }
    Ok(s.undefined())
}

fn option_default_selected<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    Ok(s.boolean(has_in(d, &d.doc.borrow(), o, "selected")))
}

fn set_option_default_selected<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    if s.to_boolean(arg0) {
        crate::browser_dom::attr_set_s(d, s, o, "selected", "")?;
    } else {
        d.doc.borrow_mut().remove_attr(o, "selected");
    }
    Ok(s.undefined())
}

fn option_index<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let (sel, e) = old::<E, _>(s, |it| d.opt_state(it, o))?;
    Ok(s.number(if sel.is_some() { e.idx as f64 } else { 0.0 }))
}

fn option_form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = option_this(d, s, this)?;
    let f = old::<E, _>(s, |it| match d.owner_select(it, o)? {
        Some(sel) => d.form_owner(it, sel),
        None => Ok(None),
    })?;
    d.wrap_opt_s(s, f)
}

/// `new Option(text = "", value, defaultSelected = false, selected = false)`.
fn option_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    let text = match s.type_of(a0) {
        Type::Undefined => String::new(),
        _ => d.string_arg_s(s, a0)?,
    };
    let a1 = arg_s::<E>(s, a, 1);
    let value = match s.type_of(a1) {
        Type::Undefined => None,
        _ => Some(d.string_arg_s(s, a1)?),
    };
    let a2 = arg_s::<E>(s, a, 2);
    let default_selected = s.to_boolean(a2);
    let a3 = arg_s::<E>(s, a, 3);
    let selected = s.to_boolean(a3);
    let o = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, text.len())?;
        let o = doc.create_element("option");
        if !text.is_empty() {
            let t = doc.create_text(&text);
            doc.move_to(t, o, None);
        }
        o
    };
    if let Some(v) = value {
        crate::browser_dom::attr_set_s(d, s, o, "value", &v)?;
    }
    if default_selected {
        crate::browser_dom::attr_set_s(d, s, o, "selected", "")?;
    }
    // The constructor sets selectedness from the argument, also when the attribute is set (Chrome: `new Option('a', 'b', true).selected` is false).
    if default_selected || selected {
        d.sel.st.borrow_mut().opt.insert(o, Opt { selected, dirty: selected, attr: default_selected, idx: 0 });
    }
    d.wrap_s(s, o)
}

// ------------------------------------------------------------------------------------------ install

fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, Made<E>> {
    if let Some(&m) = d.sel.made.get() {
        return Ok(m);
    }
    let op = it.object_prototype();
    let html = d.p().html;
    let collection = d.obj(it, op)?;
    let options = d.obj(it, collection)?;
    let (select, option, optgroup, textarea) = (d.obj(it, html)?, d.obj(it, html)?, d.obj(it, html)?, d.obj(it, html)?);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut ctors = [collection; 7];
    for (i, (name, proto)) in [("HTMLSelectElement", select), ("HTMLOptionElement", option), ("HTMLOptGroupElement", optgroup), ("HTMLOptionsCollection", options), ("HTMLCollection", collection), ("HTMLTextAreaElement", textarea)].into_iter().enumerate() {
        let f = crate::browser_custom::interface_ctor(d, it, name, true)?;
        it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
        it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        ctors[i] = f;
    }
    // `Option` has the prototype of `HTMLOptionElement`.
    let f = d.add_fn_s_obj(it, option_ctor, "Option", 0, HostFn::Ctor)?;
    it.define_data(f, kp, E::obj_raw(option), false, false, false)?;
    ctors[6] = f;

    d.def_get_s(it, collection, "length", nl::length, None)?;
    d.def_method_s(it, collection, "item", 1, nl::item_method)?;
    d.def_method_s(it, collection, "namedItem", 1, collection_named_item)?;
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let kv = d.key(it, "values")?;
    let values = it.get_value(E::obj_raw(it.array_prototype()), kv)?;
    it.define_data(collection, sym, values, true, false, true)?;

    d.def_get_s(it, options, "length", nl::length, Some(set_length))?;
    d.def_method_s(it, options, "add", 1, collection_add)?;
    d.def_method_s(it, options, "remove", 1, collection_remove)?;
    d.def_get_s(it, options, "selectedIndex", select_selected_index, Some(set_select_selected_index))?;

    for (name, g, s) in [
        ("selectedIndex", select_selected_index as HS<E>, Some(set_select_selected_index as HS<E>)),
        ("value", select_value, Some(set_select_value)),
        ("multiple", select_multiple, Some(set_select_multiple)),
        ("size", select_size, Some(set_select_size)),
        ("required", select_required, Some(set_select_required)),
        ("disabled", select_disabled, Some(set_select_disabled)),
        ("name", select_name, Some(set_select_name)),
        ("length", select_length, Some(set_length)),
        ("options", select_options, None),
        ("selectedOptions", select_selected_options, None),
        ("type", select_type, None),
        ("form", select_form, None),
    ] {
        d.def_get_s(it, select, name, g, s)?;
    }
    d.def_method_s(it, select, "item", 1, select_item)?;
    d.def_method_s(it, select, "namedItem", 1, select_named_item)?;
    d.def_method_s(it, select, "add", 1, select_add)?;
    d.def_method_s(it, select, "remove", 0, select_remove)?;

    for (name, g, s) in [
        ("value", option_value as HS<E>, Some(set_option_value as HS<E>)),
        ("text", option_text, Some(set_option_text)),
        ("label", option_label, Some(set_option_label)),
        ("selected", option_selected, Some(set_option_selected)),
        ("defaultSelected", option_default_selected, Some(set_option_default_selected)),
        ("disabled", option_disabled, Some(set_option_disabled)),
        ("index", option_index, None),
        ("form", option_form, None),
    ] {
        d.def_get_s(it, option, name, g, s)?;
    }
    crate::browser_controls::decorate(d, it, select, true)?;
    d.def_get_s(it, optgroup, "disabled", group_disabled, Some(set_group_disabled))?;
    d.def_get_s(it, optgroup, "label", group_label, Some(set_group_label))?;

    let m = Made { select, option, optgroup, collection, options, textarea, ctors };
    crate::browser_controls::hold::<E>(it, &d.sel.keep, &[select, option, optgroup, collection, options, textarea]);
    crate::browser_controls::hold::<E>(it, &d.sel.keep, &ctors);
    let _ = d.sel.made.set(m);
    Ok(m)
}

fn pick<E: Eng>(d: &Dom<E>, i: usize) -> E::Obj {
    d.sel.made.get().expect("made").ctors[i]
}

fn ens<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    ensure(d, it).map(|_| ())
}

/// Make the members of the textarea prototype (once). They are 40 functions, so a page with a select but no textarea does not pay.
fn fill_area<E: Eng>(d: &Dom<E>, it: &mut E::Vm, m: &Made<E>) -> R<E, ()> {
    if !d.sel.area.get() {
        crate::browser_textarea::install(d, it, m.textarea)?;
        d.sel.area.set(true);
    }
    Ok(())
}

fn ens_area<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let m = ensure(d, it)?;
    fill_area(d, it, &m)
}

lazy_global!(lazy_select, "HTMLSelectElement", ens, |d| pick(d, 0));
lazy_global!(lazy_option_el, "HTMLOptionElement", ens, |d| pick(d, 1));
lazy_global!(lazy_optgroup, "HTMLOptGroupElement", ens, |d| pick(d, 2));
lazy_global!(lazy_options, "HTMLOptionsCollection", ens, |d| pick(d, 3));
lazy_global!(lazy_collection, "HTMLCollection", ens, |d| pick(d, 4));
lazy_global!(lazy_textarea, "HTMLTextAreaElement", ens_area, |d| pick(d, 5));
lazy_global!(lazy_option, "Option", ens, |d| pick(d, 6));

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "HTMLSelectElement", lazy_select),
        Row::both(1, "HTMLOptionElement", lazy_option_el),
        Row::both(1, "HTMLOptGroupElement", lazy_optgroup),
        Row::both(1, "HTMLOptionsCollection", lazy_options),
        Row::both(1, "HTMLCollection", lazy_collection),
        Row::both(1, "HTMLTextAreaElement", lazy_textarea),
        Row::both(1, "Option", lazy_option),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// Forget the `options` or `selectedOptions` collection `o` of the select `s`, which the collector freed.
pub(crate) fn forget_collection<E: Eng>(d: &Dom<E>, s: NodeId, o: E::Obj) {
    if let Some(c) = d.sel.st.borrow_mut().cache.get_mut(&s) {
        c.options = c.options.filter(|&x| x != o);
        c.chosen = c.chosen.filter(|&x| x != o);
    }
}

impl<E: Eng> Browser<E> {
    /// The labels of the selected options of a `<select>`.
    pub fn selected_labels(&self, select: NodeId) -> Option<Vec<String>> {
        self.dom.sel_labels(select)
    }

    /// The option that a closed `<select>` shows (its first selected option), or `None`.
    pub fn selected_option(&self, select: NodeId) -> Option<NodeId> {
        self.dom.selected_option(select)
    }
}

#[cfg(test)]
impl<E: Eng> Dom<E> {
    /// Has any select, option or list interface been made? (Tests: a page without selects pays nothing.)
    pub(crate) fn sel_made(&self) -> bool {
        self.sel.made.get().is_some()
    }

    /// How many collections (`options`, `selectedOptions`) the selects keep for `===`. (Tests.)
    pub(crate) fn sel_cached(&self) -> usize {
        self.sel.st.borrow().cache.values().map(|c| usize::from(c.options.is_some()) + usize::from(c.chosen.is_some())).sum()
    }
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// Drop the state of the nodes that the reclaim freed. The option lists are caches: the next read builds them again.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, gone: &dyn Fn(NodeId) -> bool) {
    let mut st = d.sel.st.borrow_mut();
    st.opt.retain(|&n, _| !gone(n));
    st.cache.clear();
}

//! `HTMLElement.dataset` and `DOMStringMap` over the native `data-*` attributes (HTML Standard, "The dataset IDL attribute"; WebIDL
//! "legacy platform objects", interface `DOMStringMap` with `[LegacyOverrideBuiltIns]`).
//!
//! Cause: React's stylesheet ordering reads `link.dataset.precedence` and `style.dataset.precedence`. Without `dataset` the commit
//! of a client navigation threw `TypeError: Cannot read properties of undefined (reading 'precedence')`.
//!
//! The native `Document` is the one source of truth. A map stores only its element.
//! - `el.dataset` is one `DOMStringMap` for each element while the element wrapper lives (`SameObject`). The wrapper traces the map and
//!   the map traces the wrapper, as `style` does, so a detached element lives while a page holds its map.
//! - The map is a host named object (`Heap::new_named`, read `heap_named.rs`). The heap asks `Dom` (`NamedSource`) on every read:
//!   nothing is cached, so `setAttribute`, `removeAttribute`, the parser and the driver are seen at once. A write, a define or a
//!   delete goes through the evaluator (`Host::named_write`, `write` here) because it can throw and must charge the budget.
//! - Name conversion: an attribute is in the map when its name starts with `data-` and has no ASCII upper alpha after it. The property
//!   name drops `data-`, and each `-` followed by an ASCII lowercase letter becomes that letter in upper case (`data-foo-bar` is
//!   `fooBar`; `data-a-1` is `a-1`; `data-` is `""`). A property name finds its attribute the other way, and only counts when the
//!   attribute converts back to the same name, so `ds["foo-bar"]` is `undefined` even when `data-foo-bar` exists.
//! - Order: `Object.keys`, `in`, `for in` and `JSON.stringify` follow the order of the attribute list. Other attributes are invisible.
//! - The setter converts the value with `ToString` first, then checks the name: a `-` followed by an ASCII lowercase letter is a
//!   `SyntaxError`; a `data-` name that is not an XML `Name` is an `InvalidCharacterError`. Then it sets the attribute (a new one goes at
//!   the end, an existing one keeps its place). `delete` removes the attribute of a supported name and is `true` for any other name.
//!   `Object.defineProperty` with a data descriptor is a set (the flags are ignored, as WebIDL says); an accessor descriptor is `false`.
//!   The error texts follow Blink as remembered (`'x-y' is not a valid property name.`, `'data-a b' is not a valid attribute name.`). Not checked
//!   against Chrome.
//! - SVG elements have `dataset` too (`SVGElement.prototype`). `setAttribute` keeps the case on an SVG element, so `data-Up` has an
//!   ASCII upper alpha and is not in the map.
//!
//! Caps: map objects count against `max_wrappers` (`dom-wrappers`, checked before one is made). A value or name that grows the document
//! uses the shared checks of `attr_set` (`dom-text`, `dom-attrs`, `dom-names`). Reads never scan a long attribute list for free: an
//! element with more than `SCAN_FREE` attributes makes the heap ask the evaluator first (`HeapError::NeedsHost`, `fill`), which charges
//! the step budget (1 step for 8 attributes, as `getAttributeNames` does), then the read repeats.
//!
//! Gates (never silent): MathML elements have no `dataset`, because they are plain `Element`s with no `MathMLElement` brand. A lone
//! surrogate in a name or value cannot be stored in the UTF-8 document: `Unsupported`. `Object.freeze` and `Object.seal` of a map that has
//! names are refused (`TypeError`); the Standard says `[[PreventExtensions]]` of a legacy platform object is `false`, which is not
//! forced for an empty map. `new DOMStringMap()` is an illegal constructor. A map is not plain data for structured clone.

use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use crate::browser_dom::attr_set;
use bare_html::dom::{NodeData, NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::heap::{HeapError, NamedOp, NamedRequest};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{Cell, OnceCell, RefCell};

/// An element with at most this many attributes is scanned by a read at no charge: the scan costs less than a step.
const SCAN_FREE: usize = 32;

#[derive(Default)]
struct DState {
    /// `DOMStringMap` object -> its element.
    objs: IdTab<ObjId, NodeId>,
    /// element -> its map (`SameObject`).
    by_el: IdTab<NodeId, ObjId>,
}

#[derive(Default)]
pub(crate) struct DatasetHost {
    s: RefCell<DState>,
    proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    /// `fill` charged a long scan: the next read of a long list may go on (one use).
    paid: Cell<bool>,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct DatasetStats {
    pub objects: usize,
}

// ------------------------------------------------------------------------------------ names

/// Is `attr` a dataset attribute: `data-` and then no ASCII upper alpha.
fn is_data(attr: &str) -> bool {
    attr.strip_prefix("data-").is_some_and(|t| !t.bytes().any(|b| b.is_ascii_uppercase()))
}

/// `data-foo-bar` -> the characters of `fooBar`. `None` when `attr` is not a dataset attribute.
fn prop_chars(attr: &str) -> Option<impl Iterator<Item = char> + '_> {
    let tail = attr.strip_prefix("data-").filter(|t| !t.bytes().any(|b| b.is_ascii_uppercase()))?;
    let mut cs = tail.chars().peekable();
    Some(std::iter::from_fn(move || {
        let c = cs.next()?;
        match cs.peek() {
            Some(&n) if c == '-' && n.is_ascii_lowercase() => {
                cs.next();
                Some(n.to_ascii_uppercase())
            }
            _ => Some(c),
        }
    }))
}

/// `data-foo-bar` -> `fooBar`. `None` when `attr` is not a dataset attribute.
fn prop_of(attr: &str) -> Option<String> {
    let cs = prop_chars(attr)?;
    let mut out = String::with_capacity(attr.len());
    out.extend(cs);
    Some(out)
}

/// Does the attribute `attr` have the property name `key` (UTF-16)? Same as `prop_of(attr)` equals `key`, with no allocation.
#[inline]
fn matches(attr: &str, key: &[u16]) -> bool {
    let Some(tail) = attr.strip_prefix("data-") else { return false };
    if tail.is_ascii() {
        // The common case: bytes against UTF-16 units. A lone surrogate in `key` never equals an ASCII unit.
        let (b, mut k) = (tail.as_bytes(), 0);
        let mut i = 0;
        while i < b.len() {
            let mut c = b[i];
            if c.is_ascii_uppercase() {
                return false;
            }
            if c == b'-' && i + 1 < b.len() && b[i + 1].is_ascii_lowercase() {
                i += 1;
                c = b[i].to_ascii_uppercase();
            }
            i += 1;
            if key.get(k) != Some(&u16::from(c)) {
                return false;
            }
            k += 1;
        }
        return k == key.len();
    }
    let mut want = char::decode_utf16(key.iter().copied());
    let mut cs = tail.chars().peekable();
    while let Some(c) = cs.next() {
        // An uppercase letter is not a dataset name: the scan stops at the first one, not before the loop.
        if c.is_ascii_uppercase() {
            return false;
        }
        let e = match cs.peek() {
            Some(&n) if c == '-' && n.is_ascii_lowercase() => {
                cs.next();
                n.to_ascii_uppercase()
            }
            _ => c,
        };
        if want.next() != Some(Ok(e)) {
            return false;
        }
    }
    want.next().is_none()
}

/// The attribute name of a property name: each ASCII upper alpha becomes `-` and its lowercase, behind `data-`.
fn attr_of(prop: &str) -> String {
    let mut out = String::with_capacity(5 + prop.len());
    out.push_str("data-");
    for c in prop.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// A character that an XML `Name` may have after its first character (XML 1.0, production 4a).
fn name_char(c: char) -> bool {
    matches!(c,
        ':' | '_' | '-' | '.' | 'A'..='Z' | 'a'..='z' | '0'..='9' | '\u{B7}' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{37D}'
        | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{203F}'..='\u{2040}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}'
        | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

// ------------------------------------------------------------------------------------ heap hooks

impl Dom {
    /// The element of the map `owner`. `Ok(None)`: not a map of this host.
    fn dataset_el(&self, owner: ObjId) -> Result<Option<NodeId>, HeapError> {
        Ok(self.dataset.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?.objs.get(&owner).copied())
    }

    /// Ask for the charge of a long scan before it runs (see the header). `Ok` when the scan may run now.
    fn dataset_gate(&self, owner: ObjId, n: usize) -> Result<(), HeapError> {
        if n <= SCAN_FREE || self.dataset.paid.replace(false) {
            return Ok(());
        }
        Err(HeapError::NeedsHost { owner, index: u32::MAX })
    }
}

/// The UTF-16 units of `cs`, in one allocation when `cap` is at least their number (`collect` grows step by step).
fn units(cs: impl Iterator<Item = char>, cap: usize) -> Vec<u16> {
    let mut v = Vec::with_capacity(cap);
    for c in cs {
        match u16::try_from(c) {
            Ok(u) if !(0xD800..0xE000).contains(&u) => v.push(u),
            _ => v.extend_from_slice(c.encode_utf16(&mut [0; 2])),
        }
    }
    v
}

/// Is `key` a name of the map `owner` now? `browser_sources.rs` calls these for the kind `Dataset`.
#[inline]
pub(crate) fn has(d: &Dom, owner: ObjId, key: &[u16]) -> Result<bool, HeapError> {
    let Some(el) = d.dataset_el(owner)? else { return Ok(false) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    d.dataset_gate(owner, doc.attr_count(el))?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(false) };
    Ok(doc.attrs(attrs).iter().any(|a| matches(doc.atom_str(a.name), key)))
}

/// The value of name `key` of the map `owner`. No gate: the heap calls this right after `has`, and that call paid for the scan.
#[inline]
pub(crate) fn get(d: &Dom, owner: ObjId, key: &[u16]) -> Result<Option<bare_js_api::Named<Value>>, HeapError> {
    let Some(el) = d.dataset_el(owner)? else { return Ok(None) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(None) };
    Ok(doc.attrs(attrs).iter().find(|a| matches(doc.atom_str(a.name), key)).map(|a| {
        let t = doc.text(a.value);
        bare_js_api::Named::Text(units(t.chars(), t.len()))
    }))
}

/// The number of names of the map `owner`.
#[inline]
pub(crate) fn len(d: &Dom, owner: ObjId) -> Result<usize, HeapError> {
    let Some(el) = d.dataset_el(owner)? else { return Ok(0) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    d.dataset_gate(owner, doc.attr_count(el))?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(0) };
    Ok(doc.attrs(attrs).iter().filter(|a| is_data(doc.atom_str(a.name))).count())
}

/// The names of the map `owner`. No gate here: the heap calls `len` first, and that call paid for the scan.
#[inline]
pub(crate) fn keys(d: &Dom, owner: ObjId) -> Result<Vec<Vec<u16>>, HeapError> {
    let Some(el) = d.dataset_el(owner)? else { return Ok(Vec::new()) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(Vec::new()) };
    Ok(doc.attrs(attrs).iter().filter_map(|a| {
        let n = doc.atom_str(a.name);
        prop_chars(n).map(|cs| units(cs, n.len()))
    }).collect())
}

/// `Host::fill_indexed` for a map: charge the long scan that the heap announced. `Ok(false)`: `owner` is not a map.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId) -> R<bool> {
    let Some(&el) = d.dataset.s.borrow().objs.get(&owner) else { return Ok(false) };
    let n = d.doc.borrow().attr_count(el);
    it.charge(1 + n as u64 / 8)?;
    d.dataset.paid.set(true);
    Ok(true)
}

// ------------------------------------------------------------------------------------ writes

/// The text of a property key (a string key, or an array index as its digits).
fn key_text(d: &Dom, it: &mut Interp<'_>, key: PropertyKey) -> R<String> {
    let v = it.heap.key_to_value(key).map_err(|e| it.heap_error(e))?;
    d.string_arg(it, v)
}

/// `Host::named_write` for a map: the setter, the deleter and `[[DefineOwnProperty]]` of the Standard.
pub(crate) fn write(d: &Dom, it: &mut Interp<'_>, req: NamedRequest) -> R<bool> {
    let Some(&el) = d.dataset.s.borrow().objs.get(&req.owner) else { return Ok(false) };
    match req.op {
        NamedOp::Set(v) => set(d, it, el, req.key, v).map(|_| true),
        NamedOp::Define(desc) => {
            // WebIDL: only a data descriptor (a `value` or a `writable` field) runs the setter. Its flags are ignored.
            if desc.value.is_none() && desc.writable.is_none() {
                return Ok(false);
            }
            set(d, it, el, req.key, desc.value.unwrap_or(Value::Undefined)).map(|_| true)
        }
        NamedOp::Delete => {
            let name = key_text(d, it, req.key)?;
            let mut doc = d.doc.borrow_mut();
            it.charge(1 + doc.attr_count(el) as u64 / 8)?;
            let attr = attr_of(&name);
            // Only a supported name: `data-keep-me` is the property `keepMe`, so `delete ds["keep-me"]` removes nothing.
            if let Some(at) = doc.find_attr_qname(el, &attr)
                && prop_of(&attr).as_deref() == Some(name.as_str())
            {
                doc.remove_attr_atom(el, at);
            }
            Ok(true)
        }
    }
}

/// The named setter: the value first (`ToString` can run script), then the name, then the attribute.
fn set(d: &Dom, it: &mut Interp<'_>, el: NodeId, key: PropertyKey, v: Value) -> R<()> {
    let value = d.string_arg(it, v)?;
    let name = key_text(d, it, key)?;
    let b = name.as_bytes();
    if b.windows(2).any(|w| w[0] == b'-' && w[1].is_ascii_lowercase()) {
        return Err(d.dom_err(it, "SyntaxError", &format!("'{name}' is not a valid property name.")));
    }
    let attr = attr_of(&name);
    if !attr.chars().all(name_char) {
        return Err(d.dom_err(it, "InvalidCharacterError", &format!("'{attr}' is not a valid attribute name.")));
    }
    attr_set(d, it, el, &attr, &value)
}

// ------------------------------------------------------------------------------------ the attribute

/// `el.dataset` for an element of the right kind: `svg` for an SVG element, else an HTML one.
fn dataset_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, svg: bool) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    let right = {
        let doc = d.doc.borrow();
        if svg { doc.element_ns(el) == Ns::Svg } else { !doc.is_foreign(el) }
    };
    if !right {
        return Err(s.type_error("Illegal invocation"));
    }
    if let Some(&o) = d.dataset.s.borrow().by_el.get(&el) {
        return Ok(d.obj_local(s, o));
    }
    if d.dataset.s.borrow().objs.len() >= d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
    let proto = *d.dataset.proto.get().expect("installed");
    let o = d.new_source_s(s, crate::browser_sources::Source::Dataset, Some(proto))?;
    let mut st = d.dataset.s.borrow_mut();
    st.objs.insert(o, el);
    st.by_el.insert(el, o);
    Ok(d.obj_local(s, o))
}

fn html_dataset<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    dataset_of(d, s, this, false)
}

fn svg_dataset<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    dataset_of(d, s, this, true)
}

// ------------------------------------------------------------------------------------ install and GC hooks

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let p = d.p();
    let proto = d.obj(it, it.object_prototype())?;
    let _ = d.dataset.proto.set(proto);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn_s_obj(it, illegal_constructor, "DOMStringMap", 0, bare_js_api::HostFn::Ctor)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    let k = d.key(it, "DOMStringMap")?;
    it.define_data(g, k, Value::Object(f), true, false, true)?;
    // `DOMStringMap.prototype[Symbol.toStringTag]` is "DOMStringMap".
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sy, kt)? {
        // Not `js_str`: a constant of the setup is not script data, and a small `max_string_bytes` must not stop the browser.
        let tag = Value::String(it.heap.intern_str("DOMStringMap").map_err(|e| it.heap_error(e))?);
        it.define_data(proto, PropertyKey::Symbol(s), tag, false, false, true)?;
    }
    d.def_get_s(it, p.html, "dataset", html_dataset, None)?;
    d.def_get_s(it, p.svg, "dataset", svg_dataset, None)?;
    *d.dataset.fixed.borrow_mut() = vec![proto, f];
    Ok(())
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.dataset.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

/// The references of the dataset object `o` (its owner entry, `browser_sources.rs`): the wrapper of its element.
pub(crate) fn trace_obj(d: &Dom, o: ObjId, t: &mut bare_js_api::Tracer<'_, Value>) {
    if let Some(&el) = d.dataset.s.borrow().objs.get(&o)
        && let Some(w) = d.st.borrow().by_node.get(&el)
    {
        t.visit(w);
    }
}

/// The references of the wrapper of the element `n`: its dataset object.
pub(crate) fn trace_node(d: &Dom, n: NodeId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let s = d.dataset.s.borrow();
    if !s.by_el.is_empty()
        && let Some(&o) = s.by_el.get(&n)
    {
        crate::browser_sources::visit(t, Value::Object(o));
    }
}

/// How many dataset objects there are.
pub(crate) fn count(d: &Dom) -> usize {
    d.dataset.s.borrow().objs.len()
}

/// After a batch of `prune_obj` that dropped entries: free the empty pages of the tables (`IdTab::remove`).
pub(crate) fn compact(d: &Dom, before: usize) {
    let mut s = d.dataset.s.borrow_mut();
    if s.objs.len() != before {
        s.objs.retain(|_, _| true);
        s.by_el.retain(|_, _| true);
    }
}

/// Drop the entries of the dataset object `o`, which the collector freed.
pub(crate) fn prune_obj(d: &Dom, o: ObjId) {
    let mut s = d.dataset.s.borrow_mut();
    if let Some(el) = s.objs.remove(&o)
        && s.by_el.get(&el) == Some(&o)
    {
        s.by_el.remove(&el);
    }
}

impl Dom {
    /// Is `o` a `DOMStringMap` of this module (so not plain data for structured clone)?
    pub(crate) fn is_dataset_object(&self, o: ObjId) -> bool {
        self.dataset.s.borrow().objs.contains_key(&o)
    }
}

impl Browser {
    pub fn dataset_stats(&self) -> DatasetStats {
        DatasetStats { objects: self.dom.dataset.s.borrow().objs.len() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn names_convert_and_round_trip() {
        for (attr, prop) in [("data-foo-bar", "fooBar"), ("data-x", "x"), ("data-a-1", "a-1"), ("data-", ""), ("data--a", "A"), ("data-a--b", "a-B"), ("data-a-", "a-"), ("data-é-ü", "é-ü")] {
            assert_eq!(prop_of(attr).as_deref(), Some(prop), "{attr}");
            assert!(matches(attr, &units(prop)), "{attr}");
            assert_eq!(attr_of(prop), attr, "{prop}");
        }
        // Not in the map: another prefix, an ASCII upper alpha after `data-`.
        for attr in ["id", "data", "dat-a", "Data-x", "data-Up", "data-aB"] {
            assert_eq!(prop_of(attr), None, "{attr}");
            assert!(!is_data(attr));
        }
        // `matches` stops at an upper alpha, wherever it is.
        for (attr, prop) in [("data-Up", "Up"), ("data-aB", "aB"), ("data-a-bC", "aBC"), ("data-ab-C", "abC")] {
            assert!(!matches(attr, &units(prop)), "{attr}");
        }
        // A property name matches only the attribute that converts back to it.
        assert!(!matches("data-foo-bar", &units("foo-bar")));
        assert!(!matches("data-foo-bar", &units("fooBa")));
        assert!(!matches("data-foo-bar", &units("fooBarX")));
        assert!(!matches("data-x", &[0xD800]));
    }

    #[test]
    fn xml_name_characters() {
        assert!("data-foo_bar.baz:q-1".chars().all(name_char));
        assert!("data-é".chars().all(name_char));
        assert!(!"data-a b".chars().all(name_char));
        assert!(!"data-a!".chars().all(name_char));
        assert!(!"data-a=".chars().all(name_char));
        assert!(!"data-\u{0}".chars().all(name_char));
    }
}

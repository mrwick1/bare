//! `HTMLElement.style` and `CSSStyleDeclaration` over the native `style` attribute (CSSOM, "CSSStyleDeclaration").
//!
//! - The declaration block stores nothing but its element. Every call reads the `style` attribute at that moment, parses
//!   it into a declaration block (`browser_style_block.rs`, where the model is described), and for a change writes the
//!   serialized block back with the normal attribute path (`attr_set`: caps, charge, native cache invalidation). So
//!   `setAttribute("style", ...)`, `cssText`, the named properties and the native selector/cascade code always agree.
//! - `el.style` is one object for each element while the element wrapper lives (`SameObject`): the wrapper traces the
//!   declaration and the declaration traces the wrapper. The setter forwards to `cssText` (`[PutForwards=cssText]`).
//! - Supported properties are exactly the ones `bare_css` understands: `values::Property::ALL` and `values::SHORTHANDS`.
//!   For each one the prototype has camelCase and dashed accessors (and `cssFloat`; `-webkit-` names also get the
//!   `webkitX` form), so `"x" in style` is true only for a property that really works. `WebkitAnimation`, `animation`,
//!   `transition` and the like are NOT supported: absent, and `setProperty` ignores them.
//! - Order of work: arguments are converted with `ToString` first (user code may run, no borrow is held), then the
//!   block is read, changed in Rust and written once. A value that does not parse, an unsupported property, or a
//!   priority other than `""`/`important` changes nothing (no write, no exception), as the standard says.
//! - A shorthand owns its longhands: `style.margin = "1px"` gives `marginTop` `1px`, `length` 4, and
//!   `removeProperty("margin-top")` leaves three. A later write takes the longhands it sets from the older declarations
//!   (CSSOM: one value per longhand) and goes where the first of them was. See `browser_style_block.rs`.
//! - Caps (`StyleLimits`, `Limit(Host(name))`, never catchable): `style-objects`, `style-bytes` (one block, one value,
//!   the output, checked before each piece is added), `style-decls` (declarations of the block, counted while the parser
//!   reads, so the parser stops at the cap; the count includes declarations that only the cascade understands, and
//!   a repeated name counts each time). Scans are charged to the step budget before they run. Text growth is the native
//!   document's cap (`dom-text`): every write adds the new attribute text to the arena, which never shrinks.
//!
//! Gates and deviations (on purpose, none is silently faked)
//! - A value is kept as written (trimmed). It is not canonicalized (`red` stays `red`, `0` stays `0`, Chrome gives `0px`).
//!   An unchanged shorthand is written as authored (`margin: 1px 1px` stays); one built from longhands is the shortest form.
//!   A longhand the author did not write (`flex: 2` sets `flex-basis`, `border: solid` sets the width) is written from the
//!   engine's typed value: `0%`, `3px` (Chrome: `medium`), `400` (Chrome: `normal`).
//! - Declarations the engine does not read (an unknown property, a value it refuses) are kept in the attribute, in order,
//!   and are invisible here (`length`, `item`, `getPropertyValue`). A browser would drop a value it refuses; the native
//!   cascade drops it too. Text that is not a declaration (`oops;`) is not kept.
//! - `getPropertyValue` of a shorthand the model cannot build from longhands is `""`: `flex-flow`, `background`, `font`,
//!   `list-style`, `text-decoration`, `grid-row`, `grid-column`, `grid-area` unless one declaration of that name is
//!   still whole. A shorthand with `var()` has no longhand text (`""`), and writing or removing one of its longhands is
//!   `Unsupported` before anything changes.
//! - Lone surrogates are `Unsupported` at every string entry (the document is UTF-8), never ignored.
//! - No indexed access (`style[0]`), no iterator, no `getPropertyCSSValue`, `parentRule` is `null`. No computed style
//!   (`getComputedStyle`), no SVG `style`, no `innerHTML`/`outerHTML` parse of `style`, no layout reads.
//! - `new CSSStyleDeclaration()` and a subclass of it throw `TypeError: Illegal constructor`.

use crate::browser::*;
use crate::browser_dom::attr_set;
use crate::browser_platform::{V, arg};
use crate::browser_style_block::{Decl, Fail, Lim, custom, get, parse, priority, remove, serialize, set};
use crate::browser_style_block as blk;
use bare_css::values::{Property, SHORTHANDS};
use bare_js::eval::Error;
use std::borrow::Cow;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

/// Bounds of the style module. Each is checked before the thing grows (`Limit(Host(name))`).
#[derive(Clone, Copy, Debug)]
pub struct StyleLimits {
    /// Live `CSSStyleDeclaration` objects. `style-objects`.
    pub max_objects: usize,
    /// Bytes of one declaration block (the attribute, or a `cssText`/value that JS gives). `style-bytes`.
    pub max_bytes: usize,
    /// Kept declarations of one block. `style-decls`.
    pub max_decls: usize,
}

impl Default for StyleLimits {
    fn default() -> Self {
        StyleLimits { max_objects: 1 << 16, max_bytes: 256 << 10, max_decls: 8192 }
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct StyleStats {
    pub objects: usize,
}

#[derive(Default)]
struct SState {
    /// `CSSStyleDeclaration` object -> its element.
    objs: HashMap<ObjId, NodeId>,
    /// element -> its `style` object (`SameObject`).
    by_el: HashMap<NodeId, ObjId>,
}

#[derive(Default)]
pub(crate) struct Style {
    lim: Cell<StyleLimits>,
    s: RefCell<SState>,
    proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    /// The host function id of the call in progress (set by `Dom::call`; the named accessors read it first).
    pub(crate) cur: Cell<u32>,
    /// Function id of a named getter or setter -> index in `props`.
    fns: RefCell<HashMap<u32, usize>>,
    /// The supported property names, in CSS form.
    props: OnceCell<Vec<&'static str>>,
}

/// `-webkit-x-y` -> `WebkitXY`, `margin-top` -> `marginTop` (CSSOM "CSS property to IDL attribute").
fn camel(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut up = false;
    for c in css.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The IDL attribute names of one property.
fn keys_of(css: &str) -> Vec<String> {
    let camel = camel(css);
    let mut keys = vec![camel.clone()];
    if css.contains('-') {
        keys.push(css.to_string());
    }
    if css.starts_with("-webkit-") {
        keys.push(format!("w{}", &camel[1..]));
    }
    if css == "float" {
        keys.push("cssFloat".to_string());
    }
    keys
}

fn eq_important(s: &str) -> bool {
    s.eq_ignore_ascii_case("important")
}

/// The name as the block keeps it: a custom property as written, anything else in lowercase.
fn key(name: &str) -> Cow<'_, str> {
    if custom(name) { Cow::Borrowed(name) } else { Cow::Owned(name.to_ascii_lowercase()) }
}

impl Dom {
    fn style_lim(&self) -> StyleLimits {
        self.style.lim.get()
    }

    fn blim(&self) -> Lim {
        let l = self.style_lim();
        Lim { bytes: l.max_bytes, decls: l.max_decls }
    }

    /// The error of a block operation. A cap is `Limit(Host(name))`.
    fn style_fail(&self, f: Fail) -> Error {
        match f {
            Fail::Bytes => self.lim("style-bytes"),
            Fail::Decls => self.lim("style-decls"),
            Fail::Unsupported(what) => Error::Unsupported(unsupported(what)),
            Fail::Err(e) => e,
        }
    }

    /// Parse a block of declarations (an attribute value or `cssText`).
    fn style_parse(&self, it: &mut Interp<'_>, text: &str) -> R<Vec<Decl>> {
        parse(text, self.blim(), &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))
    }

    /// The block of the `style` attribute now.
    fn style_read(&self, it: &mut Interp<'_>, el: NodeId) -> R<Vec<Decl>> {
        let doc = self.doc.borrow();
        let text = doc.find_atom("style").and_then(|a| doc.attr(el, a)).unwrap_or("");
        self.style_parse(it, text)
    }

    fn style_text(&self, it: &mut Interp<'_>, list: &[Decl]) -> R<String> {
        serialize(list, self.blim(), &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))
    }

    /// The update steps: write the serialized block back (skipped when the attribute already has exactly that text).
    fn style_write(&self, it: &mut Interp<'_>, el: NodeId, list: &[Decl]) -> R<()> {
        let text = self.style_text(it, list)?;
        {
            let doc = self.doc.borrow();
            if doc.find_atom("style").and_then(|a| doc.attr(el, a)) == Some(text.as_str()) {
                return Ok(());
            }
        }
        attr_set(self, it, el, "style", &text)
    }

    fn style_set(&self, it: &mut Interp<'_>, el: NodeId, name: &str, value: &str, priority: &str) -> R<()> {
        let name = key(name);
        if value.is_empty() {
            return self.style_remove(it, el, &name).map(|_| ());
        }
        let important = if priority.is_empty() {
            false
        } else if eq_important(priority) {
            true
        } else {
            return Ok(());
        };
        let mut list = self.style_read(it, el)?;
        if !set(&mut list, &name, value, important, self.blim(), &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))? {
            return Ok(());
        }
        self.style_write(it, el, &list)
    }

    /// `removeProperty`: the old value of the property (`""` if none).
    fn style_remove(&self, it: &mut Interp<'_>, el: NodeId, name: &str) -> R<String> {
        let mut list = self.style_read(it, el)?;
        let (old, changed) = remove(&mut list, &key(name), &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))?;
        if changed {
            self.style_write(it, el, &list)?;
        }
        Ok(old)
    }

    fn style_get(&self, it: &mut Interp<'_>, el: NodeId, name: &str) -> R<Option<String>> {
        let list = self.style_read(it, el)?;
        get(&list, &key(name), &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))
    }

/// The element of a `CSSStyleDeclaration` object.
    fn style_el(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        if let Value::Object(o) = this
            && let Some(&el) = self.style.s.borrow().objs.get(&o)
        {
            return Ok(el);
        }
        Err(it.type_error("Illegal invocation"))
    }

    /// An HTML element (`style` is on `HTMLElement`, not on `Element`).
    fn style_html_el(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        let n = self.el(it, this)?;
        let doc = self.doc.borrow();
        let NodeData::Element { name, .. } = *doc.data(n) else { unreachable!() };
        if matches!(doc.atom_str(name), "svg" | "math") {
            drop(doc);
            return Err(it.type_error("Illegal invocation"));
        }
        Ok(n)
    }

    pub(crate) fn is_style_object(&self, o: ObjId) -> bool {
        self.style.s.borrow().objs.contains_key(&o)
    }
}

/// WebIDL `[LegacyNullToEmptyString] DOMString`: `null` is `""`.
fn null_empty(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<String> {
    if matches!(v, Value::Null) { Ok(String::new()) } else { d.string_arg(it, v) }
}

fn required(it: &mut Interp<'_>, a: &[Value], n: usize, name: &str) -> R<()> {
    if a.len() < n {
        return Err(it.type_error(&format!(
            "Failed to execute '{name}' on 'CSSStyleDeclaration': {n} argument{} required, but only {} present.",
            if n == 1 { "" } else { "s" },
            a.len()
        )));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ JS side

/// `HTMLElement.prototype.style`
fn style_attr(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    if let Some(&o) = d.style.s.borrow().by_el.get(&el) {
        return Ok(Value::Object(o));
    }
    if d.style.s.borrow().objs.len() >= d.style_lim().max_objects {
        return Err(d.lim("style-objects"));
    }
    let o = d.obj(it, *d.style.proto.get().expect("installed"))?;
    let mut s = d.style.s.borrow_mut();
    s.objs.insert(o, el);
    s.by_el.insert(el, o);
    Ok(Value::Object(o))
}

fn set_style_attr(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    d.style_set_text(it, el, arg(a, 0))?;
    Ok(Value::Undefined)
}

impl Dom {
    /// `cssText` setter: the attribute is set even when the block is empty (the standard always sets it).
    fn style_set_text(&self, it: &mut Interp<'_>, el: NodeId, v: Value) -> R<()> {
        let text = null_empty(self, it, v)?;
        let list = self.style_parse(it, &text)?;
        let text = self.style_text(it, &list)?;
        attr_set(self, it, el, "style", &text)
    }
}

fn css_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    let list = d.style_read(it, el)?;
    let text = d.style_text(it, &list)?;
    d.js_str(it, &text)
}

fn set_css_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    d.style_set_text(it, el, arg(a, 0))?;
    Ok(Value::Undefined)
}

fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    Ok(Value::Number(blk::length(&d.style_read(it, el)?) as f64))
}

fn parent_rule(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.style_el(it, this)?;
    Ok(Value::Null)
}

fn item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    required(it, a, 1, "item")?;
    let i = crate::browser_platform_class::to_u32(it, a[0])? as usize;
    let name = blk::item(&d.style_read(it, el)?, i).unwrap_or_default();
    d.js_str(it, &name)
}

fn get_property_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    required(it, a, 1, "getPropertyValue")?;
    let name = d.string_arg(it, a[0])?;
    let v = d.style_get(it, el, &name)?.unwrap_or_default();
    d.js_str(it, &v)
}

fn get_property_priority(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    required(it, a, 1, "getPropertyPriority")?;
    let name = d.string_arg(it, a[0])?;
    let imp = priority(&d.style_read(it, el)?, &key(&name));
    d.js_str(it, if imp { "important" } else { "" })
}

fn set_property(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    required(it, a, 2, "setProperty")?;
    let name = d.string_arg(it, a[0])?;
    let value = null_empty(d, it, a[1])?;
    let prio = match a.get(2) {
        None | Some(Value::Undefined) => String::new(),
        Some(&v) => null_empty(d, it, v)?,
    };
    d.style_set(it, el, &name, &value, &prio)?;
    Ok(Value::Undefined)
}

fn remove_property(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.style_el(it, this)?;
    required(it, a, 1, "removeProperty")?;
    let name = d.string_arg(it, a[0])?;
    let old = d.style_remove(it, el, &name)?;
    d.js_str(it, &old)
}

/// The property of the named accessor that is running.
fn current(d: &Dom, it: &mut Interp<'_>) -> R<&'static str> {
    let i = d.style.fns.borrow().get(&d.style.cur.get()).copied();
    match (i, d.style.props.get()) {
        (Some(i), Some(p)) => Ok(p[i]),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn named_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let name = current(d, it)?;
    let el = d.style_el(it, this)?;
    let v = d.style_get(it, el, name)?.unwrap_or_default();
    d.js_str(it, &v)
}

fn named_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let name = current(d, it)?;
    let el = d.style_el(it, this)?;
    let value = null_empty(d, it, arg(a, 0))?;
    d.style_set(it, el, name, &value, "")?;
    Ok(Value::Undefined)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let html = d.p().html;
    let proto = d.obj(it, it.object_prototype())?;
    let _ = d.style.proto.set(proto);
    let mut fixed = vec![proto];
    d.def_get(it, proto, "cssText", css_text, Some(set_css_text))?;
    d.def_get(it, proto, "length", length, None)?;
    d.def_get(it, proto, "parentRule", parent_rule, None)?;
    d.def_method(it, proto, "item", 1, item)?;
    d.def_method(it, proto, "getPropertyValue", 1, get_property_value)?;
    d.def_method(it, proto, "getPropertyPriority", 1, get_property_priority)?;
    d.def_method(it, proto, "setProperty", 2, set_property)?;
    d.def_method(it, proto, "removeProperty", 1, remove_property)?;

    let mut props: Vec<&'static str> = Property::ALL.iter().map(|p| p.name()).collect();
    props.extend(SHORTHANDS);
    for (i, css) in props.iter().enumerate() {
        let keys = keys_of(css);
        let (gid, sid) = (d.handlers.borrow().len() as u32, d.handlers.borrow().len() as u32 + 1);
        let gf = d.add_fn(it, named_get, &format!("get {}", keys[0]), 0, false)?;
        let sf = d.add_fn(it, named_set, &format!("set {}", keys[0]), 1, false)?;
        d.style.fns.borrow_mut().extend([(gid, i), (sid, i)]);
        for k in &keys {
            let key = d.key(it, k)?;
            d.define_accessor(it, proto, key, gf, Some(sf))?;
        }
    }
    let _ = d.style.props.set(props);

    // `CSSStyleDeclaration` and `HTMLElement.prototype.style`
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn(it, illegal_constructor, "CSSStyleDeclaration", 0, false)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    let k = d.key(it, "CSSStyleDeclaration")?;
    it.define_data(g, k, Value::Object(f), true, false, true)?;
    fixed.push(f);
    d.def_get(it, html, "style", style_attr, Some(set_style_attr))?;
    *d.style.fixed.borrow_mut() = fixed;
    Ok(())
}

// ------------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.style.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let s = d.style.s.borrow();
    if let Some(&el) = s.objs.get(&owner) {
        out.extend(d.st.borrow().by_node.get(&el).map(|&o| Value::Object(o)));
    } else if !s.by_el.is_empty()
        && let Some(&Target::Node(n)) = d.st.borrow().by_obj.get(&owner)
        && let Some(&o) = s.by_el.get(&n)
    {
        out.push(Value::Object(o));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut s = d.style.s.borrow_mut();
    let s = &mut *s;
    s.objs.retain(|&o, _| live(o));
    let objs = &s.objs;
    s.by_el.retain(|_, o| objs.contains_key(o));
}

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_style_limits(&mut self, l: StyleLimits) {
        self.dom.style.lim.set(l);
    }

    pub fn style_stats(&self) -> StyleStats {
        StyleStats { objects: self.dom.style.s.borrow().objs.len() }
    }
}

//! `HTMLElement.style` and `CSSStyleDeclaration` over the native `style` attribute (CSSOM, "CSSStyleDeclaration").
//!
//! - The declaration block stores nothing but its element. Every call reads the `style` attribute at that moment, parses
//!   it into a declaration block (`browser_style_block.rs`, where the model is described), and for a change writes the
//!   serialized block back with the normal attribute path (`attr_set`: caps, charge, native cache invalidation). So
//!   `setAttribute("style", ...)`, `cssText`, the named properties and the native selector/cascade code always agree.
//! - The parse of the last read of an element is kept (`Block`, at most `KEEP_BLOCKS` of at most `KEEP_BYTES` of text). It is valid
//!   while the attribute holds exactly the text that was parsed, so nothing has to invalidate it: a read compares the text and
//!   takes the block, or parses. A write keeps the block when the text it wrote reads back to the same list (`serialize`).
//!   The step charge of the parse is charged again on a hit, so the budget does not depend on the cache. Blink and WebKit keep
//!   the parsed set as the state and write the attribute text lazily; Servo does the same with its `PropertyDeclarationBlock`.
//!   Partial pending shorthands keep their source and surviving mask separately. Attribute spans invalidate this state.
//! - `el.style` is one object for each element while the element wrapper lives (`SameObject`): the wrapper traces the
//!   declaration and the declaration traces the wrapper. The setter forwards to `cssText` (`[PutForwards=cssText]`).
//! - Supported properties are exactly the ones `bare_css` understands: `values::Property::ALL`, `values::SHORTHANDS` and `transform` (`blk::WHOLE`).
//!   For each one the prototype has camelCase and dashed accessors (and `cssFloat`; `-webkit-` names also get the
//!   `webkitX` form), so `"x" in style` is true only for a property that really works. `transition`, `animation` and their
//!   `transition-*` / `animation-*` longhands are supported (`bare_css::motion`; the events are in `browser_motion.rs`). `WebkitAnimation`,
//!   `animation-timing-function`, `-direction`, `-fill-mode`, `-play-state` and the like are NOT supported: absent, and `setProperty` ignores them.
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
//!   still whole. Pending longhands expose `""`. Their source shorthand remains available to the cascade.
//! - Lone surrogates are `Unsupported` at every string entry (the document is UTF-8), never ignored.
//! - Indexes: the object is host-indexed (`Heap::new_indexed`, like `DOMTokenList`). `style[i]` is `item(i)`, `i` past `length`
//!   is absent, and `[Symbol.iterator]` is `Array.prototype.values` (the WebIDL value iterator of an indexed getter). The names
//!   are a cache of the attribute text and of the write count (`Names`), so no read is stale: a stale cache asks the host
//!   first (`fill`, the `Host::fill_indexed` of this module).
//! - No `getPropertyCSSValue`, `parentRule` is `null`. No SVG `style`, no `innerHTML`/`outerHTML` parse of `style`, no layout reads.
//! - Computed style is `browser_computed.rs`. It has its own read-only prototype under this one.
//! - `new CSSStyleDeclaration()` and a subclass of it throw `TypeError: Illegal constructor`.

use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_dom::attr_set;
use crate::browser_geom::illegal_s;
use crate::browser_scoped::*;
use bare_js_api::{Args, HostFn, Local, Type};
use crate::browser_style_block::{Decl, Fail, Lim, custom, get, parse, priority, remove, serialize, set};
use crate::browser_style_block as blk;
use bare_css::values::{Property, SHORTHANDS};
use bare_js::eval::Error;
use std::borrow::Cow;
use bare_html::dom::{NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, StrId, Value};
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

/// The most parsed blocks kept, and the longest text kept. Past the count the blocks are dropped all at once.
const KEEP_BLOCKS: usize = 1024;
const KEEP_BYTES: usize = 1024;

/// The parse of one `style` attribute text.
pub(crate) struct Block {
    /// The attribute text that `list` is the parse of.
    text: String,
    pub(crate) list: Vec<Decl>,
    /// The steps that the parse charged.
    spent: u64,

}

struct Pending {
    block: Option<Block>,
    span: Option<bare_html::dom::Span>,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct StyleStats {
    pub objects: usize,
    /// Kept parses of `style` attributes (`Block`), and how many reads found theirs valid.
    pub blocks: usize,
    pub hits: usize,
}

/// The longhand names of one `CSSStyleDeclaration`, for its indexes. Valid for one attribute text and one write count.
struct Names {
    text: String,
    writes: u64,
    vals: Vec<StrId>,
}

#[derive(Default)]
struct SState {
    /// `CSSStyleDeclaration` object -> the names of its indexes.
    names: IdMap<ObjId, Names>,
    /// `CSSStyleDeclaration` object -> its element.
    objs: IdMap<ObjId, NodeId>,
    /// element -> its `style` object (`SameObject`).
    by_el: HashMap<NodeId, ObjId>,
    /// element -> the parse of its `style` attribute at the last call.
    blocks: HashMap<NodeId, Block>,
    // Boxed on purpose: the field is one pointer wide, and most documents have none.
    #[allow(clippy::box_collection)]
    pending: Option<Box<HashMap<NodeId, Pending>>>,
}

#[derive(Default)]
pub(crate) struct Style {
    lim: Cell<StyleLimits>,
    pub(crate) has_pending: Cell<bool>,
    s: RefCell<SState>,
    pub(crate) proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    /// The host function id of the call in progress (set by `Dom::call`; the named accessors read it first).
    pub(crate) cur: Cell<u32>,
    /// Function id of a named getter or setter -> index in `props`.
    pub(crate) fns: RefCell<HashMap<u32, usize>>,
    /// The text buffer of a call: the probe of `setProperty` and the output of the serializer.
    buf: RefCell<String>,
    hits: Cell<usize>,
    /// Writes of a block. A pending block can change the names while the attribute text stays the same.
    writes: Cell<u64>,
    /// The supported property names, in CSS form.
    pub(crate) props: OnceCell<Vec<&'static str>>,
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
pub(crate) fn keys_of(css: &str) -> Vec<String> {
    // At most 3 names, so that no push grows the list: this runs for every property at the first style use (`ensure`).
    let mut keys = Vec::with_capacity(3);
    let camel = camel(css);
    let webkit = css.starts_with("-webkit-").then(|| format!("w{}", &camel[1..]));
    keys.push(camel);
    if css.contains('-') {
        keys.push(css.to_string());
    }
    keys.extend(webkit);
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
    if custom(name) || !name.bytes().any(|b| b.is_ascii_uppercase()) { Cow::Borrowed(name) } else { Cow::Owned(name.to_ascii_lowercase()) }
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
    fn style_parse(&self, it: &mut Interp<'_>, text: &str) -> R<(Vec<Decl>, u64)> {
        let mut spent = 0;
        let list = parse(text, self.blim(), &mut |n| { spent += n; it.charge(n) }).map_err(|f| self.style_fail(f))?;
        Ok((list, spent))
    }

    /// The block of the `style` attribute now: the kept one when the text is the same, else a parse. Give it back with
    /// `style_keep` when it still is the parse of the attribute; a block that is dropped is only parsed again.
    fn style_read(&self, it: &mut Interp<'_>, el: NodeId) -> R<Block> {
        let doc = self.doc.borrow();
        let text = doc.find_atom("style").and_then(|a| doc.attr(el, a)).unwrap_or("");
        if self.style.has_pending.get() {
            let mut s = self.style.s.borrow_mut();
            if let Some(pending) = &mut s.pending {
                let span = doc.find_atom("style").and_then(|a| doc.attr_span(el, a));
                if pending.get(&el).is_some_and(|p| p.span != span) { pending.remove(&el); }
                if let Some(b) = pending.get_mut(&el).and_then(|p| p.block.take()) {
                    it.charge(b.spent)?;
                    return Ok(b);
                }
            }
        }
        if let Some(b) = self.style.s.borrow_mut().blocks.remove(&el)
            && b.text == text
        {
            it.charge(b.spent)?;
            self.style.hits.set(self.style.hits.get() + 1);
            debug_assert!(parse(text, self.blim(), &mut |_| Ok(())).ok().is_some_and(|l| l == b.list), "kept style block is stale");
            return Ok(b);
        }
        let mut spent = 0;
        let list = parse(text, self.blim(), &mut |n| {
            spent += n;
            it.charge(n)
        })
        .map_err(|f| self.style_fail(f))?;
        Ok(Block { text: text.to_string(), list, spent })
    }

    fn style_keep(&self, el: NodeId, b: Block) {
        if self.style.has_pending.get()
            && let Some(p) = self.style.s.borrow_mut().pending.as_mut().and_then(|m| m.get_mut(&el)) {
            p.block = Some(b);
            return;
        }
        if b.text.len() > KEEP_BYTES {
            return;
        }
        let mut s = self.style.s.borrow_mut();
        if s.blocks.len() >= KEEP_BLOCKS {
            s.blocks.clear();
        }
        s.blocks.insert(el, b);
    }

    /// Serialize into the call buffer. The caller returns it with `style.buf.replace`.
    fn style_text(&self, it: &mut Interp<'_>, list: &[Decl], buf: &mut String) -> R<blk::Serialized> {
        serialize(list, self.blim(), buf, &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))
    }

    #[cold]
    fn style_check_pending(&self, it: &mut Interp<'_>, el: NodeId, list: &[Decl]) -> R<()> {
        let bytes = list.iter().try_fold(0usize, |n, d| n.checked_add(d.name.len() + d.value.len())).ok_or_else(|| self.lim("style-bytes"))?;
        if bytes > self.style_lim().max_bytes { return Err(self.lim("style-bytes")); }
        let s = self.style.s.borrow();
        if let Some(m) = &s.pending
            && m.len() >= self.style_lim().max_objects && !m.contains_key(&el) { return Err(self.lim("style-objects")); }
        it.charge(1 + list.len() as u64 + bytes as u64 / 4)
    }

    #[cold]
    fn style_keep_pending(&self, el: NodeId, block: Block) {
        let doc = self.doc.borrow();
        let span = doc.find_atom("style").and_then(|a| doc.attr_span(el, a));
        self.style.has_pending.set(true);
        self.style.s.borrow_mut().pending.get_or_insert_with(Default::default).insert(el, Pending { block: Some(block), span });
    }

    /// Write exposed text, and retain pending state independently of the parse cache.
    fn style_write(&self, it: &mut Interp<'_>, el: NodeId, mut b: Block) -> R<()> {
        self.style.writes.set(self.style.writes.get() + 1);
        let mut buf = self.style.buf.take();
        let serialized = self.style_text(it, &b.list, &mut buf)?;
        if serialized.pending { self.style_check_pending(it, el, &b.list)?; }
        if buf != b.text || serialized.pending || self.style.has_pending.get() {
            attr_set(self, it, el, "style", &buf)?;
            b.text.clear();
            b.text.push_str(&buf);
        }
        self.style.buf.replace(buf);
        if self.style.has_pending.get() { self.style.s.borrow_mut().pending.as_mut().and_then(|m| m.remove(&el)); }
        if serialized.pending { self.style_keep_pending(el, b); }
        else if serialized.exact { self.style_keep(el, b); }
        Ok(())
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
        let mut b = self.style_read(it, el)?;
        let mut probe = self.style.buf.take();
        let changed = set(&mut b.list, &name, value, important, self.blim(), &mut probe, &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))?;
        self.style.buf.replace(probe);
        if !changed {
            self.style_keep(el, b);
            return Ok(());
        }
        self.style_write(it, el, b)
    }

    /// `removeProperty`: the old value of the property (`""` if none).
    fn style_remove(&self, it: &mut Interp<'_>, el: NodeId, name: &str) -> R<String> {
        let mut b = self.style_read(it, el)?;
        let (old, changed) = remove(&mut b.list, &key(name), self.style_lim().max_bytes, &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))?;
        if changed {
            self.style_write(it, el, b)?;
        } else {
            self.style_keep(el, b);
        }
        Ok(old)
    }

    fn style_get(&self, it: &mut Interp<'_>, el: NodeId, name: &str) -> R<Option<String>> {
        let b = self.style_read(it, el)?;
        let v = get(&b.list, &key(name), self.style_lim().max_bytes, &mut |n| it.charge(n)).map_err(|f| self.style_fail(f))?;
        self.style_keep(el, b);
        Ok(v)
    }

    pub(crate) fn style_inputs(&self, it: &mut Interp<'_>, doc: &bare_html::dom::Document) -> R<HashMap<NodeId, Vec<bare_css::cascade::InputDeclaration>>> {
        let mut s = self.style.s.borrow_mut();
        let atom = doc.find_atom("style");
        let mut out = HashMap::new();
        if let Some(pending) = &mut s.pending {
            pending.retain(|&el, b| b.span == atom.and_then(|a| doc.attr_span(el, a)));
            for (&el, p) in pending.iter() {
                let Some(b) = &p.block else { continue };
                it.charge(1 + b.list.len() as u64 + b.list.iter().map(|d| d.name.len() + d.value.len()).sum::<usize>() as u64 / 4)?;
                out.insert(el, blk::inputs(&b.list));
            }
        }
        Ok(out)
    }

    pub(crate) fn style_object_inputs(&self, doc: &bare_html::dom::Document, style: ObjId) -> Option<Vec<bare_css::cascade::InputDeclaration>> {
        let s = self.style.s.borrow();
        let el = s.objs.get(&style)?;
        let b = s.pending.as_ref()?.get(el)?;
        if b.span != doc.find_atom("style").and_then(|a| doc.attr_span(*el, a)) { return None; }
        Some(blk::inputs(&b.block.as_ref()?.list))
    }

/// The element of a `CSSStyleDeclaration` object.
    fn style_el_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        if let Some(o) = obj_id(s, this)
            && let Some(&el) = self.style.s.borrow().objs.get(&o)
        {
            return Ok(el);
        }
        Err(illegal_s(s))
    }

    /// An HTML element (`style` is on `HTMLElement`, not on `Element`).
    pub(crate) fn style_html_el_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        let n = self.el_s(s, this)?;
        if self.doc.borrow().is_foreign(n) {
            return Err(illegal_s(s));
        }
        Ok(n)
    }

    /// An SVG element (`style` is also on `SVGElement`, not on `Element`).
    fn style_svg_el_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        let n = self.el_s(s, this)?;
        if self.doc.borrow().element_ns(n) != Ns::Svg {
            return Err(illegal_s(s));
        }
        Ok(n)
    }

    pub(crate) fn is_style_object(&self, o: ObjId) -> bool {
        self.style.s.borrow().objs.contains_key(&o)
    }
}

/// WebIDL `[LegacyNullToEmptyString] DOMString`: `null` is `""`.
pub(crate) fn null_empty_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, String> {
    if s.type_of(v) == Type::Null { Ok(String::new()) } else { d.string_arg_s(s, v) }
}

pub(crate) fn required_s<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, n: usize, name: &str) -> Res<'s, ()> {
    if a.len() < n {
        return Err(s.type_error(&format!(
            "Failed to execute '{name}' on 'CSSStyleDeclaration': {n} argument{} required, but only {} present.",
            if n == 1 { "" } else { "s" },
            a.len()
        )));
    }
    Ok(())
}

/// WebIDL `unsigned long`.
pub(crate) fn to_u32_s<'s>(s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, u32> {
    let n = s.to_number(v)?;
    Ok(if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as u32 } else { 0 })
}

// ------------------------------------------------------------------------------------------ JS side

/// `HTMLElement.prototype.style` and `SVGElement.prototype.style`
fn style_attr<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_html_el_s(s, this)?;
    style_of(d, s, el)
}

fn style_attr_svg<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_svg_el_s(s, this)?;
    style_of(d, s, el)
}

fn style_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId) -> Res<'s, Local<'s>> {
    let o = style_object(d, s, el)?;
    Ok(d.obj_local(s, o))
}

fn style_object<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId) -> Res<'s, ObjId> {
    let known = d.style.s.borrow().by_el.get(&el).copied();
    if let Some(o) = known {
        return Ok(o);
    }
    if d.style.s.borrow().objs.len() >= d.style_lim().max_objects {
        return Err(d.lim_s(s, "style-objects"));
    }
    if d.style.proto.get().is_none() {
        old(s, |it| ensure(d, it))?;
    }
    let o = d.new_source_s(s, crate::browser_sources::Source::Style, Some(*d.style.proto.get().expect("ensured")))?;
    let mut st = d.style.s.borrow_mut();
    st.objs.insert(o, el);
    st.by_el.insert(el, o);
    Ok(o)
}

impl Dom {
    /// The declaration of a CSSOM rule: the `style` object of a detached `span` that no script can reach, so it has no element wrapper.
    pub(crate) fn rule_declaration_s<'s>(&self, s: &mut Sc<'s, '_>, text: &str) -> Res<'s, ObjId> {
        let el = {
            let mut doc = self.doc.borrow_mut();
            self.room(&doc, 1, 0).map_err(|e| lift_err(s, e))?;
            if doc.find_atom("span").is_none() && doc.name_count() + 1 > self.lim.max_names {
                return Err(self.lim_s(s, "dom-names"));
            }
            doc.create_element("span")
        };
        let o = style_object(self, s, el)?;
        old(s, |it| self.style_set_text(it, el, text))?;
        Ok(o)
    }
}

fn set_style_attr<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_html_el_s(s, this)?;
    let v = arg_s(s, a, 0);
    let text = null_empty_s(d, s, v)?;
    old(s, |it| d.style_set_text(it, el, &text))?;
    Ok(s.undefined())
}

fn set_style_attr_svg<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_svg_el_s(s, this)?;
    let v = arg_s(s, a, 0);
    let text = null_empty_s(d, s, v)?;
    old(s, |it| d.style_set_text(it, el, &text))?;
    Ok(s.undefined())
}

impl Dom {
    /// `cssText` setter: the attribute is set even when the block is empty (the standard always sets it).
    fn style_set_text(&self, it: &mut Interp<'_>, el: NodeId, text: &str) -> R<()> {
        let (list, spent) = self.style_parse(it, text)?;
        self.style.writes.set(self.style.writes.get() + 1);
        let mut buf = self.style.buf.take();
        let serialized = self.style_text(it, &list, &mut buf)?;
        if serialized.pending { self.style_check_pending(it, el, &list)?; }
        attr_set(self, it, el, "style", &buf)?;
        if self.style.has_pending.get() { self.style.s.borrow_mut().pending.as_mut().and_then(|m| m.remove(&el)); }
        if serialized.pending {
            self.style_keep_pending(el, Block { text: buf.clone(), list, spent });
        }
        self.style.buf.replace(buf);
        Ok(())
    }
}

fn css_text<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    let mut buf = d.style.buf.take();
    let r = old(s, |it| {
        let b = d.style_read(it, el)?;
        d.style_text(it, &b.list, &mut buf)?;
        d.style_keep(el, b);
        Ok(())
    });
    let v = match r {
        Ok(()) => d.js_str_s(s, &buf),
        Err(e) => Err(e),
    };
    d.style.buf.replace(buf);
    v
}

fn set_css_text<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    let v = arg_s(s, a, 0);
    let text = null_empty_s(d, s, v)?;
    old(s, |it| d.style_set_text(it, el, &text))?;
    written(d, s, this)?;
    Ok(s.undefined())
}

/// A write through a rule declaration reaches the sheet text.
fn written<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ()> {
    let t = obj_id(s, this).map_or(Value::Undefined, Value::Object);
    old(s, |it| crate::browser_cssom::declaration_written(d, it, t))
}

fn length<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    let n = old(s, |it| {
        let b = d.style_read(it, el)?;
        let n = blk::length(&b.list);
        d.style_keep(el, b);
        Ok(n)
    })?;
    Ok(s.number(n as f64))
}

fn parent_rule<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    d.style_el_s(s, this)?;
    Ok(s.null())
}

fn item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    required_s(s, a, 1, "item")?;
    let v = arg_s(s, a, 0);
    let i = to_u32_s(s, v)? as usize;
    let name = old(s, |it| {
        let b = d.style_read(it, el)?;
        let name = blk::item(&b.list, i).unwrap_or_default();
        d.style_keep(el, b);
        Ok(name)
    })?;
    d.js_str_s(s, &name)
}

fn get_property_value<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    required_s(s, a, 1, "getPropertyValue")?;
    let v = arg_s(s, a, 0);
    let name = d.string_arg_s(s, v)?;
    let v = old(s, |it| Ok(d.style_get(it, el, &name)?.unwrap_or_default()))?;
    d.js_str_s(s, &v)
}

fn get_property_priority<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    required_s(s, a, 1, "getPropertyPriority")?;
    let v = arg_s(s, a, 0);
    let name = d.string_arg_s(s, v)?;
    let imp = old(s, |it| {
        let b = d.style_read(it, el)?;
        let imp = priority(&b.list, &key(&name));
        d.style_keep(el, b);
        Ok(imp)
    })?;
    d.js_str_s(s, if imp { "important" } else { "" })
}

fn set_property<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    required_s(s, a, 2, "setProperty")?;
    let (n, v) = (arg_s(s, a, 0), arg_s(s, a, 1));
    let name = d.string_arg_s(s, n)?;
    let value = null_empty_s(d, s, v)?;
    let prio = match a.get(2) {
        Some(p) if s.type_of(p) != Type::Undefined => null_empty_s(d, s, p)?,
        _ => String::new(),
    };
    old(s, |it| d.style_set(it, el, &name, &value, &prio))?;
    written(d, s, this)?;
    Ok(s.undefined())
}

fn remove_property<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.style_el_s(s, this)?;
    required_s(s, a, 1, "removeProperty")?;
    let v = arg_s(s, a, 0);
    let name = d.string_arg_s(s, v)?;
    let old_v = old(s, |it| d.style_remove(it, el, &name))?;
    written(d, s, this)?;
    d.js_str_s(s, &old_v)
}

/// The property of the named accessor that is running.
pub(crate) fn current<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, &'static str> {
    let i = d.style.fns.borrow().get(&(d.style.cur.get() & !CUR_NEW)).copied();
    match (i, d.style.props.get()) {
        (Some(i), Some(p)) => Ok(p[i]),
        _ => Err(illegal_s(s)),
    }
}

fn named_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let name = current(d, s)?;
    let el = d.style_el_s(s, this)?;
    let v = old(s, |it| Ok(d.style_get(it, el, name)?.unwrap_or_default()))?;
    d.js_str_s(s, &v)
}

fn named_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let name = current(d, s)?;
    let el = d.style_el_s(s, this)?;
    let v = arg_s(s, a, 0);
    let value = null_empty_s(d, s, v)?;
    old(s, |it| d.style_set(it, el, name, &value, ""))?;
    written(d, s, this)?;
    Ok(s.undefined())
}

crate::browser_lazy::lazy_global!(lazy_ctor, "CSSStyleDeclaration", ensure, |d| d.style.fixed.borrow()[1]);

pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    // The prototype, its 250 accessors and the computed prototype wait for the first style object, the first `getComputedStyle` or the
    // first read of the global (`ensure`). A page that uses none of them never pays for them.
    table!(lazy = [
        Row::both(1, "CSSStyleDeclaration", lazy_ctor),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T)?;
    table!(rows1 = [
        Row::get(1, "style", style_attr, Some(set_style_attr)),
        Row::get(2, "style", style_attr_svg, Some(set_style_attr_svg)),
    ]);
    let targets = [d.obj_local(s, d.p().html), d.obj_local(s, d.p().svg)];
    d.install_rows(s, &targets, &rows1::T)?;
    Ok(())
}

/// Make the `CSSStyleDeclaration` prototype and the computed prototype, once. `style.proto` is the flag, set last.
pub(crate) fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.style.proto.get().is_some() {
        return Ok(());
    }
    let proto = d.obj(it, it.object_prototype())?;
    d.def_get_s(it, proto, "cssText", css_text, Some(set_css_text))?;
    d.def_get_s(it, proto, "length", length, None)?;
    d.def_get_s(it, proto, "parentRule", parent_rule, None)?;
    d.def_method_s(it, proto, "item", 1, item)?;
    d.def_method_s(it, proto, "getPropertyValue", 1, get_property_value)?;
    d.def_method_s(it, proto, "getPropertyPriority", 1, get_property_priority)?;
    d.def_method_s(it, proto, "setProperty", 2, set_property)?;
    d.def_method_s(it, proto, "removeProperty", 1, remove_property)?;
    // The value iterator of an interface with an indexed getter (WebIDL "iterable"): `Array.prototype.values`.
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let values = it.get_value(Value::Object(it.array_prototype()), sym)?;
    it.define_data(proto, sym, values, true, false, true)?;

    let mut props: Vec<&'static str> = Property::LISTED.iter().map(|p| p.name()).collect();
    props.extend(SHORTHANDS);
    props.extend(blk::WHOLE);
    // Legacy aliases have their own accessor names and share the canonical property.
    props.extend(["-webkit-user-select", "word-wrap"]);
    let mut nm = String::new();
    // One scope for the 500 functions: a scope for each of them cost more than the functions.
    bare_js_api::enter(it, |s| {
        for (i, css) in props.iter().enumerate() {
            let keys = keys_of(css);
            let (gid, sid) = (d.scoped.borrow().len() as u32, d.scoped.borrow().len() as u32 + 1);
            // One buffer for both names (a `format!` for each of the 250 names cost more than the functions).
            nm.clear();
            nm.push_str("get ");
            nm.push_str(&keys[0]);
            let gf = d.add_fn_s_raw(s, named_get, &nm, 0, HostFn::Method)?;
            nm.replace_range(..3, "set");
            let sf = d.add_fn_s_raw(s, named_set, &nm, 1, HostFn::Method)?;
            d.style.fns.borrow_mut().extend([(gid, i), (sid, i)]);
            let it = s.engine();
            for k in &keys {
                let key = d.key(it, k)?;
                d.define_accessor(it, proto, key, gf, Some(sf))?;
            }
        }
        Ok(())
    })?;

    // `constructor` comes after the accessors, as in `getOwnPropertyNames`.
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn_s_obj(it, illegal_constructor, "CSSStyleDeclaration", 0, bare_js_api::HostFn::Method)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_computed::fill(d, it, proto, &props)?;
    *d.style.fixed.borrow_mut() = vec![proto, f];
    let _ = d.style.props.set(props);
    let _ = d.style.proto.set(proto);
    Ok(())
}

// ------------------------------------------------------------------------------------------ GC hooks

// ------------------------------------------------------------------------------------------ indexes

/// The names of `owner`, read by `f`, when they are fresh. `None`: `owner` is not a style object. Stale names answer
/// `NeedsHost` (index 0: `fill` makes all of them), so the evaluator fills them and repeats the read.
fn fresh<T>(d: &Dom, owner: ObjId, f: impl FnOnce(&[StrId]) -> T) -> Option<Result<T, HeapError>> {
    let Ok(s) = d.style.s.try_borrow() else { return Some(Err(HeapError::HostUnavailable)) };
    let el = *s.objs.get(&owner)?;
    let Ok(doc) = d.doc.try_borrow() else { return Some(Err(HeapError::HostUnavailable)) };
    let now = doc.find_atom("style").and_then(|a| doc.attr(el, a)).unwrap_or("");
    Some(match s.names.get(&owner) {
        Some(n) if n.text == now && n.writes == d.style.writes.get() => Ok(f(&n.vals)),
        _ => Err(HeapError::NeedsHost { owner, index: 0 }),
    })
}

/// `IndexedSource::indexed_len` for a style object. `None`: `owner` is not one.
pub(crate) fn indexed_len(d: &Dom, owner: ObjId) -> Option<Result<usize, HeapError>> {
    fresh(d, owner, |v| v.len())
}

/// `IndexedSource::indexed_item` for a style object. `None`: `owner` is not one.
pub(crate) fn indexed_item(d: &Dom, owner: ObjId, index: usize) -> Option<Result<Slot, HeapError>> {
    fresh(d, owner, |v| v.get(index).map_or(Slot::Absent, |&s| Slot::Str(s)))
}

/// `Host::fill_indexed` for a style object: make its names fresh. `Ok(false)`: `owner` is not a style object.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId) -> R<bool> {
    let el = d.style.s.borrow().objs.get(&owner).copied();
    let Some(el) = el else { return Ok(false) };
    // The read charges the parse, like `item` and `length`.
    let b = d.style_read(it, el)?;
    let mut vals = Vec::new();
    for i in 0..blk::length(&b.list) {
        let name = blk::item(&b.list, i).unwrap_or_default();
        match d.js_str(it, &name)? {
            Value::String(s) => vals.push(s),
            _ => unreachable!("js_str makes a string"),
        }
    }
    let text = d.doc.borrow().find_atom("style").and_then(|a| d.doc.borrow().attr(el, a).map(str::to_string)).unwrap_or_default();
    d.style_keep(el, b);
    let writes = d.style.writes.get();
    d.style.s.borrow_mut().names.insert(owner, Names { text, writes, vals });
    Ok(true)
}

pub(crate) fn roots(d: &Dom, out: &mut Roots<'_, '_>) {
    out.extend(d.style.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

/// The references of the style object `o` (its owner entry, `browser_sources.rs`): the wrapper of its element and its strings.
pub(crate) fn trace_obj(d: &Dom, o: ObjId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let s = d.style.s.borrow();
    if let Some(&el) = s.objs.get(&o) {
        if let Some(w) = d.st.borrow().by_node.get(&el) {
            t.visit(w);
        }
        for &v in s.names.get(&o).into_iter().flat_map(|n| &n.vals) {
            crate::browser_sources::visit(t, Value::String(v));
        }
    }
}

/// The references of the wrapper of the element `n`: its style object.
pub(crate) fn trace_node(d: &Dom, n: NodeId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let s = d.style.s.borrow();
    if !s.by_el.is_empty()
        && let Some(&o) = s.by_el.get(&n)
    {
        crate::browser_sources::visit(t, Value::Object(o));
    }
}

/// Drop the entries of the style object `o`, which the collector freed, and the kept block of its element.
pub(crate) fn prune_obj(d: &Dom, o: ObjId) {
    let mut s = d.style.s.borrow_mut();
    s.names.remove(&o);
    if let Some(el) = s.objs.remove(&o)
        && s.by_el.get(&el) == Some(&o)
    {
        s.by_el.remove(&el);
        s.blocks.remove(&el);
    }
}

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_style_limits(&mut self, l: StyleLimits) {
        self.dom.style.lim.set(l);
        self.dom.style.s.borrow_mut().blocks.clear();
    }

    pub fn style_stats(&self) -> StyleStats {
        let s = self.dom.style.s.borrow();
        StyleStats { objects: s.objs.len(), blocks: s.blocks.len(), hits: self.dom.style.hits.get() }
    }
}

// ------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The elements that `style` objects and kept blocks name.
pub(crate) fn held(d: &Dom, out: &mut Vec<NodeId>) {
    let s = d.style.s.borrow();
    out.extend(s.objs.values().copied());
    out.extend(s.by_el.keys().copied());
    out.extend(s.blocks.keys().copied());
    out.extend(s.pending.iter().flat_map(|m| m.keys().copied()));
}

/// The spans of the pending blocks: the reclaim moves them when it builds the text list again.
pub(crate) fn pins(d: &Dom, f: &mut dyn FnMut(&mut bare_html::dom::Span)) {
    for p in d.style.s.borrow_mut().pending.iter_mut().flat_map(|m| m.values_mut()) {
        if let Some(sp) = p.span.as_mut() {
            f(sp);
        }
    }
}

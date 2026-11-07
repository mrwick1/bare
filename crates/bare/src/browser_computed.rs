//! `window.getComputedStyle(element)` over the native cascade (`bare_css::cascade`), CSSOM "resolved value" rules.
//!
//! How it works
//! - A computed declaration stores its element and nothing else. Every read builds the style again from the real document:
//!   the browser sheet (`UA_CSS`), then the author sheets in document order, then `compute`. So a held object always sees
//!   the current classes, `id`, `dir`, `style` attributes, `<style>` text, `<link>` elements and viewport. No snapshot is kept
//!   and no computed value is stored as JS data. Only the parsed `RuleSet` is cached, and only while its inputs (the sheet texts, the
//!   layers and the viewport) are equal to the inputs of the last build.
//! - Author sheets: `<style>` text comes from the document. A `<link rel=stylesheet href>` has no text here: the embedder supplies it
//!   with `Browser::set_external_stylesheet(href, parts)`, keyed by the exact `href` attribute text (`@import` of an inline `<style>`:
//!   keyed by the exact url text). `parts` are the flattened sheets in cascade order, imports first (what `main.rs` `add_sheet` does).
//!   A sheet that is linked but not supplied makes every computed read `Unsupported`: a missing sheet is never read as "no rules".
//!   The exception is a driver that called `Browser::set_pending_sheets(true)` (the page loader does, once the sheets of the parsed
//!   document are in): then a sheet that is not supplied is "still loading", as in Chrome. It adds no rules, and the reads go on
//!   (Chrome: a script-inserted `<link>` is not in `styleSheets` and has no rules until it loads). A link keeps the sheet that it had
//!   until a new `href` is supplied (`Computed::bound`, set by `style_availability`). Still `Unsupported`: a link whose old sheet is
//!   gone, and a link with a `media` text (it may have just been turned on). A cached or preloaded URL is in the cache of the driver:
//!   it is supplied before the `<link>` asks. Supply an empty list for a sheet that failed to load, if the page should go on without it. `Browser::style_availability` tells which
//!   sheets are missing, so the loader knows what to fetch. A `<link>` with a `media` that does not match the viewport, an alternate
//!   sheet and a link without `href` are not sources (`cascade::sources`).
//! - Not rendered: an element inside `display:none` gets its style on demand. `RuleSet::compute_for(doc, Some(el))` styles the elements
//!   from the root down to `el` inside a hidden subtree, in the same pass, so a hidden read costs the depth of `el`. Layout and the other
//!   readers do not ask, so they never see a style there. The content of a `<template>` reads as empty, as in Chrome (it is in another
//!   document there; here it is in the tree).
//! - Order of sheets: `<style>` and `<link>` in tree order, an `@import` of an inline sheet before its text. A later sheet beats an earlier one on a tie.
//! - Not a snapshot object: `getComputedStyle(el) !== getComputedStyle(el)`. The object has its own prototype (`CSSStyleDeclaration` is
//!   its parent, so `instanceof` works). It does not share the brand of the mutable inline declaration of `browser_style.rs`: every
//!   method and accessor has a computed version, and a method of the other brand is `TypeError: Illegal invocation`.
//! - Order of work for a method: brand, argument count, argument conversion (user code may run, nothing is borrowed), then the work.
//!
//! Supported (tested)
//! - Names: every longhand of `bare_css::values::Property::ALL` and every shorthand of `SHORTHANDS`, in camelCase and dashed form (and
//!   `cssFloat`), plus custom properties through `getPropertyValue`. Anything else is not on the prototype and `getPropertyValue` gives `""`.
//! - Values are the engine's computed values as text: keywords, `<n>px`, `<n>%`, numbers, `rgb()`/`rgba()`. `getPropertyValue` of a
//!   shorthand is built from its longhands with the serializer of `browser_style_block.rs`.
//! - `length` and `item(i)`: the supported longhands in lexicographic order, followed by custom properties. A computed object of an element that is not
//!   connected is empty (CSSOM): `length` 0, `""` everywhere. Shorthands are not listed.
//! - `getPropertyPriority` is `""`. `parentRule` is `null`. `setProperty`, `removeProperty`, a `cssText` write and every named write
//!   throw `NoModificationAllowedError` after the arguments are converted.
//! - `direction` (`ltr`, `rtl`) is a native property: starts `ltr`, inherited, `inherit/initial/unset/var()` work. The HTML `dir` attribute
//!   is a rule of the browser sheet (`ua.css`), so a rule of the page or the `style` attribute beats it (`dir=auto` and `bdi` below).
//!
//! Gates (on purpose, none is silently faked; each is `Error::Unsupported`)
//! - A pseudo-element argument (`::before`, anything that starts with `:`): the engine has no pseudo-element styles.
//! - Resolved values that are used values (CSSOM "resolved value") come from layout (`browser_used.rs`): `width` and `height` of a box,
//!   `auto` and percent `margin-*`, percent `padding-*`, the offsets of a positioned box, grid track lists, a numeric `line-height`
//!   (in px). The read runs the geometry layout, so `GeometryLimits` apply (`geometry-*`, never catchable). A value that needs no
//!   layout (a px length, an element that is not rendered, a non-atomic inline box) is the computed value and costs no layout. Gated:
//!   a percent padding or margin of a grid item, an auto margin of a grid item or of an item of a wrapping flex container, and a box whose containing block is an inline box.
//! - Page-level or per-read? A gate stays a page-level `Unsupported` (fatal), not an empty string or a throw: this module and the other
//!   host modules say that a gate is never faked, and an empty string or a throw would let a script go on with a value that Chrome
//!   does not give. `docs/decisions.md` has no rule that says otherwise.
//! - A light child of a shadow host has no style (the flat tree has no slots, `RuleSet::compute_for` does not reach it).
//! - Values that the engine keeps only loosely: `align-*` and `justify-*` lose `safe`/`unsafe`, and `text-decoration` keeps only the line
//!   (the style, the color and the thickness are dropped, so `underline dotted` reads as `underline`). Those read as the text that the
//!   engine has, not a fake. A value with no text (an unresolved `var()`) and a shorthand that cannot be built from its longhands
//!   (`border` with different sides, `font`): gated.
//! - `direction` of an element whose `dir` chain reaches `dir=auto` or a `<bdi>` with no `ltr/rtl` before an explicit one: it depends on
//!   the text (not read). This is conservative: a rule of the page that sets `direction` in between does not lift it.
//! - `cssText` of a computed declaration. External sheets that are not supplied, unless the driver loads them later (see above).
//!
//! Caps (`ComputedLimits`, `Limit(Host(name))`, never catchable). All are checked before the work that they bound, because
//! `RuleSet::compute` cannot be interrupted: `computed-objects` (live objects), `computed-nodes` (document nodes at a read: the
//! engine makes one `u32` per node and up to one style (about 700 bytes) per different style, so this bounds its memory), `computed-sources`
//! (sheets, and bytes of `<style>` text, `href` and supplied parts that one read uses), `computed-rules` (selectors after the build:
//! the build is bounded by the bytes), `computed-work` (nodes times rules, a loose upper bound: the real work is far smaller because
//! rules are indexed), `computed-external` and `computed-external-bytes` (stored external sheets). Steps are charged before the
//! build and before the compute. Names that the sheets add to the document table are bounded by the sheet bytes and checked against
//! `max_names` after the build.
//!
//! Not done: `document.styleSheets` does not exist (when it does, a pending sheet must not be in it, as in Chrome), no cache of `Styles` between reads (each read costs one pass over the document), no live `getComputedStyle` of a
//! pseudo-element, no `@container`, `@import` inside a supplied part (the embedder flattens), `<link disabled>`, `<style disabled>`.

use crate::browser::*;
use crate::browser_platform::{V, arg};
use crate::browser_style::{current, keys_of};
use crate::browser_style_block::{self as blk, Fail, Lim, custom};
use crate::browser_used::{Ctx, calc_text, flow_text, grid_text};
use bare_css::cascade::{self, ComputedStyle, Origin, RuleSet, Source, Styles, UA_CSS};
use bare_css::values::{Keyword, Length, Property, SHORTHANDS, Value as Cv, shorthand_longhands};
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::{Error, Interp, LimitKind};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

/// Bounds of the computed-style module (see the header). Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct ComputedLimits {
    /// Live computed declarations. `computed-objects`.
    pub max_objects: usize,
    /// Document nodes at a read. `computed-nodes`.
    pub max_nodes: usize,
    /// Sheets of one read (the `<style>`, `<link>` and parts), and supplied parts of one external sheet. `computed-sources`.
    pub max_sources: usize,
    /// Bytes of one read: `<style>` text, `href` text and the parts. Also the bytes of one supplied sheet. `computed-sources`.
    pub max_sheet_bytes: usize,
    /// Selectors of the built rule set. `computed-rules`.
    pub max_rules: usize,
    /// Nodes times (selectors + 1). `computed-work`.
    pub max_work: u64,
    /// Supplied external sheets. `computed-external`.
    pub max_external: usize,
    /// Bytes of all supplied external sheets (the keys count too). `computed-external-bytes`.
    pub max_external_bytes: usize,
}

impl Default for ComputedLimits {
    fn default() -> Self {
        ComputedLimits {
            max_objects: 1 << 16,
            max_nodes: 1 << 17,
            max_sources: 1024,
            max_sheet_bytes: 4 << 20,
            max_rules: 1 << 18,
            max_work: 1 << 27,
            max_external: 256,
            max_external_bytes: 8 << 20,
        }
    }
}

/// One flattened piece of a supplied sheet, in cascade order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetPart {
    pub text: String,
    /// `Some("x")` or `Some("x.y")` when the piece was imported into a layer, `Some("")` for an unnamed layer, else `None`.
    pub layer: Option<String>,
}

/// Can `getComputedStyle` read now?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StyleAvailability {
    Ready,
    /// These `href` texts are linked (or imported) by the document and not supplied. At most 16 are listed.
    MissingExternal { hrefs: Vec<String>, more: bool },
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComputedStats {
    pub objects: usize,
    pub external_sheets: usize,
    pub external_bytes: usize,
    /// Rule sets built (parse of all sheets).
    pub builds: u64,
    /// Cascade passes over the document.
    pub computes: u64,
}

pub(crate) struct Built {
    vp: (f32, f32),
    pub(crate) key: Vec<(Option<String>, String)>,
    rules: RuleSet,
    /// Does a sheet use what the engine does not model (`content-visibility`, `display: contents`)? Read by `checkVisibility` only.
    pub(crate) odd: OnceCell<bool>,
}

#[derive(Default)]
pub(crate) struct Computed {
    lim: Cell<ComputedLimits>,
    objs: RefCell<IdMap<ObjId, NodeId>>,
    proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    ext: RefCell<HashMap<String, Vec<SheetPart>>>,
    ext_bytes: Cell<usize>,
    /// The driver loads a sheet that a script adds after the task (`Browser::set_pending_sheets`): until then the sheet adds no rules.
    pending_ok: Cell<bool>,
    /// A `<link>` to the `href` of the sheet it had at the last `style_availability`. It keeps those rules until the new sheet is supplied.
    bound: RefCell<HashMap<NodeId, String>>,
    /// Moves when a supplied sheet is set or removed. `ResizeObserver` reads it to know that styles may have changed.
    pub(crate) sheet_gen: Cell<u64>,
    pub(crate) cache: RefCell<Option<Built>>,
    builds: Cell<u64>,
    computes: Cell<u64>,
}

const MISSING: &str = "computed style needs an external stylesheet that the embedder has not supplied (Browser::style_availability lists them)";
const PSEUDO: &str = "getComputedStyle of a pseudo-element (the engine has no pseudo-element styles)";
const NO_STYLE: &str = "an element that the style walk does not reach has no computed style in the engine (a light child of a shadow host: no slots)";
const AUTO_DIR: &str = "direction depends on dir=auto or bdi (the text decides it)";
const CSS_TEXT: &str = "computed cssText (CSSOM and browsers differ; not faked)";
const NO_TEXT: &str = "this computed value has no text (an unresolved value)";
const NO_SHORT: &str = "this shorthand has no single computed text (sides differ, or it needs more than the engine keeps)";

struct Plan {
    parts: Vec<(Option<String>, String)>,
    bytes: usize,
    missing: Vec<String>,
    more: bool,
    /// A missing sheet that a read must not take as "no rules" (see the header).
    strict: bool,
}

/// A layer of an imported piece: `outer` is the layer of the `@import`, `inner` the layer the piece had (as `main.rs` joins them).
pub(crate) fn join_layer(outer: Option<&str>, inner: Option<&str>) -> Option<String> {
    match (outer, inner) {
        (Some(o), Some(n)) if !n.is_empty() => Some(format!("{o}.{n}")),
        (o, None | Some("")) if o.is_some() => o.map(str::to_string),
        (_, n) => n.map(str::to_string),
    }
}

/// The styles of the document at one read, and the style of the element asked about.
pub(crate) struct Snap {
    pub(crate) styles: Styles,
    pub(crate) style: ComputedStyle,
    /// The element is not rendered: it, or an ancestor, is `display:none`. Its resolved values are the computed ones.
    pub(crate) hidden: bool,
}

/// A number as Chrome prints it: 6 significant digits, no trailing zeros, no `-0`.
pub(crate) fn num(n: f32) -> String {
    let v = n as f64;
    if v == 0.0 {
        return "0".into();
    }
    // The exponent after rounding to 6 digits (`log10` would link libm, and cost startup).
    let exp: i32 = format!("{v:.5e}").rsplit_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
    let s = format!("{v:.*}", (5 - exp).clamp(0, 30) as usize);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').into() } else { s }
}

impl Snap {
    /// The computed text of a longhand, or the reason that there is none. `used` is the layout for the values that CSSOM
    /// resolves to the used value (`browser_used.rs`).
    fn text(&self, p: Property, used: Option<&Ctx>) -> Result<String, &'static str> {
        if let Some(t) = used.map(|c| c.text(self, p)).transpose()?.flatten() {
            return Ok(t);
        }
        let v = self.style.get(p);
        // A relative box with an `auto` offset moves by the opposite offset, backwards. The layout gives it when that offset is a percent.
        if !self.hidden && v == Cv::Keyword(Keyword::Auto) && self.style.get(Property::Position) == Cv::Keyword(Keyword::Relative) {
            let opposite = match p {
                Property::Top => Some(Property::Bottom),
                Property::Bottom => Some(Property::Top),
                Property::Left => Some(Property::Right),
                Property::Right => Some(Property::Left),
                _ => None,
            };
            if let Some(o) = opposite {
                return Ok(match self.style.get(o) {
                    Cv::Length(Length::Px(x)) => format!("{}px", num(-x)),
                    _ => "0px".to_string(),
                });
            }
        }
        Ok(match v {
            Cv::Keyword(k) => k.name().to_string(),
            // A number `line-height` is times the font size of the element, in pixels.
            Cv::Number(n) if p == Property::LineHeight => format!("{}px", num(n * self.style.px(Property::FontSize))),
            Cv::Number(n) if p == Property::GridAutoFlow => flow_text(n).to_string(),
            Cv::Number(n) => num(n),
            Cv::Length(Length::Px(x)) => format!("{}px", num(x)),
            Cv::Length(Length::Percent(x)) => format!("{}%", num(x)),
            Cv::Length(Length::Calc(i)) => calc_text(&self.styles, i),
            Cv::Grid(i) => grid_text(&self.styles, self.styles.grid(i)),
            Cv::Color(c) if c.a == 255 => format!("rgb({}, {}, {})", c.r, c.g, c.b),
            Cv::Color(c) => format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, num((c.a as f32 / 255.0 * 1000.0).round() / 1000.0)),
            _ => return Err(NO_TEXT),
        })
    }
}

impl Dom {
    pub(crate) fn gate(what: &'static str) -> Error {
        Error::Unsupported(unsupported(what))
    }

    /// The sheets of one read, in cascade order, and the supplied external ones that are missing.
    /// With `record`, each `<link>` is bound to the sheet that it has now (`Computed::bound`).
    fn plan(&self, doc: &mut bare_html::dom::Document, vp: (f32, f32), record: bool) -> R<Plan> {
        let l = self.computed.lim.get();
        let Some(sources) = cascade::sources_capped(doc, vp, l.max_sources, l.max_sheet_bytes) else {
            return Err(self.lim("computed-sources"));
        };
        let ext = self.computed.ext.borrow();
        let mut p = Plan { parts: Vec::new(), bytes: 0, missing: Vec::new(), more: false, strict: false };
        let (pending_ok, bound) = (self.computed.pending_ok.get(), self.computed.bound.borrow());
        // With `record`: the links of this plan, and the bindings that change.
        let (mut seen, mut changes) = (Vec::new(), Vec::new());
        fn add(d: &Dom, p: &mut Plan, layer: Option<String>, text: &str) -> R<()> {
            let l = d.computed.lim.get();
            p.bytes = p.bytes.saturating_add(text.len());
            if p.bytes > l.max_sheet_bytes || p.parts.len() >= l.max_sources {
                return Err(d.lim("computed-sources"));
            }
            p.parts.push((layer, text.to_string()));
            Ok(())
        }
        fn miss(p: &mut Plan, href: &str) {
            if p.missing.iter().any(|m| m == href) {
                return;
            }
            if p.missing.len() < 16 { p.missing.push(href.to_string()) } else { p.more = true }
        }
        for (s, el) in sources {
            match s {
                Source::Inline(text) => {
                    if text.contains("@import") {
                        for imp in cascade::imports(doc, &text, vp) {
                            match ext.get(&imp.href) {
                                Some(parts) => {
                                    for part in parts {
                                        add(self, &mut p, join_layer(imp.layer.as_deref(), part.layer.as_deref()), &part.text)?;
                                    }
                                }
                                None => {
                                    miss(&mut p, &imp.href);
                                    p.strict |= !pending_ok;
                                }
                            }
                        }
                    }
                    add(self, &mut p, None, &text)?;
                }
                Source::Link(href) if href.trim().is_empty() => {}
                Source::Link(href) => match ext.get(&href) {
                    Some(parts) => {
                        for part in parts {
                            add(self, &mut p, part.layer.clone(), &part.text)?;
                        }
                        if record {
                            seen.push(el);
                            if bound.get(&el) != Some(&href) {
                                changes.push((el, href));
                            }
                        }
                    }
                    None => {
                        miss(&mut p, &href);
                        // Chrome: the link keeps the sheet that it had until the new one has loaded. Else it has none.
                        let old = bound.get(&el);
                        match old.and_then(|k| ext.get(k)) {
                            Some(parts) => {
                                for part in parts {
                                    add(self, &mut p, part.layer.clone(), &part.text)?;
                                }
                            }
                            // The link had a sheet that is gone, or a `media` may have just turned it on (Chrome loaded it before).
                            None => p.strict |= old.is_some() || doc.find_atom("media").and_then(|m| doc.attr(el, m)).is_some_and(|m| !m.trim().is_empty()),
                        }
                        p.strict |= !pending_ok;
                        if record && old.is_some() {
                            seen.push(el);
                        }
                    }
                },
            }
        }
        drop(bound);
        if record {
            let mut bound = self.computed.bound.borrow_mut();
            bound.extend(changes);
            // A link that is not a source any more loses its binding. Rare: only when there are more bindings than links.
            if bound.len() > seen.len() {
                let keep: std::collections::HashSet<NodeId> = seen.into_iter().collect();
                bound.retain(|n, _| keep.contains(n));
            }
        }
        Ok(p)
    }

    /// Is `el` in the document tree? `false`: it is not connected (a detached tree, or a shadow tree that is not attached). A connected shadow tree is a gate.
    pub(crate) fn in_document(&self, it: &mut Interp<'_>, el: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        let mut top = el;
        while let Some(p) = doc.parent(top) {
            it.charge(1)?;
            top = p;
        }
        it.charge(1)?;
        if top != doc.root() {
            if matches!(doc.data(top), NodeData::ShadowRoot) {
                let mut including = top;
                while let Some(p) = doc.parent_or_host(including) {
                    it.charge(1)?;
                    including = p;
                }
                if including == doc.root() {
                    return Err(Self::gate("computed style in a shadow tree (shadow cascade is not implemented)"));
                }
            }
            return Ok(false);
        }
        Ok(true)
    }

    /// The styles of the whole document now (the cascade of one read). Charged and capped as described in the header.
    pub(crate) fn cascade_now(&self, it: &mut Interp<'_>) -> R<Styles> {
        self.cascade_for(it, None)
    }

    /// The same, and `want` is styled even inside `display:none` (`RuleSet::compute_for`).
    fn cascade_for(&self, it: &mut Interp<'_>, want: Option<NodeId>) -> R<Styles> {
        let l = self.computed.lim.get();
        let vp = self.plat.s.borrow().viewport;
        if self.doc.borrow().len() > l.max_nodes {
            return Err(self.lim("computed-nodes"));
        }
        let mut doc = self.doc.borrow_mut();
        it.charge(doc.len() as u64 / 4 + 1)?;
        let plan = self.plan(&mut doc, vp, false)?;
        if plan.strict {
            return Err(Self::gate(MISSING));
        }
        it.charge(plan.bytes as u64 / 32)?;
        {
            let mut cache = self.computed.cache.borrow_mut();
            if !cache.as_ref().is_some_and(|b| b.vp == vp && b.key == plan.parts) {
                *cache = None;
                let mut rules = RuleSet::new(vp);
                rules.add(&mut doc, UA_CSS, Origin::UserAgent);
                for (layer, text) in &plan.parts {
                    rules.add_in_layer(&mut doc, text, Origin::Author, layer.as_deref());
                }
                self.computed.builds.set(self.computed.builds.get() + 1);
                if rules.len() > l.max_rules {
                    return Err(self.lim("computed-rules"));
                }
                if doc.name_count() > self.lim.max_names {
                    return Err(self.lim("dom-names"));
                }
                *cache = Some(Built { vp, key: plan.parts, rules, odd: OnceCell::new() });
            }
        }
        let cache = self.computed.cache.borrow();
        let rules = &cache.as_ref().expect("built").rules;
        let work = (doc.len() as u64).saturating_mul(rules.len() as u64 + 1);
        if work > l.max_work {
            return Err(self.lim("computed-work"));
        }
        it.charge(work)?;
        drop(doc);
        let doc = self.doc.borrow();
        self.computed.computes.set(self.computed.computes.get() + 1);
        Ok(rules.compute_for(&doc, want))
    }

    /// The style of `el` now, or `None` when the element is not connected (an empty declaration).
    pub(crate) fn snapshot(&self, it: &mut Interp<'_>, el: NodeId) -> R<Option<Snap>> {
        if !self.in_document(it, el)? {
            return Ok(None);
        }
        let mut styles = self.cascade_for(it, None)?;
        if styles.style(el).is_none() {
            // Not rendered: the second walk also styles the way down to `el`. A rendered element never pays for it.
            styles = self.cascade_for(it, Some(el))?;
        }
        let Some(style) = styles.style(el).cloned() else { return Err(Self::gate(NO_STYLE)) };
        // Is it rendered? Is it in the content of a `<template>`? Chrome keeps that content in another document, so it reads as empty.
        let doc = self.doc.borrow();
        let (mut hidden, mut n) = (style.get(Property::Display) == Cv::Keyword(Keyword::None), 0u64);
        let template = doc.find_atom("template");
        let mut up = doc.flat_parent(el);
        while let Some(a) = up {
            n += 1;
            if let NodeData::Element { name, .. } = *doc.data(a) {
                if Some(name) == template && !doc.is_foreign(a) {
                    drop(doc);
                    it.charge(n / 8 + 1)?;
                    return Ok(None);
                }
                hidden |= styles.style(a).is_some_and(|s| s.get(Property::Display) == Cv::Keyword(Keyword::None));
            }
            up = doc.flat_parent(a);
        }
        drop(doc);
        it.charge(n / 8 + 1)?;
        Ok(Some(Snap { styles, style, hidden }))
    }

    fn connected(&self, it: &mut Interp<'_>, el: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        let (mut top, mut n) = (el, 0u64);
        while let Some(p) = doc.parent(top) {
            top = p;
            n += 1;
        }
        it.charge(n / 8 + 1)?;
        Ok(top == doc.root())
    }

    /// Does the `direction` of `el` come from `dir=auto` or `<bdi>` (the text decides)? See the header.
    fn direction_needs_text(&self, it: &mut Interp<'_>, el: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        let (dir, bdi) = (doc.find_atom("dir"), doc.find_atom("bdi"));
        let (mut cur, mut n) = (Some(el), 0u64);
        let mut auto = false;
        while let Some(e) = cur {
            n += 1;
            if let NodeData::Element { name, .. } = *doc.data(e) {
                match dir.and_then(|a| doc.attr(e, a)) {
                    Some(v) if v.eq_ignore_ascii_case("ltr") || v.eq_ignore_ascii_case("rtl") => break,
                    Some(v) if v.eq_ignore_ascii_case("auto") => {
                        auto = true;
                        break;
                    }
                    _ if Some(name) == bdi && !doc.is_foreign(e) => {
                        auto = true;
                        break;
                    }
                    _ => {}
                }
            }
            cur = doc.parent(e);
        }
        drop(doc);
        it.charge(n / 8 + 1)?;
        Ok(auto)
    }

    /// `getPropertyValue(name)` of the computed declaration of `el`.
    fn computed_value(&self, it: &mut Interp<'_>, el: NodeId, name: &str) -> R<String> {
        let is_custom = custom(name);
        let key = if is_custom { name.to_string() } else { name.to_ascii_lowercase() };
        let long = Property::from_name(&key);
        if !is_custom && long.is_none() && !SHORTHANDS.contains(&key.as_str()) {
            return Ok(String::new());
        }
        if long == Some(Property::Direction) && self.connected(it, el)? && self.direction_needs_text(it, el)? {
            return Err(Self::gate(AUTO_DIR));
        }
        let Some(s) = self.snapshot(it, el)? else { return Ok(String::new()) };
        if is_custom {
            return Ok(s.styles.custom_properties(&s.style).iter().find(|(n, _)| **n == *key).map(|(_, t)| t.to_string()).unwrap_or_default());
        }
        let longs = long.as_ref().map(std::slice::from_ref).or_else(|| shorthand_longhands(&key)).expect("a longhand or a shorthand");
        let used = self.used_for(it, &s, el, longs)?;
        let doc = self.doc.borrow();
        let ctx = used.as_ref().map(|u| Ctx::new(&doc, &s.styles, u, el));
        // The engine keeps only the line of `text-decoration`; Chrome writes the shorthand as that line when the style and the color are the defaults.
        if long.is_some() || key == "text-decoration" {
            return s.text(longs[0], ctx.as_ref()).map_err(Self::gate);
        }
        if key == "grid-row" || key == "grid-column" {
            let (a, b) = (s.text(longs[0], ctx.as_ref()).map_err(Self::gate)?, s.text(longs[1], ctx.as_ref()).map_err(Self::gate)?);
            // `start / end`: an `auto` end is left out, and so is an end that repeats a name.
            let name = a.starts_with(|c: char| c.is_alphabetic() || c == '_') && !a.starts_with("span ") && a != "auto";
            return Ok(if b == "auto" || (name && a == b) { a } else { format!("{a} / {b}") });
        }
        let mut text = String::new();
        for &p in longs {
            text.push_str(&format!("{}:{};", p.name(), s.text(p, ctx.as_ref()).map_err(Self::gate)?));
        }
        drop(doc);
        let lim = Lim { bytes: 64 << 10, decls: 256 };
        let fail = |f: Fail| match f {
            Fail::Err(e) => e,
            Fail::Unsupported(w) => Self::gate(w),
            Fail::Bytes | Fail::Decls => self.lim("computed-shorthand"),
        };
        let list = blk::parse(&text, lim, &mut |n| it.charge(n)).map_err(fail)?;
        match blk::get(&list, &key, lim.bytes, &mut |n| it.charge(n)).map_err(fail)? {
            Some(t) if !t.is_empty() => Ok(t),
            _ => Err(Self::gate(NO_SHORT)),
        }
    }

    fn computed_el(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        if let Value::Object(o) = this
            && let Some(&el) = self.computed.objs.borrow().get(&o)
        {
            return Ok(el);
        }
        Err(it.type_error("Illegal invocation"))
    }

    fn read_only(&self, it: &mut Interp<'_>, what: &str) -> Error {
        self.dom_err(it, "NoModificationAllowedError", &format!("Failed to execute '{what}' on 'CSSStyleDeclaration': these styles are computed, and therefore read-only."))
    }
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

/// WebIDL `[LegacyNullToEmptyString] DOMString`.
fn null_empty(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<String> {
    if matches!(v, Value::Null) { Ok(String::new()) } else { d.string_arg(it, v) }
}

// ------------------------------------------------------------------------------------------ JS side

/// `window.getComputedStyle(element, pseudoElt)`
fn get_computed_style(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'getComputedStyle' on 'Window': 1 argument required, but only 0 present."));
    }
    let el = match d.node_of(a[0]) {
        Some(n) if d.is_element(n) => n,
        _ => return Err(it.type_error("Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.")),
    };
    let pseudo = match arg(a, 1) {
        Value::Undefined | Value::Null => String::new(),
        v => d.string_arg(it, v)?,
    };
    if pseudo.starts_with(':') {
        return Err(Dom::gate(PSEUDO));
    }
    if d.computed.objs.borrow().len() >= d.computed.lim.get().max_objects {
        return Err(d.lim("computed-objects"));
    }
    d.wrap(it, el)?;
    let o = d.obj(it, *d.computed.proto.get().expect("installed"))?;
    d.computed.objs.borrow_mut().insert(o, el);
    Ok(Value::Object(o))
}

fn get_property_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.computed_el(it, this)?;
    required(it, a, 1, "getPropertyValue")?;
    let name = d.string_arg(it, a[0])?;
    let v = d.computed_value(it, el, &name)?;
    d.js_str(it, &v)
}

fn get_property_priority(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.computed_el(it, this)?;
    required(it, a, 1, "getPropertyPriority")?;
    d.string_arg(it, a[0])?;
    d.js_str(it, "")
}

fn set_property(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.computed_el(it, this)?;
    required(it, a, 2, "setProperty")?;
    d.string_arg(it, a[0])?;
    null_empty(d, it, a[1])?;
    if let Some(&v) = a.get(2) && !matches!(v, Value::Undefined) {
        null_empty(d, it, v)?;
    }
    Err(d.read_only(it, "setProperty"))
}

fn remove_property(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.computed_el(it, this)?;
    required(it, a, 1, "removeProperty")?;
    d.string_arg(it, a[0])?;
    Err(d.read_only(it, "removeProperty"))
}

fn css_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.computed_el(it, this)?;
    Err(Dom::gate(CSS_TEXT))
}

fn set_css_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.computed_el(it, this)?;
    null_empty(d, it, arg(a, 0))?;
    Err(d.read_only(it, "cssText"))
}

fn names(s: &Snap) -> Vec<&str> {
    let mut names = Property::ALL.iter().map(|p| p.name()).collect::<Vec<_>>();
    names.sort_unstable();
    names.extend(s.styles.custom_properties(&s.style).iter().map(|(name, _)| &**name));
    names
}

fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.computed_el(it, this)?;
    let n = d.snapshot(it, el)?.map_or(0, |s| Property::COUNT + s.styles.custom_properties(&s.style).len());
    Ok(Value::Number(n as f64))
}

fn parent_rule(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.computed_el(it, this)?;
    Ok(Value::Null)
}

fn item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.computed_el(it, this)?;
    required(it, a, 1, "item")?;
    let i = crate::browser_platform_class::to_u32(it, a[0])? as usize;
    let Some(s) = d.snapshot(it, el)? else { return d.js_str(it, "") };
    it.charge((Property::COUNT + s.styles.custom_properties(&s.style).len()) as u64)?;
    d.js_str(it, names(&s).get(i).copied().unwrap_or(""))
}

fn named_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let name = current(d, it)?;
    let el = d.computed_el(it, this)?;
    let v = d.computed_value(it, el, name)?;
    d.js_str(it, &v)
}

fn named_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let name = current(d, it)?;
    d.computed_el(it, this)?;
    null_empty(d, it, arg(a, 0))?;
    Err(d.read_only(it, name))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let proto = d.obj(it, *d.style.proto.get().expect("style installed first"))?;
    let _ = d.computed.proto.set(proto);
    d.def_get(it, proto, "cssText", css_text, Some(set_css_text))?;
    d.def_get(it, proto, "length", length, None)?;
    d.def_get(it, proto, "parentRule", parent_rule, None)?;
    d.def_method(it, proto, "item", 1, item)?;
    d.def_method(it, proto, "getPropertyValue", 1, get_property_value)?;
    d.def_method(it, proto, "getPropertyPriority", 1, get_property_priority)?;
    d.def_method(it, proto, "setProperty", 2, set_property)?;
    d.def_method(it, proto, "removeProperty", 1, remove_property)?;
    let props = d.style.props.get().expect("style installed first");
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
    let f = d.def_method(it, g, "getComputedStyle", 1, get_computed_style)?;
    // Window operations are enumerable.
    let k = d.key(it, "getComputedStyle")?;
    it.define_data(g, k, Value::Object(f), true, true, true)?;
    *d.computed.fixed.borrow_mut() = vec![proto, f];
    Ok(())
}

// ------------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.computed.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

/// A computed declaration keeps its element's wrapper alive (and so the element's tree, attached or not).
pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    if let Some(&el) = d.computed.objs.borrow().get(&owner) {
        out.extend(d.st.borrow().by_node.get(&el).map(|&o| Value::Object(o)));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    d.computed.objs.borrow_mut().retain(|&o, _| live(o));
}

// ------------------------------------------------------------------------------------------ Browser API

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_computed_limits(&mut self, l: ComputedLimits) {
        self.dom.computed.lim.set(l);
    }

    pub fn computed_stats(&self) -> ComputedStats {
        let c = &self.dom.computed;
        ComputedStats {
            objects: c.objs.borrow().len(),
            external_sheets: c.ext.borrow().len(),
            external_bytes: c.ext_bytes.get(),
            builds: c.builds.get(),
            computes: c.computes.get(),
        }
    }

    /// Supply the text of an external sheet (see the header): `href` is the exact `<link href>` (or `@import`) text, `parts` the
    /// flattened pieces in cascade order, imports first. It replaces an older entry of the same `href`. An empty list means "loaded,
    /// no rules" (use it for a sheet that failed, when the page should go on). Bounded: `computed-external`,
    /// `computed-external-bytes`, `computed-sources`. Nothing changes on an error.
    pub fn set_external_stylesheet(&mut self, href: &str, parts: Vec<SheetPart>) -> R<()> {
        let c = &self.dom.computed;
        let l = c.lim.get();
        let bytes = href.len() + parts.iter().map(|p| p.text.len() + p.layer.as_ref().map_or(0, |s| s.len())).sum::<usize>();
        if parts.len() > l.max_sources || bytes > l.max_sheet_bytes {
            return Err(Error::Limit(LimitKind::Host("computed-sources")));
        }
        let mut ext = c.ext.borrow_mut();
        let old = ext.get(href).map(|ps| href.len() + ps.iter().map(|p| p.text.len() + p.layer.as_ref().map_or(0, |s| s.len())).sum::<usize>());
        if old.is_none() && ext.len() >= l.max_external {
            return Err(Error::Limit(LimitKind::Host("computed-external")));
        }
        let total = c.ext_bytes.get() - old.unwrap_or(0) + bytes;
        if total > l.max_external_bytes {
            return Err(Error::Limit(LimitKind::Host("computed-external-bytes")));
        }
        ext.insert(href.to_string(), parts);
        c.ext_bytes.set(total);
        c.sheet_gen.set(c.sheet_gen.get() + 1);
        Ok(())
    }

    /// Say that the driver loads a sheet that a script adds, after the task that added it, and tells the page with `load` or
    /// `error`. From then on a `<link>` or `@import` that is not supplied is a sheet that is still loading, as in Chrome: it adds no rules
    /// and a read does not stop. Call it when every sheet of the parsed document is supplied. Before it (and for an embedder that
    /// never calls it) a missing sheet is `Unsupported`.
    pub fn set_pending_sheets(&mut self, on: bool) {
        self.dom.computed.pending_ok.set(on);
    }

    /// Forget a supplied sheet: a `<link>` to it is missing again. False when there was none.
    pub fn remove_external_stylesheet(&mut self, href: &str) -> bool {
        let c = &self.dom.computed;
        let mut ext = c.ext.borrow_mut();
        match ext.remove(href) {
            Some(ps) => {
                let n = href.len() + ps.iter().map(|p| p.text.len() + p.layer.as_ref().map_or(0, |s| s.len())).sum::<usize>();
                c.ext_bytes.set(c.ext_bytes.get() - n);
                c.sheet_gen.set(c.sheet_gen.get() + 1);
                true
            }
            None => false,
        }
    }

    /// Whether `getComputedStyle` can read now, or which external sheets are missing. Reads the document, runs no JS, charges no steps.
    /// Source caps are errors. The cascade node cap applies to computed reads, not sheet discovery.
    pub fn style_availability(&self) -> R<StyleAvailability> {
        let d = &self.dom;
        let vp = d.plat.s.borrow().viewport;
        let mut doc = d.doc.borrow_mut();
        let p = d.plan(&mut doc, vp, true)?;
        Ok(if p.missing.is_empty() { StyleAvailability::Ready } else { StyleAvailability::MissingExternal { hrefs: p.missing, more: p.more } })
    }
}

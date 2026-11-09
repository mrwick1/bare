//! `window.getComputedStyle(element)` over the native cascade (`bare_css::cascade`), CSSOM "resolved value" rules.
//!
//! How it works
//! - A computed declaration stores its element and nothing else. Every read builds the style again from the real document:
//!   the browser sheet (`UA_CSS`), then the author sheets in document order, then `compute`. So a held object always sees
//!   the current classes, `id`, `dir`, `style` attributes, `<style>` text, `<link>` elements and viewport. No snapshot is kept
//!   and no computed value is stored as JS data. The parsed `RuleSet` is cached while its inputs (the sheet texts, the layers and
//!   the viewport) are equal to the inputs of the last build. The last cascade of that `RuleSet` is cached too, while the document
//!   counters (`revision`, `text_revision`, length, user state revisions) are the same and no element has custom element state
//!   (see `cascade_for`).
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
//!   sheet, a link with `disabled` and a link without `href` are not sources (`cascade::sources`).
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
//! - Resolved values that are used values (CSSOM "resolved value") come from layout (`browser_used.rs`): `width` and `height` of a box,
//!   `auto` and percent `margin-*`, percent `padding-*`, the offsets of a positioned box, grid track lists, a numeric `line-height`
//!   (in px). The read runs the geometry layout, so `GeometryLimits` apply (`geometry-*`, never catchable). A value that needs no
//!   layout (a px length, an element that is not rendered, a non-atomic inline box) is the computed value and costs no layout. Gated:
//!   a percent padding or margin of a grid item, an auto margin of a grid item or of an item of a wrapping flex container, and a box whose containing block is an inline box.
//! - Page-level or per-read? A gate stays a page-level `Unsupported` (fatal), not an empty string or a throw: this module and the other
//!   host modules say that a gate is never faked, and an empty string or a throw would let a script go on with a value that Chrome
//!   does not give. `docs/decisions.md` has no rule that says otherwise.
//! - A light child of a shadow host that no slot takes has no style (it is not rendered; `RuleSet::compute_for` does not reach it).
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
//! the build is bounded by the bytes), `computed-work` (nodes times rules, or if that is over `STYLED_FROM` the styled elements times rules: a loose upper
//! bound, the real work is far smaller because rules are indexed; see `styled_work`), `computed-external` and `computed-external-bytes` (stored external sheets). Steps are charged before the
//! build and before a full compute (`work / UNITS_PER_STEP`); a restyle charges the elements that it styles; a read that finds the last styles right charges 1. Names that the sheets add to the document table are bounded by the sheet bytes and checked against
//! `max_names` after the build.
//!
//! Pseudo-elements (`getComputedStyle(el, '::before')`): the engine has no pseudo-element cascade. A rule with a pseudo-element in a selector
//! is dropped when the sheet is built (`cascade.rs`), and `content` is not a property. So a pseudo-element reads as one that no rule matches:
//! the inherited properties (`bare_css::cascade::inherited`, and custom properties) are the element's, every other property is initial
//! (`display` is `inline`, `width` is `auto`), and `content` is `normal`. Wrong in Chrome: a `::before { content: "x"; color: blue }` rule
//! (Chrome gives `"x"` and `blue`). It throws nothing. Valid names: `::before`, `::after`, `::first-line`, `::first-letter` (and the old one-colon
//! forms), `::marker`, `::placeholder`, `::selection`, `::backdrop`, `::file-selector-button`, `::-webkit-*`. Another string that starts with
//! `:` is an empty declaration (CSSOM). The values are read live. No layout runs for a pseudo-element.
//!
//! Not done: `document.styleSheets` does not exist (when it does, a pending sheet must not be in it, as in Chrome), no cache of `Styles` between reads (each read costs one pass over the document), no rule-based `getComputedStyle` of a
//! pseudo-element (see above), no `@container`, `@import` inside a supplied part (the embedder flattens), `<style disabled>`.

use crate::browser::*;
use crate::browser_platform::{V, arg};
use crate::browser_style::{current, keys_of};
use crate::browser_style_block::{self as blk, Fail, Lim, custom};
use crate::browser_used::{Ctx, calc_of, calc_text, flow_text, grid_text};
use crate::browser_ui::chains;
use bare_css::cascade::{self, ComputedStyle, Origin, RuleSet, Scopes, Source, Styles, UA_CSS, inherited};
use bare_css::values::{Keyword, Length, Property, SHORTHANDS, Value as Cv, shorthand_longhands};
use bare_html::dom::{DAMAGE_STYLE, Mark, NodeData, NodeId};
use bare_js::eval::{Error, Interp, LimitKind};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
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
    /// Nodes times (selectors + 1), or the styled elements times (selectors + 1) when the first is over. `computed-work`.
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
    /// Reads that restyled only the elements that the changes reach (`RuleSet::restyle`), and hidden reads that styled only the
    /// way down to the element (`RuleSet::style_hidden`). They are not in `computes`.
    pub restyles: u64,
}

/// The key of `Built::last`.
type LastKey = (u64, u64, usize, [u64; 3], Option<NodeId>);

pub(crate) struct Built {
    vp: (f32, f32),
    pub(crate) key: Vec<(NodeId, Option<String>, String)>,
    rules: RuleSet,
    /// The last cascade of these rules, and its key: `revision`, `text_revision`, length and user state revisions of the
    /// document, and `want`. The flag tells that it keeps what `restyle_frame` needs (`RuleSet::compute_kept`).
    /// Two places: `[0]` for no `want`, `[1]` for a `want`. A read of a hidden element asks for both (`snapshot`), and one
    /// place made each such read throw away the other cascade: a poll of a hidden element did two full cascades for each read.
    last: [Option<(LastKey, bool, Rc<Styles>)>; 2],
    /// `Some` when `last[0]` is the cascade of the reads: the document logs its changes for it (`DAMAGE_STYLE`), and the next read
    /// restyles only what they reach. The user state chains (`chains`) of that cascade.
    read: Option<[Vec<NodeId>; 3]>,
    /// Does a sheet use what the engine does not model (`content-visibility`, `display: contents`)? Read by `checkVisibility` only.
    pub(crate) odd: OnceCell<bool>,
    /// The inputs of the plan that `key` came from (`revision`, `text_revision`, length, `sheet_gen`). Equal inputs give the same plan, so the
    /// read skips the walk over the sources, the copy of the sheet text and the comparison with `key`.
    stamp: (u64, u64, usize, u64),
}

#[derive(Default)]
pub(crate) struct Computed {
    lim: Cell<ComputedLimits>,
    objs: RefCell<IdMap<ObjId, NodeId>>,
    /// The declarations that are not of the element itself: `PSEUDO` (a pseudo-element) or `EMPTY` (a colon string that is no pseudo-element).
    kinds: RefCell<IdMap<ObjId, u8>>,
    proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    pub(crate) ext: RefCell<HashMap<String, Vec<SheetPart>>>,
    ext_bytes: Cell<usize>,
    /// The driver loads a sheet that a script adds after the task (`Browser::set_pending_sheets`): until then the sheet adds no rules.
    pending_ok: Cell<bool>,
    /// A `<link>` to the `href` of the sheet it had at the last `style_availability`. It keeps those rules until the new sheet is supplied.
    bound: RefCell<HashMap<NodeId, String>>,
    /// Moves when a supplied sheet is set or removed. `ResizeObserver` reads it to know that styles may have changed.
    pub(crate) sheet_gen: Cell<u64>,
    pub(crate) cache: RefCell<Option<Built>>,
    /// Styled elements of the last tree that `styled_work` counted, and its key (`revision`, length).
    elements: Cell<Option<((u64, usize), u64)>>,
    builds: Cell<u64>,
    computes: Cell<u64>,
    restyles: Cell<u64>,
}

const MISSING: &str = "computed style needs an external stylesheet that the embedder has not supplied (Browser::style_availability lists them)";
/// Is `e` the gate for a linked sheet that the embedder has not supplied (yet)?
pub(crate) fn is_missing_sheet(e: &Error) -> bool {
    matches!(e, Error::Unsupported(u) if u.what == MISSING)
}
/// The kind of a computed declaration that is not of the element: a pseudo-element, or an empty declaration (CSSOM: a string that starts
/// with a colon and is no pseudo-element).
/// Above this bound (nodes x (rules + 1)) a full compute is charged by its styled elements, which costs one walk of the tree.
const STYLED_FROM: u64 = 1 << 20;
/// Elements x rules that one step stands for (the cap counts them all). Measured: see `docs/decisions.md`.
const UNITS_PER_STEP: u64 = 4;
const PSEUDO: u8 = 1;
const EMPTY: u8 = 2;

/// Is `s` (`pseudoElt`, starts with `:`) a pseudo-element selector? One colon is for the four old names only.
fn pseudo_kind(s: &str) -> u8 {
    let l = s.to_ascii_lowercase();
    let (name, double) = match l.strip_prefix("::") {
        Some(n) => (n, true),
        None => (&l[1..], false),
    };
    let old = matches!(name, "before" | "after" | "first-line" | "first-letter");
    let new = matches!(name, "marker" | "placeholder" | "selection" | "backdrop" | "file-selector-button")
        || name.strip_prefix("-webkit-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    if old || (double && new) { PSEUDO } else { EMPTY }
}
const NO_STYLE: &str = "an element that the style walk does not reach has no computed style in the engine (a light child of a shadow host that no slot takes)";
const AUTO_DIR: &str = "direction depends on dir=auto or bdi (the text decides it)";
const CSS_TEXT: &str = "computed cssText (CSSOM and browsers differ; not faked)";
const NO_TEXT: &str = "this computed value has no text (an unresolved value)";
const NO_SHORT: &str = "this shorthand has no single computed text (sides differ, or it needs more than the engine keeps)";

struct Plan {
    /// The tree of the sheet (the document node, or a shadow root), its layer and its text.
    parts: Vec<(NodeId, Option<String>, String)>,
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
    pub(crate) styles: Rc<Styles>,
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
        let v = self.style.value(p);
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
            // Chrome gives `normal` for `letter-spacing` and `0px` for `word-spacing`.
            Cv::Keyword(Keyword::Normal) if p == Property::WordSpacing => "0px".to_string(),
            Cv::Keyword(k) => k.name().to_string(),
            // A number `line-height` is times the font size of the element, in pixels.
            Cv::Number(n) if p == Property::LineHeight => format!("{}px", num(n * self.style.px(Property::FontSize))),
            Cv::Number(n) if p == Property::GridAutoFlow => flow_text(n).to_string(),
            Cv::Number(n) if matches!(p, Property::BackgroundImage | Property::MaskImage) => bare_css::urls::get(n as u32).map_or("none".to_string(), |u| format!("url(\"{u}\")")),
            Cv::Number(n) => num(n),
            Cv::Length(Length::Px(x)) => format!("{}px", num(x)),
            Cv::Length(Length::Percent(x)) => format!("{}%", num(x)),
            Cv::Length(Length::Calc(i)) if p.is_picture() => calc_of(bare_css::cascade::picture_calc(i)),
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
        let sources = cascade::sources_capped(doc, vp, l.max_sources, l.max_sheet_bytes, doc.has_shadow_roots());
        let ext = self.computed.ext.borrow();
        let mut p = Plan { parts: Vec::new(), bytes: 0, missing: Vec::new(), more: false, strict: false };
        let (pending_ok, bound) = (self.computed.pending_ok.get(), self.computed.bound.borrow());
        // With `record`: the links of this plan, and the bindings that change.
        let (mut seen, mut changes) = (Vec::new(), Vec::new());
        fn add(d: &Dom, p: &mut Plan, scope: NodeId, layer: Option<String>, text: &str) -> R<()> {
            let l = d.computed.lim.get();
            if text.len() > l.max_sheet_bytes.saturating_sub(p.bytes) || p.parts.len() >= l.max_sources {
                return Ok(());
            }
            p.bytes += text.len();
            p.parts.push((scope, layer, text.to_string()));
            Ok(())
        }
        fn miss(p: &mut Plan, href: &str) {
            if p.missing.iter().any(|m| m == href) {
                return;
            }
            if p.missing.len() < 16 { p.missing.push(href.to_string()) } else { p.more = true }
        }
        let (shadows, root) = (doc.has_shadow_roots(), doc.root());
        for (s, el) in sources {
            let scope = if shadows { doc.tree_root(el) } else { root };
            match s {
                Source::Inline(text) => {
                    if text.contains("@import") {
                        for imp in cascade::imports(doc, &text, vp) {
                            match ext.get(&imp.href) {
                                Some(parts) => {
                                    for part in parts {
                                        add(self, &mut p, scope, join_layer(imp.layer.as_deref(), part.layer.as_deref()), &part.text)?;
                                    }
                                }
                                None => {
                                    miss(&mut p, &imp.href);
                                    p.strict |= !pending_ok;
                                }
                            }
                        }
                    }
                    add(self, &mut p, scope, None, &text)?;
                }
                Source::Link(href) if href.trim().is_empty() => {}
                Source::Link(href) => match ext.get(&href) {
                    Some(parts) => {
                        for part in parts {
                            add(self, &mut p, scope, part.layer.clone(), &part.text)?;
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
                                    add(self, &mut p, scope, part.layer.clone(), &part.text)?;
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

    /// Is `el` in the document tree? `false`: it is not connected (a detached tree, or a shadow tree that is not attached). A shadow tree of a connected host is connected.
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
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Ok(true)
    }

    /// The user action state (`bare_css::selector::USES_*`) that the rules of the last cascade read. All of it before the first cascade.
    pub(crate) fn user_rules(&self) -> u8 {
        self.computed.cache.borrow().as_ref().map_or(u8::MAX, |b| b.rules.user_flags())
    }

    /// The styles of the whole document now (the cascade of one read). Charged and capped as described in the header.
    pub(crate) fn cascade_now(&self, it: &mut Interp<'_>) -> R<Rc<Styles>> {
        self.cascade_for(it, None)
    }

    /// `cascade_now` for a window frame: the styles keep what `restyle_frame` needs (`RuleSet::compute_kept`).
    pub(crate) fn cascade_frame(&self, it: &mut Interp<'_>) -> R<Rc<Styles>> {
        self.cascade_with(it, None, true)
    }

    /// The work bound of a page that the cheap bound (nodes x (rules + 1)) refuses: only an element is styled, so the text and the
    /// comments do not count. It walks the tree like the cascade does (the shadow tree of a host and its light children, which
    /// is more than the cascade reaches). The count is kept while the tree does not change.
    #[cold]
    fn styled_work(&self, doc: &bare_html::dom::Document, rules: usize) -> u64 {
        let key = (doc.revision(), doc.len());
        let elements = match self.computed.elements.get() {
            Some((k, n)) if k == key => n,
            _ => {
                let n = elements_under(doc, doc.root(), u64::MAX);
                self.computed.elements.set(Some((key, n)));
                n
            }
        };
        elements.saturating_mul(rules as u64 + 1)
    }

    /// The same, and `want` is styled even inside `display:none` (`RuleSet::compute_for`).
    fn cascade_for(&self, it: &mut Interp<'_>, want: Option<NodeId>) -> R<Rc<Styles>> {
        self.cascade_with(it, want, false)
    }

    fn cascade_with(&self, it: &mut Interp<'_>, want: Option<NodeId>, keep: bool) -> R<Rc<Styles>> {
        let l = self.computed.lim.get();
        let vp = self.plat.s.borrow().viewport;
        if self.doc.borrow().len() > l.max_nodes {
            return Err(self.lim("computed-nodes"));
        }
        let mut doc = self.doc.borrow_mut();
        let stamp = (doc.revision(), doc.text_revision(), doc.len(), self.computed.sheet_gen.get());
        // The same inputs as the plan of the rules in the cache: that plan was not strict, and its sheets are the rules' sheets.
        let same = self.computed.cache.borrow().as_ref().is_some_and(|b| b.vp == vp && b.stamp == stamp);
        if !same {
            it.charge(doc.len() as u64 / 4 + 1)?;
            let plan = self.plan(&mut doc, vp, false)?;
            if plan.strict {
                return Err(Self::gate(MISSING));
            }
            it.charge(plan.bytes as u64 / 32)?;
            let mut cache = self.computed.cache.borrow_mut();
            if !cache.as_ref().is_some_and(|b| b.vp == vp && b.key == plan.parts) {
                // Sheets only added at the end (a script appends a `<link>` or `<style>`): add them to the rules of the last build.
                // Adding in order gives the same rules as a new build, without parsing the sheets again.
                let old = cache.take().filter(|b| b.vp == vp && plan.parts.starts_with(&b.key));
                let from = old.as_ref().map_or(0, |b| b.key.len());
                let mut rules = match old {
                    Some(b) => b.rules,
                    None => {
                        let mut rules = RuleSet::new(vp);
                        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
                        rules
                    }
                };
                let root = doc.root();
                for (scope, layer, text) in &plan.parts[from..] {
                    if *scope == root {
                        rules.add_in_layer(&mut doc, text, Origin::Author, layer.as_deref());
                    } else {
                        rules.add_scoped(&mut doc, text, Origin::Author, layer.as_deref(), *scope);
                    }
                }
                self.computed.builds.set(self.computed.builds.get() + 1);
                if rules.len() > l.max_rules {
                    return Err(self.lim("computed-rules"));
                }
                if doc.name_count() > self.lim.max_names {
                    return Err(self.lim("dom-names"));
                }
                *cache = Some(Built { vp, key: plan.parts, rules, last: [None, None], read: None, odd: OnceCell::new(), stamp });
            }
            cache.as_mut().expect("built").stamp = stamp;
        }
        let mut cache = self.computed.cache.borrow_mut();
        let built = cache.as_mut().expect("built");
        // The bound of a full compute: nodes x (rules + 1), or styled elements x (rules + 1) when that is over `STYLED_FROM`. It is the cap
        // check, and the steps of the full compute below. A read that only restyles charges for the elements that it restyles.
        let mut work = (doc.len() as u64).saturating_mul(built.rules.len() as u64 + 1);
        if work > STYLED_FROM {
            work = self.styled_work(&doc, built.rules.len());
        }
        if work > l.max_work {
            return Err(self.lim("computed-work"));
        }
        // The cascade reads only the tree, the attributes, the text, the `set_ui` and `set_states` state (they move
        // `revision`), the hover, active and focus state (`UserState::revision`) and the custom element state. `attach_shadow`
        // adds a node, so the length moves. A custom element state change moves no counter, so the cascade is kept and used
        // only while no element has custom element state.
        let key = (doc.revision(), doc.text_revision(), doc.len(), doc.user().revision, want);
        let slot = usize::from(want.is_some());
        if let Some((k, kept, styles)) = &built.last[slot]
            && *k == key
            && (*kept || !keep)
            && !doc.has_custom()
        {
            // The styles of the last read are still right: no cascade runs, so a read charges the check only.
            it.charge(1)?;
            return Ok(styles.clone());
        }
        // A read after changes: restyle the cascade of the last read only where the changes reach, as a window frame does.
        let read = want.is_none() && !keep;
        let old = if slot == 0 { built.read.take() } else { None };
        let marks = old.as_ref().filter(|_| read).and_then(|_| doc.take_damage(DAMAGE_STYLE));
        drop(doc);
        let doc = self.doc.borrow();
        if let (Some(old), Some(marks), Some((k, true, styles))) = (old, marks, built.last[0].as_mut())
            && !doc.has_custom()
            && let Some(st) = Rc::get_mut(styles)
        {
            let new = chains(&doc);
            if restyle_marks(it, &built.rules, &doc, st, &marks, &old, &new)?.is_some() {
                *k = key;
                built.read = Some(new);
                self.computed.restyles.set(self.computed.restyles.get() + 1);
                return Ok(styles.clone());
            }
        }
        it.charge(work / UNITS_PER_STEP + 1)?;
        self.computed.computes.set(self.computed.computes.get() + 1);
        // The cascade of a read keeps what `restyle` needs: the next read after a change updates it.
        let styles = Rc::new(if keep || read { built.rules.compute_kept(&doc) } else { built.rules.compute_for(&doc, want) });
        built.last[slot] = (!doc.has_custom()).then(|| (key, keep || read, styles.clone()));
        if read && !doc.has_custom() {
            built.read = Some(chains(&doc));
            drop(doc);
            self.doc.borrow_mut().track_damage(DAMAGE_STYLE);
        }
        Ok(styles)
    }

    /// How many times the rule set was built. Styles from one build can be updated with the rules of the same build only.
    pub(crate) fn rule_builds(&self) -> u64 {
        self.computed.builds.get()
    }

    /// Update `styles` (from `cascade_frame` of rule build `builds`) for the changes `marks` of the tree and for the user state that
    /// moved from the element chains `old` to `new` (`:hover`, `:active`, focus: each element and its ancestors). Gives the elements
    /// whose style changed, with their old style number (`RuleSet::restyle`). `None` when only a new cascade is right: the rules were
    /// built again, `styles` is shared, or `restyle_marks` says so. Then `styles` is as it was, except for new styles at its end
    /// that no node uses.
    pub(crate) fn restyle_frame(&self, it: &mut Interp<'_>, styles: &mut Rc<Styles>, builds: u64, marks: &[Mark], old: &[Vec<NodeId>; 3], new: &[Vec<NodeId>; 3]) -> R<Option<Vec<(NodeId, u32)>>> {
        let vp = self.plat.s.borrow().viewport;
        let mut cache = self.computed.cache.borrow_mut();
        let Some(built) = cache.as_mut().filter(|b| b.vp == vp && self.computed.builds.get() == builds) else { return Ok(None) };
        // The kept last cascade is this one, from before the changes: it does not match the document now.
        for last in &mut built.last {
            if last.as_ref().is_some_and(|l| Rc::ptr_eq(&l.2, styles)) {
                *last = None;
            }
        }
        if built.last[0].is_none() {
            built.read = None;
        }
        let Some(styles) = Rc::get_mut(styles) else { return Ok(None) };
        let doc = self.doc.borrow();
        if doc.len() > self.computed.lim.get().max_nodes {
            return Err(self.lim("computed-nodes"));
        }
        restyle_marks(it, &built.rules, &doc, styles, marks, old, new)
    }

    /// `snapshot` of an element with no style in the cascade of the reads (it is inside `display: none`): the cascade of the reads,
    /// with the styles of the way down to `el` at its end (`RuleSet::style_hidden`), and the style of `el`. `None` when that cannot
    /// be done: then a walk with `want` (`cascade_for`) gives it.
    fn style_hidden(&self, it: &mut Interp<'_>, el: NodeId) -> R<Option<(Rc<Styles>, ComputedStyle)>> {
        drop(self.cascade_for(it, None)?);
        let mut cache = self.computed.cache.borrow_mut();
        let Some(built) = cache.as_mut().filter(|b| b.read.is_some()) else { return Ok(None) };
        let Some((_, true, styles)) = built.last[0].as_mut() else { return Ok(None) };
        let Some(st) = Rc::get_mut(styles) else { return Ok(None) };
        let doc = self.doc.borrow();
        let Some(style) = built.rules.style_hidden(&doc, st, el) else { return Ok(None) };
        self.computed.restyles.set(self.computed.restyles.get() + 1);
        Ok(Some((styles.clone(), style)))
    }

    /// The style of `el` now, or `None` when the element is not connected (an empty declaration).
    pub(crate) fn snapshot(&self, it: &mut Interp<'_>, el: NodeId) -> R<Option<Snap>> {
        if !self.in_document(it, el)? {
            return Ok(None);
        }
        let mut styles = self.cascade_for(it, None)?;
        let style = match styles.style(el).cloned() {
            Some(style) => style,
            None => {
                // Not rendered: style the way down to `el` too. A rendered element never pays for it.
                drop(styles);
                match self.style_hidden(it, el)? {
                    Some((s, style)) => {
                        styles = s;
                        style
                    }
                    None => {
                        styles = self.cascade_for(it, Some(el))?;
                        let Some(style) = styles.style(el).cloned() else { return Err(Self::gate(NO_STYLE)) };
                        style
                    }
                }
            }
        };
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
    /// `kind`: 0 for the element, `PSEUDO` for a pseudo-element (its inherited values are the element's, the rest is initial), `EMPTY` for no style.
    fn computed_value(&self, it: &mut Interp<'_>, el: NodeId, name: &str, kind: u8) -> R<String> {
        if kind == EMPTY {
            return Ok(String::new());
        }
        let is_custom = custom(name);
        let key = if is_custom { name.to_string() } else { name.to_ascii_lowercase() };
        // `content` is not a property of this engine. A pseudo-element with no rule has `normal`.
        if kind == PSEUDO && key == "content" {
            return Ok("normal".into());
        }
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
        // A pseudo-element has no box here: no layout, and the computed values stay.
        let used = if kind == PSEUDO { None } else { self.used_for(it, &s, el, longs)? };
        let init = (kind == PSEUDO).then(|| Snap { styles: s.styles.clone(), style: ComputedStyle::initial(), hidden: true });
        let pick = |p: Property| match &init {
            Some(i) if !inherited(p) => i,
            _ => &s,
        };
        let doc = self.doc.borrow();
        let ctx = used.as_ref().map(|u| Ctx::new(&doc, &s.styles, u, el));
        // The engine keeps only the line of `text-decoration`; Chrome writes the shorthand as that line when the style and the color are the defaults.
        if long.is_some() || key == "text-decoration" {
            return pick(longs[0]).text(longs[0], ctx.as_ref()).map_err(Self::gate);
        }
        if key == "grid-row" || key == "grid-column" {
            let (a, b) = (pick(longs[0]).text(longs[0], ctx.as_ref()).map_err(Self::gate)?, pick(longs[1]).text(longs[1], ctx.as_ref()).map_err(Self::gate)?);
            // `start / end`: an `auto` end is left out, and so is an end that repeats a name.
            let name = a.starts_with(|c: char| c.is_alphabetic() || c == '_') && !a.starts_with("span ") && a != "auto";
            return Ok(if b == "auto" || (name && a == b) { a } else { format!("{a} / {b}") });
        }
        let mut text = String::new();
        for &p in longs {
            text.push_str(&format!("{}:{};", p.name(), pick(p).text(p, ctx.as_ref()).map_err(Self::gate)?));
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

    /// 0 for the element, `PSEUDO` or `EMPTY` (see `computed_value`). Call it after `computed_el` accepted `this`.
    fn computed_kind(&self, this: Value) -> u8 {
        match this {
            Value::Object(o) => self.computed.kinds.borrow().get(&o).copied().unwrap_or(0),
            _ => 0,
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

/// Restyle `styles` (kept by `RuleSet::compute_kept` with `rules`) for the changes `marks` and the user state chains `old` to `new`.
/// Gives the elements whose style changed, with their old style number. `None` when only a new cascade is right: a change touched
/// a `<style>`, `<link>` or `<base>` element, the rules read what changed in a way that `RuleSet::roots` does not bound, or
/// `RuleSet::restyle` cannot.
fn restyle_marks(it: &mut Interp<'_>, rules: &RuleSet, doc: &bare_html::dom::Document, styles: &mut Styles, marks: &[Mark], old: &[Vec<NodeId>; 3], new: &[Vec<NodeId>; 3]) -> R<Option<Vec<(NodeId, u32)>>> {
    let sheets = ["style", "link", "base"].map(|n| doc.find_atom(n));
    let sheet = |n: NodeId| matches!(*doc.data(n), NodeData::Element { name, .. } if sheets.contains(&Some(name)));
    let scopes = rules.scopes();
    let mut roots = Vec::new();
    for &m in marks {
        let touched = match m {
            Mark::Link { parent, node } | Mark::Unlink { parent, node } => {
                sheet(parent) || {
                    let mut stack = vec![node];
                    let mut found = false;
                    while let Some(n) = stack.pop() {
                        it.charge(1)?;
                        found |= sheet(n);
                        stack.extend(doc.children(n));
                    }
                    found
                }
            }
            Mark::Text { node } => doc.parent(node).is_some_and(sheet),
            Mark::Attr { el, .. } => sheet(el),
            Mark::Ui { .. } | Mark::Image { .. } => false,
        };
        if touched || !rules.roots(doc, m, &mut roots) {
            return Ok(None);
        }
    }
    Scopes::user_roots(scopes.hover, &old[0], &new[0], doc, &mut roots);
    Scopes::user_roots(scopes.active, &old[1], &new[1], doc, &mut roots);
    Scopes::user_roots(scopes.focus, &old[2], &new[2], doc, &mut roots);
    it.charge(roots.len() as u64 + 1)?;
    let mut changed = Vec::new();
    if !rules.restyle(doc, styles, &roots, &mut changed) {
        return Ok(None);
    }
    // What a full cascade charges for each element, for each element that `RuleSet::restyle` styled: a root that is styled with its
    // subtree, and the subtree of a root whose style changed. A root with the same style costs one element.
    let (mut visited, changed_roots) = (0u64, changed.iter().map(|c| c.0).collect::<std::collections::HashSet<_>>());
    for &(root, subtree) in &roots {
        visited += elements_under(doc, root, if subtree || changed_roots.contains(&root) { doc.len() as u64 } else { 1 });
        if visited >= doc.len() as u64 {
            break;
        }
    }
    it.charge(visited.saturating_mul(rules.len() as u64 / UNITS_PER_STEP + 1))?;
    Ok(Some(changed))
}

/// The elements in the tree under `node` (the node too), as the cascade walks it: a shadow tree and the light children of its host.
/// Stops when the count reaches `max`.
fn elements_under(doc: &bare_html::dom::Document, node: NodeId, max: u64) -> u64 {
    let (mut n, mut stack) = (0u64, vec![node]);
    while let Some(node) = stack.pop() {
        n += u64::from(matches!(doc.data(node), NodeData::Element { .. }));
        if n >= max {
            break;
        }
        stack.extend(doc.shadow_root(node));
        let mut child = doc.last_child(node);
        while let Some(c) = child {
            stack.push(c);
            child = doc.prev_sibling(c);
        }
    }
    n
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
    let kind = if pseudo.starts_with(':') { pseudo_kind(&pseudo) } else { 0 };
    if d.computed.objs.borrow().len() >= d.computed.lim.get().max_objects {
        return Err(d.lim("computed-objects"));
    }
    d.wrap(it, el)?;
    let o = d.obj(it, *d.computed.proto.get().expect("installed"))?;
    d.computed.objs.borrow_mut().insert(o, el);
    if kind != 0 {
        d.computed.kinds.borrow_mut().insert(o, kind);
    }
    Ok(Value::Object(o))
}

fn get_property_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.computed_el(it, this)?;
    required(it, a, 1, "getPropertyValue")?;
    let name = d.string_arg(it, a[0])?;
    let v = d.computed_value(it, el, &name, d.computed_kind(this))?;
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
    let mut names = Property::LISTED.iter().map(|p| p.name()).collect::<Vec<_>>();
    names.sort_unstable();
    names.extend(s.styles.custom_properties(&s.style).iter().map(|(name, _)| &**name));
    names
}

fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.computed_el(it, this)?;
    if d.computed_kind(this) == EMPTY {
        return Ok(Value::Number(0.0));
    }
    let n = d.snapshot(it, el)?.map_or(0, |s| Property::LISTED.len() + s.styles.custom_properties(&s.style).len());
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
    if d.computed_kind(this) == EMPTY {
        return d.js_str(it, "");
    }
    let Some(s) = d.snapshot(it, el)? else { return d.js_str(it, "") };
    it.charge((Property::LISTED.len() + s.styles.custom_properties(&s.style).len()) as u64)?;
    d.js_str(it, names(&s).get(i).copied().unwrap_or(""))
}

fn named_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let name = current(d, it)?;
    let el = d.computed_el(it, this)?;
    let v = d.computed_value(it, el, name, d.computed_kind(this))?;
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
    d.computed.kinds.borrow_mut().retain(|&o, _| live(o));
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
            restyles: c.restyles.get(),
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
        let c = &self.dom.computed;
        c.pending_ok.set(on);
        c.sheet_gen.set(c.sheet_gen.get() + 1);
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

//! Bounded CSSOM objects. Sheet and rule lists are live host-indexed objects. All allocation is deferred until a CSSOM read.
//! Inline rule edits update the style text through the DOM. The existing sheet pipeline reads that text.
//! A rule object is made when a script reads its index. Until then a rule is a span (`Entry::Chunk`) of the sheet text, 12 bytes. The
//! declaration of a rule (`rule.style`) is the object of `browser_style` with the shared `CSSStyleDeclaration` prototype: no own accessors.
//! External rules require a supplied sheet and a same-origin URL. `CSSRule` is the base interface of every rule object. The interface shape and the
//! semantics follow Ladybird `LibWeb/CSS/CSSRule.cpp` (BSD-2).
use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_geom::illegal_s;
use crate::browser_scoped::*;
use crate::browser_style::to_u32_s;
use crate::browser_sources::Source;
use bare_js_api::{Args, HostFn, Local};
use crate::browser_dom::{attr_set, next_in};
use bare_html::dom::{Document, NodeData, NodeId};
use crate::eng::Slot;
use bare_js_api::Pending;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

// The CSSOM objects of a page follow the collector. `State::roots` holds the prototypes and constructors only. Every other object is an
// owner (`browser_sources.rs`) and visits its references in `trace_*`: a sheet reaches its list, a list its sheet or rule and its rule
// objects, a rule its sheet, parent, nested list, declaration and media list, and a declaration or media list its rule. The wrapper of
// the owner element reaches the sheet of the element (`el.sheet === el.sheet`), and the wrapper of a document or shadow root reaches the
// sheets that it adopted. A rule that leaves its list (deleted, or a replaced text) is garbage unless a script holds it, and `prune_*`
// drops the host table entries of a collected object. So the number of objects follows the live rules, and no fixed cap is needed. A rule
// object costs about 0.5 KB when only its selector is read (measured on a 16 MiB sheet: 187,852 rules, 585 B each). The declaration of a
// rule counts for `style-objects` too (65,536, `browser_style`).
const MAX_TEXT: usize = 32 << 20;
// The rules of one list. A rule that no script has read is an `Entry` of 12 bytes, so this holds the list near 24 MiB.
const MAX_RULES: usize = 1 << 21;
// An item of a list that does not exist yet: the index that asks for the length only (as `browser_attr` and `browser_dataset` do).
const LENGTH_ONLY: u32 = u32::MAX;
// An item of a rule list: the object, or the span of its text that no script has read yet. The offsets are in the source of the
// sheet (a rule list of a sheet) or in `Rule::raw` of the grouping rule that owns the list.
#[derive(Clone, Copy, PartialEq)]
enum Entry<E: Eng> {
    Obj(E::Obj),
    Chunk(u32, u32),
}
#[derive(Clone)]
struct Sheet<E: Eng> {
    node: NodeId,
    list: E::Obj,
    source: String,
    text_key: Option<Vec<(NodeId, bare_html::dom::Span)>>,
    // `new CSSStyleSheet()`: no owner node (`node` is the document root, unused), the text is in `source`. `stale`: the rules are not parsed yet.
    ctor: bool,
    stale: bool,
    // Constructed sheets can defer writing the source in list order.
    edits: SheetEdits,
    // A `<style>` sheet that CSSOM edited: the text key of the element at the first edit. While the element text has this key, `source` is
    // the text in use, as for a constructed sheet. The element text stays: in the spec an edit changes the rules only.
    owned: Option<Vec<(NodeId, bare_html::dom::Span)>>,
}
// The text in use is `source`, and an edit can change it in place.
fn in_place<E: Eng>(sh: &Sheet<E>) -> bool {
    sh.ctor || sh.owned.is_some()
}
#[derive(Clone, Copy, Default)]
struct SheetEdits {
    // Size in list order, including the newline after each rule.
    bytes: usize,
    // Top-level entries that have a CSSRule object.
    objects: u32,
    // The source holds the spans but may include deleted text or have a different order.
    dirty: bool,
}
// The interface of a rule. The value is the index of its prototype in `State::protos`.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Other,
    Style,
    Media,
    Supports,
    Import,
    FontFace,
    Keyframes,
    Keyframe,
}
#[derive(Clone)]
struct Rule<E: Eng> {
    sheet: E::Obj,
    kind: Kind,
    selector: Option<String>,
    raw: String,
    // The declaration object (`rule.style`). It is made when a script first needs it, from `decl`.
    style: Option<E::Obj>,
    decl: Option<String>,
    // The condition of `@media` and `@supports`, the name of `@keyframes`, the key list of a keyframe, the prelude of `@import`.
    // The nested rules of `@media`, `@supports` and `@keyframes` are in `list`.
    prelude: String,
    list: Option<E::Obj>,
    media: Option<E::Obj>,
    parent: Option<E::Obj>,
    // The bytes used by the cascade until the next edit serializes this object.
    sheet_span: (u32, u32),
}
#[derive(Clone)]
struct List<E: Eng> {
    sheet: Option<E::Obj>,
    // The grouping rule that owns the list.
    rule: Option<E::Obj>,
    items: Vec<Entry<E>>,
    stamp: Option<(u64, u64, u64)>,
    // How deep the rules of the list are nested: 0 for a sheet.
    depth: usize,
}
pub(crate) struct State<E: Eng> {
    roots: Vec<E::Obj>,
    sheet_p: E::Obj,
    // The prototypes of the rule interfaces, indexed by `Kind`.
    protos: Vec<E::Obj>,
    list_p: E::Obj,
    rule_list_p: E::Obj,
    media_p: E::Obj,
    // A `MediaList` object, and the rule that it reads.
    medias: IdMap<E::Obj, E::Obj>,
    doc_list: Option<E::Obj>,
    sheets: IdMap<E::Obj, Sheet<E>>,
    by_node: HashMap<NodeId, E::Obj>,
    rules: IdMap<E::Obj, Rule<E>>,
    lists: IdMap<E::Obj, List<E>>,
    // The declaration object of a rule -> the rule. A write through the declaration publishes the sheet (`declaration_written`).
    style_rules: IdMap<E::Obj, E::Obj>,
    // The constructed sheets adopted by the document (key: its root) or a shadow root, in order.
    adopted: HashMap<NodeId, Vec<E::Obj>>,
}
#[derive(Default)]
pub(crate) struct Cssom<E: Eng> {
    state: OnceCell<RefCell<State<E>>>,
    style_p: OnceCell<<E as Eng>::Obj>,
    // How far the revision and the text revision of the document moved by the detached elements of rule declarations (`declaration`).
    // The list stamps leave them out: making a rule object must not make every rule list stale.
    own: Cell<(u64, u64)>,
}
fn state<E: Eng>(d: &Dom<E>) -> &RefCell<State<E>> {
    d.cssom.state.get().expect("CSSOM initialized")
}
pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    out.extend(d.cssom.style_p.get().copied().map(E::obj_raw));
    let Some(cell) = d.cssom.state.get() else { return };
    out.extend(cell.borrow().roots.iter().copied().map(E::obj_raw));
}
/// The references of the rule `o` (its owner entry, `browser_sources.rs`): its sheet, declaration, list, media list and parent.
pub(crate) fn trace_rule<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let s = cell.borrow();
    let Some(r) = s.rules.get(&o) else { return };
    for o in [Some(r.sheet), r.style, r.list, r.media, r.parent].into_iter().flatten() {
        crate::browser_sources::visit::<E>(t, E::obj_raw(o));
    }
}
/// The reference of the sheet `o`: its rule list.
pub(crate) fn trace_sheet<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let Some(cell) = d.cssom.state.get() else { return };
    if let Some(sh) = cell.borrow().sheets.get(&o) {
        crate::browser_sources::visit::<E>(t, E::obj_raw(sh.list));
    }
}
/// The reference of the media list `o`: its rule.
pub(crate) fn trace_media<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let Some(cell) = d.cssom.state.get() else { return };
    if let Some(&r) = cell.borrow().medias.get(&o) {
        crate::browser_sources::visit::<E>(t, E::obj_raw(r));
    }
}
/// The reference of the declaration object `o` of a rule, if it is one: its rule.
#[inline]
pub(crate) fn trace_decl<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some(cell) = d.cssom.state.get()
        && let Some(&r) = cell.borrow().style_rules.get(&o)
    {
        crate::browser_sources::visit::<E>(t, E::obj_raw(r));
    }
}
/// The references of the wrapper of the node `n`: the sheet of its element, and the sheets that it adopted (a document or shadow root).
pub(crate) fn trace_node<E: Eng>(d: &Dom<E>, n: NodeId, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let s = cell.borrow();
    if !s.by_node.is_empty()
        && let Some(&o) = s.by_node.get(&n)
    {
        crate::browser_sources::visit::<E>(t, E::obj_raw(o));
    }
    if !s.adopted.is_empty() {
        for &o in s.adopted.get(&n).into_iter().flatten() {
            crate::browser_sources::visit::<E>(t, E::obj_raw(o));
        }
    }
}
/// The references of the rule list `o` (its owner entry, `browser_sources.rs`): its sheet, its rule and its rule objects.
pub(crate) fn trace_list<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let s = cell.borrow();
    let Some(l) = s.lists.get(&o) else { return };
    let items = l.items.iter().filter_map(|e| if let &Entry::Obj(o) = e { Some(o) } else { None });
    for o in l.sheet.into_iter().chain(l.rule).chain(items) {
        crate::browser_sources::visit::<E>(t, E::obj_raw(o));
    }
}
/// Drop the entry of the rule list `o`, which the collector freed.
pub(crate) fn prune_list<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(cell) = d.cssom.state.get() {
        cell.borrow_mut().lists.remove(&o);
    }
}
/// Drop the entry of the rule `o`, which the collector freed.
pub(crate) fn prune_rule<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(cell) = d.cssom.state.get() {
        cell.borrow_mut().rules.remove(&o);
    }
}
/// Drop the entry of the sheet `o`, which the collector freed, the sheet of its element, and its place in an adopted list. A sheet is
/// freed with the wrapper of its root only when that root is detached and no script holds it.
pub(crate) fn prune_sheet<E: Eng>(d: &Dom<E>, o: E::Obj) {
    let Some(cell) = d.cssom.state.get() else { return };
    let mut s = cell.borrow_mut();
    if let Some(sh) = s.sheets.remove(&o)
        && s.by_node.get(&sh.node) == Some(&o)
    {
        s.by_node.remove(&sh.node);
    }
    if !s.adopted.is_empty() {
        s.adopted.retain(|_, v| {
            v.retain(|&x| x != o);
            !v.is_empty()
        });
    }
}
/// Drop the entry of the media list `o`, which the collector freed.
pub(crate) fn prune_media<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(cell) = d.cssom.state.get() {
        cell.borrow_mut().medias.remove(&o);
    }
}
/// Drop the entry of the declaration object `o`, which the collector freed, if it is the one of a rule.
pub(crate) fn prune_decl<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(cell) = d.cssom.state.get() {
        cell.borrow_mut().style_rules.remove(&o);
    }
}
// A prototype or a constructor: an object that stays as long as the page.
fn keep<E: Eng>(d: &Dom<E>, o: E::Obj) -> R<E, E::Obj> {
    state(d).borrow_mut().roots.push(o);
    Ok(o)
}
#[cfg(test)]
pub(crate) fn rule_objects<E: Eng>(d: &Dom<E>) -> usize {
    d.cssom.state.get().map_or(0, |s| s.borrow().rules.len())
}
#[cfg(test)]
thread_local! {
    static WORK: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}
/// The number of rules returned by the parser and entries written to sheet text.
#[cfg(test)]
pub(crate) fn take_work() -> (usize, usize) {
    WORK.replace((0, 0))
}
fn parse_cssom(text: &str) -> Vec<bare_css::parser::Rule<'_>> {
    let rules = bare_css::parser::parse_stylesheet(text);
    #[cfg(test)]
    WORK.with(|w| {
        let (parsed, published) = w.get();
        w.set((parsed + rules.len(), published));
    });
    rules
}
/// The entries of the host tables: (rules, lists, sheets, declarations, media lists).
#[cfg(test)]
pub(crate) fn table_sizes<E: Eng>(d: &Dom<E>) -> (usize, usize, usize, usize, usize) {
    d.cssom.state.get().map_or((0, 0, 0, 0, 0), |s| {
        let s = s.borrow();
        (s.rules.len(), s.lists.len(), s.sheets.len(), s.style_rules.len(), s.medias.len())
    })
}
#[cold]
fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    if d.cssom.state.get().is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let (sp, lp, rlp, base) = (d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?);
    let rp = d.obj(it, base)?;
    let (gp, mlp) = (d.obj(it, base)?, d.obj(it, op)?);
    let cp = d.obj(it, gp)?;
    let (mp, supp) = (d.obj(it, cp)?, d.obj(it, cp)?);
    let (ip, fp, kfsp, kfp) = (d.obj(it, base)?, d.obj(it, base)?, d.obj(it, base)?, d.obj(it, base)?);
    d.cssom
        .state
        .set(RefCell::new(State {
            roots: vec![sp, base, rp, gp, cp, mp, supp, ip, fp, kfsp, kfp, mlp, lp, rlp],
            sheet_p: sp,
            protos: vec![base, rp, mp, supp, ip, fp, kfsp, kfp],
            list_p: lp,
            rule_list_p: rlp,
            media_p: mlp,
            medias: IdMap::default(),
            doc_list: None,
            sheets: IdMap::default(),
            by_node: HashMap::new(),
            rules: IdMap::default(),
            lists: IdMap::default(),
            style_rules: IdMap::default(),
            adopted: HashMap::new(),
        }))
        .ok();
    let mut ctors: HashMap<E::Obj, E::Obj> = HashMap::new();
    for (name, p, parent) in [
        ("CSSStyleSheet", sp, None),
        ("CSSRule", base, None),
        ("CSSStyleRule", rp, Some(base)),
        ("CSSGroupingRule", gp, Some(base)),
        ("CSSConditionRule", cp, Some(gp)),
        ("CSSMediaRule", mp, Some(cp)),
        ("CSSSupportsRule", supp, Some(cp)),
        ("CSSImportRule", ip, Some(base)),
        ("CSSFontFaceRule", fp, Some(base)),
        ("CSSKeyframesRule", kfsp, Some(base)),
        ("CSSKeyframeRule", kfp, Some(base)),
        ("MediaList", mlp, None),
        ("StyleSheetList", lp, None),
        ("CSSRuleList", rlp, None),
    ] {
        crate::browser_lazy::tag(d, it, p, name)?;
        let f = if name == "CSSStyleSheet" { d.add_fn_s_obj(it, sheet_ctor_s, name, 0, HostFn::CtorNewTarget)? } else { d.add_fn_s_obj(it, illegal_constructor, name, 0, HostFn::Method)? };
        keep(d, f)?;
        ctors.insert(p, f);
        if let Some(pf) = parent.and_then(|pp| ctors.get(&pp).copied()) {
            it.h_set_proto(f, Some(pf))?;
        }
        let (kp, kc, k) = (d.key(it, "prototype")?, d.key(it, "constructor")?, d.key(it, name)?);
        it.define_data(f, kp, E::obj_raw(p), false, false, false)?;
        it.define_data(p, kc, E::obj_raw(f), true, false, true)?;
        let own = it.h_peek_own(it.global_object(), k)?;
        if own.is_none() || matches!(own, Some(crate::eng::Prop::Accessor { .. })) {
            it.define_data(it.global_object(), k, E::obj_raw(f), true, false, true)?;
        }
    }
    d.def_get_s(it, sp, "cssRules", rules_get_s, None)?;
    d.def_get_s(it, sp, "ownerNode", owner_get_s, None)?;
    d.def_get_s(it, sp, "href", href_get_s, None)?;
    d.def_get_s(it, sp, "disabled", disabled_get_s, Some(disabled_set))?;
    d.def_method_s(it, sp, "insertRule", 1, insert)?;
    d.def_method_s(it, sp, "deleteRule", 1, delete)?;
    d.def_method_s(it, sp, "replaceSync", 1, replace_sync)?;
    d.def_method_s(it, sp, "replace", 1, replace)?;
    d.def_get_s(it, rp, "selectorText", selector_get_s, Some(selector_set))?;
    d.def_get_s(it, rp, "style", style_get_s, None)?;
    d.def_get_s(it, gp, "cssRules", group_rules_get_s, None)?;
    d.def_method_s(it, gp, "insertRule", 1, group_insert)?;
    d.def_method_s(it, gp, "deleteRule", 1, group_delete)?;
    d.def_get_s(it, cp, "conditionText", condition_get_s, None)?;
    d.def_get_s(it, mp, "media", media_get_s, None)?;
    d.def_get_s(it, ip, "media", media_get_s, None)?;
    d.def_get_s(it, ip, "href", import_href_get_s, None)?;
    d.def_get_s(it, ip, "styleSheet", import_sheet_get_s, None)?;
    d.def_get_s(it, fp, "style", style_get_s, None)?;
    d.def_get_s(it, kfsp, "name", condition_get_s, None)?;
    d.def_get_s(it, kfsp, "cssRules", group_rules_get_s, None)?;
    d.def_get_s(it, kfp, "keyText", condition_get_s, Some(key_text_set))?;
    d.def_get_s(it, kfp, "style", style_get_s, None)?;
    d.def_get_s(it, mlp, "mediaText", media_text_get_s, None)?;
    d.def_get_s(it, base, "cssText", rule_text_s, None)?;
    d.def_get_s(it, base, "type", type_get_s, None)?;
    d.def_get_s(it, base, "parentStyleSheet", parent_get_s, None)?;
    d.def_get_s(it, base, "parentRule", parent_rule_get_s, None)?;
    // The type constants are on the constructor and on the prototype.
    let consts = [
        ("STYLE_RULE", 1),
        ("CHARSET_RULE", 2),
        ("IMPORT_RULE", 3),
        ("MEDIA_RULE", 4),
        ("FONT_FACE_RULE", 5),
        ("PAGE_RULE", 6),
        ("KEYFRAMES_RULE", 7),
        ("KEYFRAME_RULE", 8),
        ("MARGIN_RULE", 9),
        ("NAMESPACE_RULE", 10),
        ("COUNTER_STYLE_RULE", 11),
        ("SUPPORTS_RULE", 12),
        ("FONT_FEATURE_VALUES_RULE", 14),
    ];
    for (name, n) in consts {
        let k = d.key(it, name)?;
        for o in [ctors[&base], base] {
            it.define_data(o, k, E::num(n as f64), false, true, false)?;
        }
    }
    // The lists are array-like: their `[Symbol.iterator]` is the one of `Array.prototype`, as in browsers.
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let values = it.get_value(E::obj_raw(it.array_prototype()), sym)?;
    for p in [lp, rlp] {
        d.def_get_s(it, p, "length", list_length_s, None)?;
        d.def_method_s(it, p, "item", 1, list_item_s)?;
        it.define_data(p, sym, values, true, false, true)?;
    }
    Ok(())
}
// Resolve only through the existing cold host-property miss path.
#[cold]
pub(crate) fn define<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &str) -> R<E, bool> {
    match name {
        "CSSStyleSheet" | "CSSRule" | "CSSStyleRule" | "CSSRuleList" | "StyleSheetList" | "CSSGroupingRule" | "CSSConditionRule" | "CSSMediaRule"
        | "CSSSupportsRule" | "CSSImportRule" | "CSSFontFaceRule" | "CSSKeyframesRule" | "CSSKeyframeRule" | "MediaList" => {
            ensure(d, it)?;
            Ok(true)
        }
        "adoptedStyleSheets" => {
            for p in [d.p().document, d.p().shadow] {
                let key = d.key(it, name)?;
                if it.h_peek_own(p, key)?.is_none() {
                    d.def_get_s(it, p, name, adopted_get, Some(adopted_set))?;
                }
            }
            ensure(d, it)?;
            Ok(true)
        }
        "styleSheets" => {
            let key = d.key(it, name)?;
            if it.h_peek_own(d.p().document, key)?.is_none() {
                d.def_get_s(it, d.p().document, name, document_list, None)?;
            }
            Ok(true)
        }
        "sheet" => {
            let p = style_proto(d, it)?;
            let key = d.key(it, name)?;
            if it.h_peek_own(d.p().link, key)?.is_none() {
                d.def_get_s(it, d.p().link, name, sheet_get_s, None)?;
            }
            let nodes: Vec<_> = d.st.borrow().wrappers().collect();
            it.charge_o(nodes.len() as u64)?;
            for (n, o) in nodes {
                if d.doc.borrow().is_html(n, "style") {
                    it.h_set_proto(o, Some(p))?;
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn wrapper_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    if d.html_on.get() {
        crate::browser_html::proto(d, it, "style")
    } else {
        Ok(d.cssom.style_p.get().copied().unwrap_or(d.p().html))
    }
}

// Created only after a sheet or HTMLStyleElement read.
#[cold]
pub(crate) fn style_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    if let Some(&p) = d.cssom.style_p.get() {
        return Ok(p);
    }
    let p = d.obj(it, d.p().html)?;
    d.def_get_s(it, p, "sheet", sheet_get_s, None)?;
    d.cssom.style_p.set(p).ok();
    Ok(p)
}
fn new_list<E: Eng>(d: &Dom<E>, it: &mut E::Vm, sheet: Option<E::Obj>) -> R<E, E::Obj> {
    let p = {
        let s = state(d).borrow();
        if sheet.is_some() { s.rule_list_p } else { s.list_p }
    };
    let o = d.new_source(it, crate::browser_sources::Source::RuleList, Some(p))?;
    state(d).borrow_mut().lists.insert(
        o,
        List {
            sheet,
            rule: None,
            items: vec![],
            stamp: None,
            depth: 0,
        },
    );
    Ok(o)
}
fn document_list<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    if d.nd_s(s, this)? != d.doc.borrow().root() {
        return Err(s.type_error("Illegal invocation"));
    }
    let o = old::<E, _>(s, |it| {
        ensure(d, it)?;
        let old = state(d).borrow().doc_list;
        match old {
            Some(o) => Ok(o),
            None => {
                let o = new_list(d, it, None)?;
                keep(d, o)?;
                state(d).borrow_mut().doc_list = Some(o);
                Ok(o)
            }
        }
    })?;
    Ok(d.obj_local(s, o))
}
fn sheet_of<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId) -> R<E, E::Obj> {
    ensure(d, it)?;
    if let Some(&o) = state(d).borrow().by_node.get(&n) {
        return Ok(o);
    }
    let p = state(d).borrow().sheet_p;
    let o = d.new_owned(it, Source::Sheet, p)?;
    let list = new_list(d, it, Some(o))?;
    state(d).borrow_mut().sheets.insert(
        o,
        Sheet {
            node: n,
            list,
            source: String::new(),
            text_key: None,
            ctor: false,
            stale: false,
            edits: SheetEdits::default(),
            owned: None,
        },
    );
    state(d).borrow_mut().by_node.insert(n, o);
    d.wrapper_edges.set(true);
    Ok(o)
}
// "Update a style block": the element gets a sheet when its root is any document or a shadow root, not only the page document.
// A sheet in another document is never read by the cascade of the page, and the page `styleSheets` does not list it.
fn has_style_root<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId) -> R<E, bool> {
    let doc = d.doc.borrow();
    let mut top = n;
    while let Some(p) = doc.parent(top) {
        it.charge_o(1)?;
        top = p;
    }
    Ok(matches!(doc.data(top), NodeData::Document | NodeData::ShadowRoot))
}
fn sheet<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
    match obj_id::<E>(s, v) {
        Some(o) if d.cssom.state.get().is_some_and(|st| st.borrow().sheets.contains_key(&o)) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}
fn rule<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
    match obj_id::<E>(s, v) {
        Some(o) if d.cssom.state.get().is_some_and(|st| st.borrow().rules.contains_key(&o)) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}
fn node<E: Eng>(d: &Dom<E>, o: E::Obj) -> NodeId {
    state(d).borrow().sheets[&o].node
}
fn is_ctor<E: Eng>(d: &Dom<E>, o: E::Obj) -> bool {
    state(d).borrow().sheets[&o].ctor
}
fn disabled_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet(d, s, t)?;
    if is_ctor(d, o) {
        return Ok(s.undefined());
    }
    let n = node(d, o);
    let v = arg_s::<E>(s, a, 0);
    if s.to_boolean(v) {
        old::<E, _>(s, |it| attr_set(d, it, n, "disabled", ""))?;
    } else {
        let v = d.wrap_s(s, n)?;
        let k = d.js_str_s(s, "disabled")?;
        call_hs(d, s, crate::browser_dom::remove_attribute_s, v, &[k])?;
    }
    Ok(s.undefined())
}
fn text_key(doc: &bare_html::dom::Document, node: NodeId) -> Vec<(NodeId, bare_html::dom::Span)> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if let bare_html::dom::NodeData::Text(span) = *doc.data(n) { out.push((n, span)); }
        stack.extend(doc.children(n));
    }
    out
}

fn pending_key<E: Eng>(d: &Dom<E>, doc: &bare_html::dom::Document, node: NodeId, rules: &[Entry<E>]) -> Option<Vec<(NodeId, bare_html::dom::Span)>> {
    if !d.style.has_pending.get() { return None; }
    let s = state(d).borrow();
    rules.iter().any(|e| matches!(e, Entry::Obj(r) if s.rules[r].style.and_then(|o| d.style_object_inputs(doc, o)).is_some())).then(|| text_key(doc, node))
}

pub(crate) fn sheet_inputs<E: Eng>(d: &Dom<E>, doc: &bare_html::dom::Document, node: NodeId, text: &str) -> Option<bare_css::cascade::InputSheet> {
    if !d.style.has_pending.get() { return None; }
    let cell = d.cssom.state.get()?;
    let s = cell.borrow();
    let sheet = s.sheets.get(s.by_node.get(&node)?)?;
    if sheet.source != text || sheet.text_key.as_ref()? != &text_key(doc, node) { return None; }
    let blocks: Vec<_> = s.lists[&sheet.list]
        .items
        .iter()
        .map(|e| match e {
            Entry::Obj(r) => s.rules[r].style.and_then(|o| d.style_object_inputs(doc, o)),
            Entry::Chunk(..) => None,
        })
        .collect();
    blocks.iter().any(Option::is_some).then_some(blocks)
}

/// The text in use of the `<style>` element `el` after a CSSOM edit (`Sheet::owned`), while the element text is the one of that edit.
pub(crate) fn owned_text<E: Eng>(d: &Dom<E>, doc: &bare_html::dom::Document, el: NodeId) -> Option<String> {
    let mut s = d.cssom.state.get()?.borrow_mut();
    let State { by_node, sheets, lists, rules, .. } = &mut *s;
    let sh = sheets.get_mut(by_node.get(&el)?)?;
    if *sh.owned.as_ref()? != text_key(doc, el) {
        return None;
    }
    flush_chunks(sh, lists.get_mut(&sh.list)?, rules);
    Some(sh.source.clone())
}

fn source<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, String> {
    let n = node(d, o);
    let (tag, href) = {
        let doc = d.doc.borrow();
        (name_of(&doc, n), attr(&doc, n, "href").unwrap_or("").to_owned())
    };
    if tag == "style" {
        return run::<E, _>(it, |s| {
            let v = d.wrap_s(s, n)?;
            let text = call_hs(d, s, crate::browser_dom::text_content_s, v, &[])?;
            d.string_arg_s(s, text)
        });
    }
    // The URL helper uses the document base. A link that is not same-origin must not reveal its rules.
    let rec = crate::browser_navigation_url::parse(&href, crate::browser_elements::base(d).as_ref()).ok();
    let same = d.cur_rec().ok().zip(rec).is_some_and(|(a, b)| a.origin() == b.origin());
    if !same {
        return Err(d.dom_err(it, "SecurityError", "Cannot access rules of a cross-origin stylesheet"));
    }
    if d.computed.ext.borrow().get(&href).is_some_and(|parts| parts.len() > 1) {
        return Err(Dom::gate("CSSOM of flattened imported sheets"));
    }
    let text = d
        .computed
        .ext
        .borrow()
        .get(&href)
        .map(|p| p.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    Ok(text)
}
fn declaration<E: Eng>(d: &Dom<E>, it: &mut E::Vm, text: &str) -> R<E, E::Obj> {
    let before = {
        let doc = d.doc.borrow();
        (doc.revision(), doc.text_revision())
    };
    let style = run::<E, _>(it, |s| d.rule_declaration_s(s, text));
    let doc = d.doc.borrow();
    let own = d.cssom.own.get();
    d.cssom.own.set((own.0 + doc.revision() - before.0, own.1 + doc.text_revision() - before.1));
    drop(doc);
    style
}
fn new_rule<E: Eng>(d: &Dom<E>, it: &mut E::Vm, mut r: Rule<E>, decl: Option<String>) -> R<E, E::Obj> {
    let p = state(d).borrow().protos[r.kind as usize];
    let o = d.new_owned(it, Source::Rule, p)?;
    r.decl = decl;
    state(d).borrow_mut().rules.insert(o, r);
    Ok(o)
}
// The declaration object of a rule, made now when it does not exist yet. `None`: the rule has no declaration.
fn style_of<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, Option<E::Obj>> {
    let (style, decl) = {
        let s = state(d).borrow();
        let r = &s.rules[&o];
        (r.style, r.decl.clone())
    };
    let Some(text) = decl else { return Ok(style) };
    let style = declaration(d, it, &text)?;
    let mut s = state(d).borrow_mut();
    let r = s.rules.get_mut(&o).unwrap();
    (r.style, r.decl) = (Some(style), None);
    s.style_rules.insert(style, o);
    Ok(Some(style))
}
fn declaration_text(ds: &[bare_css::parser::Declaration<'_>]) -> String {
    let mut decl = String::new();
    for p in ds {
        decl.push_str(p.name.as_ref());
        decl.push(':');
        decl.push_str(p.value);
        if p.important {
            decl.push_str(" !important");
        }
        decl.push(';');
    }
    decl
}
// The text inside the outer `{ }` of an at-rule chunk.
fn block_body(chunk: &str) -> Option<&str> {
    use bare_css::tokenizer::{Token, tokenize};
    let mut t = tokenize(chunk);
    while let Some((token, _, b)) = t.next_spanned() {
        if matches!(token, Token::OpenCurly) {
            let rest = &chunk[b..];
            return Some(rest.strip_suffix('}').unwrap_or(rest));
        }
    }
    None
}
// Nested group rules go this deep. Deeper ones are plain text, as in the parser of `bare_css`.
const MAX_DEPTH: usize = 32;
// The rules of a text that have a chunk that parses to a rule, as spans of `base` (`text` is a part of it). Nothing is kept of the parse.
fn chunk_entries<E: Eng>(d: &Dom<E>, it: &mut E::Vm, base: &str, text: &str) -> R<E, Vec<Entry<E>>> {
    if text.len() > MAX_TEXT {
        return Err(d.lim("cssom-text"));
    }
    it.charge_o(text.len() as u64)?;
    let mut out = Vec::new();
    for chunk in chunks(text) {
        if parse_cssom(chunk).is_empty() {
            continue;
        }
        if out.len() >= MAX_RULES {
            return Err(d.lim("cssom-rules"));
        }
        let a = chunk.as_ptr() as usize - base.as_ptr() as usize;
        out.push(Entry::Chunk(a as u32, (a + chunk.len()) as u32));
    }
    Ok(out)
}
// The rule objects of a text, made now (the text of one `insertRule`).
fn parsed_rules<E: Eng>(d: &Dom<E>, it: &mut E::Vm, sheet: E::Obj, parent: Option<E::Obj>, text: &str, depth: usize) -> R<E, Vec<E::Obj>> {
    if text.len() > MAX_TEXT {
        return Err(d.lim("cssom-text"));
    }
    let mut out = Vec::new();
    for chunk in chunks(text) {
        if let Some(o) = rule_from_chunk(d, it, sheet, parent, chunk, depth)? {
            out.push(o);
        }
    }
    Ok(out)
}
// The rule object of one chunk. `None`: the chunk is not a rule. The rules inside a grouping rule stay spans of its text.
fn rule_from_chunk<E: Eng>(d: &Dom<E>, it: &mut E::Vm, sheet: E::Obj, parent: Option<E::Obj>, chunk: &str, depth: usize) -> R<E, Option<E::Obj>> {
    it.charge_o(chunk.len() as u64)?;
    let mut parsed = parse_cssom(chunk).into_iter();
    let Some(r) = parsed.next() else { return Ok(None) };
    let base = Rule {
        sheet,
        kind: Kind::Other,
        selector: None,
        prelude: String::new(),
        raw: chunk.to_owned(),
        style: None,
        decl: None,
        list: None,
        media: None,
        parent,
        sheet_span: (0, 0),
    };
    Ok(Some(match r {
        bare_css::parser::Rule::Style(r) if parent.is_some_and(|p| state(d).borrow().rules[&p].kind == Kind::Keyframes) => {
            let rule = Rule { kind: Kind::Keyframe, prelude: r.prelude.to_owned(), raw: String::new(), ..base };
            new_rule(d, it, rule, Some(declaration_text(&r.declarations())))?
        }
        bare_css::parser::Rule::Style(r) => {
            let rule = Rule { kind: Kind::Style, selector: Some(r.prelude.to_owned()), raw: String::new(), ..base };
            new_rule(d, it, rule, Some(declaration_text(&r.declarations())))?
        }
        bare_css::parser::Rule::At(a) => {
            let kind = match a.name.as_ref() {
                "media" => Kind::Media,
                "supports" => Kind::Supports,
                "import" => Kind::Import,
                "font-face" => Kind::FontFace,
                n if n.ends_with("keyframes") => Kind::Keyframes,
                _ => Kind::Other,
            };
            let nested = matches!(kind, Kind::Media | Kind::Supports | Kind::Keyframes);
            let body = block_body(chunk).filter(|_| nested && depth < MAX_DEPTH);
            let kind = if nested && body.is_none() { Kind::Other } else { kind };
            let decl = match (&a.body, kind) {
                (bare_css::parser::AtBody::Declarations(ds), Kind::FontFace) => Some(declaration_text(ds)),
                _ => None,
            };
            let o = new_rule(d, it, Rule { kind, prelude: a.prelude.to_owned(), ..base }, decl)?;
            if let Some(body) = body {
                let kids = chunk_entries(d, it, chunk, body)?;
                let list = new_rule_list(d, it, o, depth + 1)?;
                state(d).borrow_mut().lists.get_mut(&list).unwrap().items = kids;
                state(d).borrow_mut().rules.get_mut(&o).unwrap().list = Some(list);
            }
            o
        }
    }))
}
fn new_rule_list<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, depth: usize) -> R<E, E::Obj> {
    let p = state(d).borrow().rule_list_p;
    let o = d.new_source(it, crate::browser_sources::Source::RuleList, Some(p))?;
    state(d).borrow_mut().lists.insert(
        o,
        List {
            sheet: None,
            rule: Some(owner),
            items: vec![],
            stamp: None,
            depth,
        },
    );
    Ok(o)
}
#[cold]
fn refresh<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
    if is_ctor(d, o) {
        // The text is the source of truth. Find the rules once after each `replaceSync`.
        if !state(d).borrow().sheets[&o].stale {
            return Ok(());
        }
        let rules = {
            let s = state(d).borrow();
            let text = &s.sheets[&o].source;
            chunk_entries(d, it, text, text)?
        };
        let mut st = state(d).borrow_mut();
        let sh = st.sheets.get_mut(&o).unwrap();
        sh.stale = false;
        sh.edits.bytes = rules.iter().map(|e| match *e {
            Entry::Chunk(a, b) => (b - a) as usize + 1,
            Entry::Obj(_) => unreachable!(),
        }).sum();
        let list = sh.list;
        st.lists.get_mut(&list).unwrap().items = rules;
        return Ok(());
    }
    {
        // An edited `<style>` sheet: its own text is in use while the element text is the same. A new element text gives a new sheet text.
        let doc = d.doc.borrow();
        let mut st = state(d).borrow_mut();
        let sh = st.sheets.get_mut(&o).unwrap();
        if let Some(key) = &sh.owned {
            if *key == text_key(&doc, sh.node) {
                return Ok(());
            }
            sh.owned = None;
            sh.edits = SheetEdits::default();
        }
    }
    let text = source(d, it, o)?;
    let unchanged = {
        let doc = d.doc.borrow();
        let s = state(d).borrow();
        let sh = &s.sheets[&o];
        sh.source == text && sh.text_key.as_ref().is_none_or(|key| *key == text_key(&doc, sh.node))
    };
    if unchanged { return Ok(()); }
    let rules = chunk_entries(d, it, &text, &text)?;
    let key = pending_key(d, &d.doc.borrow(), node(d, o), &rules);
    let mut st = state(d).borrow_mut();
    let sh = st.sheets.get_mut(&o).unwrap();
    sh.source = text;
    sh.text_key = key;
    let list = sh.list;
    st.lists.get_mut(&list).unwrap().items = rules;
    Ok(())
}
// The items of the rule list of a sheet.
fn items<E: Eng>(s: &State<E>, sheet: E::Obj) -> &Vec<Entry<E>> {
    &s.lists[&s.sheets[&sheet].list].items
}
fn index<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, usize> {
    let v = arg_s::<E>(s, a, i);
    let n = s.to_number(v)?;
    Ok(if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as usize } else { 0 })
}
// Assemble the same text that eager publication would have stored. Reading a rule object
// does not change that text. Its original span stays live until an edit serializes it.
fn flush_chunks<E: Eng>(sh: &mut Sheet<E>, list: &mut List<E>, rules: &mut IdMap<E::Obj, Rule<E>>) {
    if !sh.edits.dirty {
        return;
    }
    let mut text = String::with_capacity(sh.edits.bytes);
    for entry in &mut list.items {
        #[cfg(test)]
        WORK.with(|w| {
            let (parsed, published) = w.get();
            w.set((parsed, published + 1));
        });
        let (a, b) = match *entry {
            Entry::Chunk(a, b) => (a, b),
            Entry::Obj(o) => rules[&o].sheet_span,
        };
        let start = text.len() as u32;
        text.push_str(&sh.source[a as usize..b as usize]);
        let end = text.len() as u32;
        match entry {
            Entry::Chunk(a, b) => (*a, *b) = (start, end),
            Entry::Obj(o) => rules.get_mut(o).unwrap().sheet_span = (start, end),
        }
        text.push('\n');
    }
    sh.source = text;
    sh.edits.dirty = false;
}

// Publish any pending spans before copying them for serialization, which can run
// script through a declaration getter and reenter the style planner.
fn published_items<E: Eng>(d: &Dom<E>, o: E::Obj) -> Vec<Entry<E>> {
    let mut st = state(d).borrow_mut();
    let State { sheets, lists, rules, .. } = &mut *st;
    let sh = sheets.get_mut(&o).unwrap();
    let list = lists.get_mut(&sh.list).unwrap();
    flush_chunks(sh, list, rules);
    list.items.clone()
}

fn chunks_changed<E: Eng>(d: &Dom<E>, st: &mut State<E>, o: E::Obj) {
    let sh = st.sheets.get_mut(&o).unwrap();
    sh.edits.dirty = true;
    // Leave room for small edits. Compact after enough deletions to pay for the copy.
    if sh.source.capacity() > 2 * sh.edits.bytes + 4096 {
        flush_chunks(sh, st.lists.get_mut(&sh.list).unwrap(), &mut st.rules);
    }
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
}

fn insert<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet(d, s, t)?;
    if a.is_empty() {
        return Err(s.type_error("insertRule requires a rule"));
    }
    let text = d.arg_str_s(s, a, 0)?;
    let i = index::<E>(s, a, 1)?;
    old::<E, _>(s, |it| refresh(d, it, o))?;
    let (len, ctor) = {
        let st = state(d).borrow();
        let sh = &st.sheets[&o];
        (st.lists[&sh.list].items.len(), in_place(sh))
    };
    if i > len {
        return Err(d.dom_err_s(s, "IndexSizeError", "Rule index is out of range"));
    }
    // TMP-FELA probe.
    if len % 1000 == 0 && len > 0 { eprintln!("TMP-FELA len={len} at={i} ctor={ctor} text={}", text.chars().take(90).collect::<String>()); }
    let parsed = parse_cssom(&text);
    if parsed.len() != 1 {
        return Err(d.dom_err_s(s, "SyntaxError", "Expected one CSS rule"));
    }
    if let bare_css::parser::Rule::Style(r) = &parsed[0] {
        old::<E, _>(s, |it| d.selectors(it, r.prelude))?;
    }
    // The new rule is a span of its text like the rules that no script has read: its object is made when a script reads its index.
    // Keep the chunk as a span. If it covers the whole input, the parse above already validated it.
    if text.len() > MAX_TEXT {
        return Err(d.lim_s(s, "cssom-text"));
    }
    let Some(chunk) = chunks(&text).find(|c| c.len() == text.len() || !parse_cssom(c).is_empty()) else {
        return Err(d.dom_err_s(s, "SyntaxError", "Expected one CSS rule"));
    };
    s.charge(chunk.len() as u64)?;
    // In place: each rule object keeps the span of its last text (`make_rule`, `publish_rules`), and an edit of a rule publishes the sheet again.
    // So a new rule is one span, and no rule is serialized again. A CSS-in-JS library inserts rules one by one: n inserts cost O(n), not O(n²).
    if ctor {
        let mut st = state(d).borrow_mut();
        let sh = st.sheets.get_mut(&o).unwrap();
        let bytes = sh.edits.bytes + chunk.len() + 1;
        if bytes > MAX_TEXT {
            return Err(d.lim_s(s, "cssom-text"));
        }
        let start = sh.source.len() as u32;
        sh.source.push_str(chunk);
        let end = sh.source.len() as u32;
        sh.edits.bytes = bytes;
        let list = sh.list;
        st.lists.get_mut(&list).unwrap().items.insert(i, Entry::Chunk(start, end));
        chunks_changed(d, &mut st, o);
        return Ok(s.number(i as f64));
    }
    let mut next = published_items(d, o);
    let at = {
        let mut st = state(d).borrow_mut();
        let source = &mut st.sheets.get_mut(&o).unwrap().source;
        source.push_str(chunk);
        source.len() - chunk.len()
    };
    next.insert(i, Entry::Chunk(at as u32, (at + chunk.len()) as u32));
    if let Err(e) = old::<E, _>(s, |it| publish_rules(d, it, o, next)) {
        state(d).borrow_mut().sheets.get_mut(&o).unwrap().source.truncate(at);
        return Err(e);
    }
    Ok(s.number(i as f64))
}
fn delete<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet(d, s, t)?;
    if a.is_empty() {
        return Err(s.type_error("deleteRule requires an index"));
    }
    let i = index::<E>(s, a, 0)?;
    old::<E, _>(s, |it| refresh(d, it, o))?;
    let (entry, ctor) = {
        let st = state(d).borrow();
        let sh = &st.sheets[&o];
        (st.lists[&sh.list].items.get(i).copied(), in_place(sh))
    };
    let Some(entry) = entry else {
        return Err(d.dom_err_s(s, "IndexSizeError", "Rule index is out of range"));
    };
    if ctor {
        let mut st = state(d).borrow_mut();
        let (a, b) = match entry {
            Entry::Chunk(a, b) => (a, b),
            Entry::Obj(r) => st.rules[&r].sheet_span,
        };
        let sh = st.sheets.get_mut(&o).unwrap();
        sh.edits.bytes -= (b - a) as usize + 1;
        sh.edits.objects -= u32::from(matches!(entry, Entry::Obj(_)));
        if b as usize == sh.source.len() {
            sh.source.truncate(a as usize);
        }
        let list = sh.list;
        st.lists.get_mut(&list).unwrap().items.remove(i);
        chunks_changed(d, &mut st, o);
        return Ok(s.undefined());
    }
    let mut next = published_items(d, o);
    next.remove(i);
    old::<E, _>(s, |it| publish_rules(d, it, o, next))?;
    Ok(s.undefined())
}
fn rule_string<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, String> {
    style_of(d, it, o)?;
    let r = state(d).borrow().rules[&o].clone();
    match (r.selector, r.style) {
        (Some(sel), Some(style)) => {
            let k = d.key(it, "cssText")?;
            let v = it.get_value(E::obj_raw(style), k)?;
            Ok(format!("{} {{ {} }}", sel, d.string_arg(it, v)?))
        }
        (None, Some(style)) if matches!(r.kind, Kind::Keyframe | Kind::FontFace) => {
            let k = d.key(it, "cssText")?;
            let v = it.get_value(E::obj_raw(style), k)?;
            let head = if r.kind == Kind::Keyframe { r.prelude.as_str() } else { "@font-face" };
            Ok(format!("{head} {{ {} }}", d.string_arg(it, v)?))
        }
        _ if r.list.is_some() => {
            // The text is built from the nested rules, so an edit of a nested rule reaches the sheet text.
            let kids = state(d).borrow().lists[&r.list.unwrap()].items.clone();
            let mut parts = Vec::with_capacity(kids.len());
            for k in kids {
                parts.push(match k {
                    Entry::Obj(k) => rule_string(d, it, k)?,
                    Entry::Chunk(a, b) => r.raw[a as usize..b as usize].to_owned(),
                });
            }
            let at = r.raw.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("@media").to_ascii_lowercase();
            Ok(format!("{at} {} {{ {} }}", r.prelude, parts.join(" ")))
        }
        _ => Ok(r.raw),
    }
}
fn publish<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
    let rules = published_items(d, o);
    publish_rules(d, it, o, rules)
}
// The sheet text is the text of its rules in order. A rule that no script has read keeps its own text, so it needs no object.
fn publish_rules<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj, rules: Vec<Entry<E>>) -> R<E, ()> {
    let (ctor, node, list) = {
        let s = state(d).borrow();
        let sh = &s.sheets[&o];
        (sh.ctor, sh.node, sh.list)
    };
    let tag = if ctor { String::new() } else { name_of(&d.doc.borrow(), node) };
    if !ctor && tag != "style" {
        return Err(Dom::gate("editing an external stylesheet through CSSOM"));
    }
    let mut text = String::new();
    let mut next = Vec::with_capacity(rules.len());
    let mut spans = Vec::new();
    for e in rules {
        #[cfg(test)]
        WORK.with(|w| {
            let (parsed, published) = w.get();
            w.set((parsed, published + 1));
        });
        let start = text.len();
        match e {
            Entry::Obj(r) => {
                let t = rule_string(d, it, r)?;
                if text.len() + t.len() + 1 > MAX_TEXT {
                    return Err(d.lim("cssom-text"));
                }
                text.push_str(&t);
                spans.push((r, (start as u32, text.len() as u32)));
                next.push(e);
            }
            Entry::Chunk(a, b) => {
                if text.len() + (b - a) as usize + 1 > MAX_TEXT {
                    return Err(d.lim("cssom-text"));
                }
                text.push_str(&state(d).borrow().sheets[&o].source[a as usize..b as usize]);
                next.push(Entry::Chunk(start as u32, text.len() as u32));
            }
        }
        text.push('\n');
    }
    // The sheet is the text in use (`owned`, `owned_text`): the cascade reads it at the next style read. The element text stays.
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    let (key, owned) = {
        let doc = d.doc.borrow();
        if ctor { (None, None) } else { (pending_key(d, &doc, node, &next), Some(text_key(&doc, node))) }
    };
    let mut s = state(d).borrow_mut();
    let sh = s.sheets.get_mut(&o).unwrap();
    if sh.owned.is_none() {
        sh.owned = owned;
    }
    sh.edits = SheetEdits { bytes: text.len(), objects: spans.len() as u32, dirty: false };
    for (r, span) in spans {
        s.rules.get_mut(&r).unwrap().sheet_span = span;
    }
    let sheet = s.sheets.get_mut(&o).unwrap();
    sheet.source = text;
    sheet.text_key = key;
    s.lists.get_mut(&list).unwrap().items = next;
    Ok(())
}
fn selector_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule(d, s, t)?;
    let text = d.arg_str_s(s, a, 0)?;
    match d.selectors(s.engine(), &text) {
        Ok(_) => {}
        Err(Fault::Throw(_)) => return Ok(s.undefined()),
        Err(e) => return Err(lift_err::<E>(s, e)),
    }
    let sh = {
        let mut st = state(d).borrow_mut();
        let r = st.rules.get_mut(&o).unwrap();
        r.selector = Some(text);
        r.sheet
    };
    old::<E, _>(s, |it| publish(d, it, sh))?;
    Ok(s.undefined())
}
// The sheet that holds the rule, when the rule is still in it (not deleted, no deleted ancestor).
fn attached<E: Eng>(d: &Dom<E>, mut o: E::Obj) -> Option<E::Obj> {
    let s = state(d).borrow();
    loop {
        let r = &s.rules[&o];
        match r.parent {
            Some(p) => {
                if !s.lists[&s.rules[&p].list?].items.contains(&Entry::Obj(o)) {
                    return None;
                }
                o = p;
            }
            None => return items(&s, r.sheet).contains(&Entry::Obj(o)).then_some(r.sheet),
        }
    }
}
// The grouping interface: `cssRules`, `insertRule` and `deleteRule` of `@media` and `@supports`.
fn group<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>) -> Res<'s, (E::Obj, E::Obj)> {
    let o = rule(d, s, t)?;
    let list = state(d).borrow().rules[&o].list;
    match list {
        Some(l) => Ok((o, l)),
        None => Err(s.type_error("Illegal invocation")),
    }
}
fn republish<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
    match attached(d, o) {
        Some(sheet) => publish(d, it, sheet),
        None => Ok(()),
    }
}
fn group_insert<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, list) = group(d, s, t)?;
    if a.is_empty() {
        return Err(s.type_error("insertRule requires a rule"));
    }
    let text = d.arg_str_s(s, a, 0)?;
    let i = index::<E>(s, a, 1)?;
    if i > state(d).borrow().lists[&list].items.len() {
        return Err(d.dom_err_s(s, "IndexSizeError", "Rule index is out of range"));
    }
    let parsed = parse_cssom(&text);
    if parsed.len() != 1 {
        return Err(d.dom_err_s(s, "SyntaxError", "Expected one CSS rule"));
    }
    if let bare_css::parser::Rule::Style(r) = &parsed[0] {
        old::<E, _>(s, |it| d.selectors(it, r.prelude))?;
    }
    let sheet = state(d).borrow().rules[&o].sheet;
    let rules = old::<E, _>(s, |it| parsed_rules(d, it, sheet, Some(o), &text, 1))?;
    state(d).borrow_mut().lists.get_mut(&list).unwrap().items.insert(i, Entry::Obj(rules[0]));
    old::<E, _>(s, |it| republish(d, it, o))?;
    Ok(s.number(i as f64))
}
fn group_delete<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, list) = group(d, s, t)?;
    if a.is_empty() {
        return Err(s.type_error("deleteRule requires an index"));
    }
    let i = index::<E>(s, a, 0)?;
    if i >= state(d).borrow().lists[&list].items.len() {
        return Err(d.dom_err_s(s, "IndexSizeError", "Rule index is out of range"));
    }
    state(d).borrow_mut().lists.get_mut(&list).unwrap().items.remove(i);
    old::<E, _>(s, |it| republish(d, it, o))?;
    Ok(s.undefined())
}
// `@import` prelude: the URL (a string or `url()`) and the media list after it.
fn import_parts(p: &str) -> (&str, &str) {
    let p = p.trim();
    let (href, rest) = match p.as_bytes().first() {
        Some(&q @ (b'"' | b'\'')) => match p[1..].find(q as char) {
            Some(e) => (&p[1..1 + e], &p[e + 2..]),
            None => (&p[1..], ""),
        },
        _ if p.get(..4).is_some_and(|h| h.eq_ignore_ascii_case("url(")) => match p[4..].find(')') {
            Some(e) => (p[4..4 + e].trim().trim_matches(['"', '\'']), &p[5 + e..]),
            None => (p[4..].trim(), ""),
        },
        _ => ("", p),
    };
    (href, rest.trim())
}
// Deviation: an imported sheet is flattened by the loader, so it has no sheet object here.
fn key_text_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule(d, s, t)?;
    let text = d.arg_str_s(s, a, 0)?;
    state(d).borrow_mut().rules.get_mut(&o).unwrap().prelude = text;
    old::<E, _>(s, |it| republish(d, it, o))?;
    Ok(s.undefined())
}
// A write through a rule declaration (`browser_style`: a named setter, `setProperty`, `removeProperty`, `cssText`) reaches the sheet text.
pub(crate) fn declaration_written<E: Eng>(d: &Dom<E>, it: &mut E::Vm, style: E::Raw) -> R<E, ()> {
    let (Some(cell), Val::Object(o)) = (d.cssom.state.get(), E::kind(style)) else { return Ok(()) };
    let sheet = {
        let s = cell.borrow();
        s.style_rules.get(&o).map(|r| s.rules[r].sheet)
    };
    match sheet {
        Some(sh) => publish(d, it, sh),
        None => Ok(()),
    }
}
// ---------------------------------------------------------------------------------------- constructable style sheets
// `new CSSStyleSheet()`, `replaceSync`, `replace` and `adoptedStyleSheets` of the document and of a shadow root. The sheet keeps its text.
// The cascade reads that text (`adopted`, called by `Dom::plan`) and adds it to the rules of the tree that adopted it, after the
// `<style>` elements of that tree. The rules (JS objects) are parsed only when `cssRules`, `insertRule` or `deleteRule` ask for them.
// Not built: the `media`, `disabled` and `baseURL` options, `@import` in the text (the spec ignores it too), a sheet shared between
// documents, and a live `adoptedStyleSheets` array (the getter returns a copy: `push` on it is lost; assign a new array).
fn constructed<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, what: &str) -> Res<'s, E::Obj> {
    let o = sheet(d, s, t)?;
    if !is_ctor(d, o) {
        return Err(d.dom_err_s(s, "NotAllowedError", &format!("Can't call {what} on a non-constructed CSSStyleSheet.")));
    }
    Ok(o)
}
fn set_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: E::Obj, text: String) -> Res<'s, ()> {
    if text.len() > MAX_TEXT {
        return Err(d.lim_s(s, "cssom-text"));
    }
    s.charge(text.len() as u64 / 32)?;
    let mut st = state(d).borrow_mut();
    let sh = st.sheets.get_mut(&o).unwrap();
    sh.source = text;
    sh.stale = true;
    sh.edits = SheetEdits::default();
    let list = sh.list;
    st.lists.get_mut(&list).unwrap().items.clear();
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(())
}
fn replace_sync<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = constructed(d, s, t, "replaceSync")?;
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'replaceSync' on 'CSSStyleSheet': 1 argument required, but only 0 present."));
    }
    let text = d.arg_str_s(s, a, 0)?;
    set_text(d, s, o, text)?;
    Ok(s.undefined())
}
// Deviation: the text applies at once, not in a later task.
fn replace<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    match constructed(d, s, t, "replace").and_then(|o| Ok((o, d.arg_str_s(s, a, 0)?))) {
        Ok((o, text)) => {
            set_text(d, s, o, text)?;
            let sheet = d.obj_local(s, o);
            s.resolve_promise(p, sheet)?;
        }
        Err(bare_js_api::Error::Throw(v)) => s.reject_promise(p, v)?,
        Err(e) => return Err(e),
    }
    Ok(p)
}
// The tree that owns the list: the document (its root) or a shadow root.
fn adopter<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.nd_s(s, this)?;
    let doc = d.doc.borrow();
    if n == doc.root() || matches!(doc.data(n), bare_html::dom::NodeData::ShadowRoot) {
        Ok(n)
    } else {
        Err(s.type_error("Illegal invocation"))
    }
}
fn adopted_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = adopter(d, s, this)?;
    old::<E, _>(s, |it| ensure(d, it))?;
    let items: Vec<E::Obj> = state(d).borrow().adopted.get(&n).into_iter().flatten().copied().collect();
    let items: Vec<Local<'s>> = items.into_iter().map(|o| d.obj_local(s, o)).collect();
    s.new_array(&items)
}
fn adopted_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = adopter(d, s, this)?;
    old::<E, _>(s, |it| ensure(d, it))?;
    let list = arg_s::<E>(s, a, 0);
    if !is_object::<E>(s, list) {
        return Err(s.type_error("Failed to set the 'adoptedStyleSheets' property: The provided value is not an object."));
    }
    let len = s.get_str(list, "length")?;
    let len = s.to_number(len)?;
    let len = if len.is_finite() { len.max(0.0) as usize } else { 0 };
    s.charge(len as u64)?;
    let mut next = Vec::with_capacity(len);
    for i in 0..len {
        let v = s.get_str(list, &i.to_string())?;
        match obj_id::<E>(s, v) {
            Some(o) if state(d).borrow().sheets.contains_key(&o) => {
                if !is_ctor(d, o) {
                    return Err(d.dom_err_s(s, "NotAllowedError", "Failed to set the 'adoptedStyleSheets' property: Can't adopt non-constructed stylesheets."));
                }
                next.push(o);
            }
            _ => return Err(s.type_error("Failed to set the 'adoptedStyleSheets' property: Failed to convert value to 'CSSStyleSheet'.")),
        }
    }
    {
        let mut st = state(d).borrow_mut();
        if next.is_empty() {
            st.adopted.remove(&n);
        } else {
            st.adopted.insert(n, next);
            d.wrapper_edges.set(true);
        }
    }
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(s.undefined())
}
/// The adopted sheets for the cascade: the tree (the document root or a shadow root) and the text, trees in node order.
pub(crate) fn adopted<E: Eng>(d: &Dom<E>, out: &mut Vec<(NodeId, String)>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let mut s = cell.borrow_mut();
    let State { adopted, sheets, lists, rules, .. } = &mut *s;
    let mut roots: Vec<_> = adopted.keys().copied().collect();
    roots.sort_by_key(|n| n.index());
    for r in roots {
        for o in &adopted[&r] {
            let sh = sheets.get_mut(o).unwrap();
            if sh.edits.dirty {
                flush_chunks(sh, lists.get_mut(&sh.list).unwrap(), rules);
            }
            out.push((r, sh.source.clone()));
        }
    }
}
fn stamp<E: Eng>(d: &Dom<E>) -> (u64, u64, u64) {
    let doc = d.doc.borrow();
    let own = d.cssom.own.get();
    (doc.revision() - own.0, doc.text_revision() - own.1, d.computed.sheet_gen.get())
}
#[cold]
pub(crate) fn fill<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, index: u32) -> R<E, bool> {
    let Some(cell) = d.cssom.state.get() else { return Ok(false) };
    let Some((sheet, rule, old)) = cell.borrow().lists.get(&owner).map(|l| (l.sheet, l.rule, l.stamp)) else {
        return Ok(false);
    };
    if old != Some(stamp(d)) {
        if let Some(sh) = sheet {
            refresh(d, it, sh)?;
        } else if rule.is_none() {
            let root = d.doc.borrow().root();
            let (mut cur, mut depth) = (root, 0);
            let mut nodes = Vec::new();
            loop {
                let next = {
                    let doc = d.doc.borrow();
                    next_in(&doc, root, cur, depth)
                };
                let Some((n, dep)) = next else { break };
                cur = n;
                depth = dep;
                it.charge_o(1)?;
                let doc = d.doc.borrow();
                let tag = name_of(&doc, n);
                if tag == "style"
                    || tag == "link"
                        && attr(&doc, n, "rel").is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")))
                        && attr(&doc, n, "href").is_some_and(|h| d.computed.ext.borrow().contains_key(h))
                {
                    nodes.push(n);
                }
            }
            let mut items = Vec::new();
            for n in nodes {
                items.push(Entry::Obj(sheet_of(d, it, n)?));
            }
            state(d).borrow_mut().lists.get_mut(&owner).unwrap().items = items;
        }
        state(d).borrow_mut().lists.get_mut(&owner).unwrap().stamp = Some(stamp(d));
    }
    if index != LENGTH_ONLY {
        make_rule(d, it, owner, index as usize)?;
    }
    Ok(true)
}
// The object of item `i` of a rule list, when it is a span of text still.
fn make_rule<E: Eng>(d: &Dom<E>, it: &mut E::Vm, list: E::Obj, i: usize) -> R<E, ()> {
    let (sheet, parent, depth, chunk, span) = {
        let s = state(d).borrow();
        let l = &s.lists[&list];
        let Some(&Entry::Chunk(a, b)) = l.items.get(i) else { return Ok(()) };
        let (sheet, parent, text) = match (l.sheet, l.rule) {
            (Some(sh), _) => (sh, None, &s.sheets[&sh].source),
            (None, Some(r)) => (s.rules[&r].sheet, Some(r), &s.rules[&r].raw),
            (None, None) => return Ok(()),
        };
        (sheet, parent, l.depth, text[a as usize..b as usize].to_owned(), (a, b))
    };
    let o = rule_from_chunk(d, it, sheet, parent, &chunk, depth)?.ok_or_else(|| it.type_error_o("The text of a CSS rule changed"))?;
    let mut st = state(d).borrow_mut();
    st.lists.get_mut(&list).unwrap().items[i] = Entry::Obj(o);
    if parent.is_none() && in_place(&st.sheets[&sheet]) {
        st.sheets.get_mut(&sheet).unwrap().edits.objects += 1;
        st.rules.get_mut(&o).unwrap().sheet_span = span;
    }
    Ok(())
}
pub(crate) fn len<E: Eng>(d: &Dom<E>, o: E::Obj) -> Option<Result<usize, Pending>> {
    let s = d.cssom.state.get()?.borrow();
    let l = s.lists.get(&o)?;
    Some(if l.stamp != Some(stamp(d)) {
        Err(Pending::Fill(LENGTH_ONLY))
    } else {
        Ok(l.items.len())
    })
}
pub(crate) fn item<E: Eng>(d: &Dom<E>, o: E::Obj, i: usize) -> Option<Result<Slot<E>, Pending>> {
    let s = d.cssom.state.get()?.borrow();
    let l = s.lists.get(&o)?;
    Some(if l.stamp != Some(stamp(d)) {
        Err(Pending::Fill(i.min(u32::MAX as usize - 1) as u32))
    } else {
        Ok(match l.items.get(i) {
            Some(&Entry::Obj(r)) => Slot::Ready(r),
            Some(Entry::Chunk(..)) => Slot::Missing,
            None => Slot::Absent,
        })
    })
}

// ---------------------------------------------------------------------------------------- handlers of the interface
// The getters and the plain methods read `this` and the arguments in place. A handler whose body is a long chain of helpers of the
// old form (`insertRule`, `deleteRule`, `adoptedStyleSheets`, `replace`, ...) stays on the old registry.
fn sheet_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
    if let Some(o) = obj_id::<E>(s, v)
        && d.cssom.state.get().is_some_and(|st| st.borrow().sheets.contains_key(&o))
    {
        return Ok(o);
    }
    Err(illegal_s::<E>(s))
}
fn rule_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
    if let Some(o) = obj_id::<E>(s, v)
        && d.cssom.state.get().is_some_and(|st| st.borrow().rules.contains_key(&o))
    {
        return Ok(o);
    }
    Err(illegal_s::<E>(s))
}
fn group_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>) -> Res<'s, (E::Obj, E::Obj)> {
    let o = rule_s(d, s, t)?;
    let list = state(d).borrow().rules[&o].list;
    match list {
        Some(l) => Ok((o, l)),
        None => Err(illegal_s::<E>(s)),
    }
}
fn sheet_ctor_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = state(d).borrow().sheet_p;
    let default = d.obj_local(s, sp);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype");
    let (o, list) = old::<E, _>(s, |it| {
        let o = d.new_owned(it, Source::Sheet, proto)?;
        Ok((o, new_list(d, it, Some(o))?))
    })?;
    let node = d.doc.borrow().root();
    state(d).borrow_mut().sheets.insert(o, Sheet { node, list, source: String::new(), text_key: None, ctor: true, stale: false, edits: SheetEdits::default(), owned: None });
    Ok(d.obj_local(s, o))
}
fn sheet_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let loaded = {
        let doc = d.doc.borrow();
        let tag = name_of(&doc, n);
        tag == "style"
            || tag == "link"
                && attr(&doc, n, "rel").is_some_and(|v| v.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")))
                && attr(&doc, n, "href").is_some_and(|h| d.computed.ext.borrow().contains_key(h))
    };
    if !loaded || !old::<E, _>(s, |it| has_style_root(d, it, n))? {
        return Ok(s.null());
    }
    let o = old::<E, _>(s, |it| sheet_of(d, it, n))?;
    Ok(d.obj_local(s, o))
}
fn owner_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet_s(d, s, t)?;
    if is_ctor(d, o) {
        return Ok(s.null());
    }
    d.wrap_s(s, node(d, o))
}
fn href_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet_s(d, s, t)?;
    if is_ctor(d, o) {
        return Ok(s.null());
    }
    let n = node(d, o);
    let h = {
        let doc = d.doc.borrow();
        if name_of(&doc, n) == "style" { None } else { attr(&doc, n, "href").map(str::to_owned) }
    };
    match h {
        None => Ok(s.null()),
        Some(h) => {
            let url = resolve(d, &h);
            d.js_str_s(s, url.as_deref().unwrap_or(&h))
        }
    }
}
fn disabled_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet_s(d, s, t)?;
    let on = !is_ctor(d, o) && attr(&d.doc.borrow(), node(d, o), "disabled").is_some();
    Ok(s.boolean(on))
}
fn rules_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = sheet_s(d, s, t)?;
    old::<E, _>(s, |it| refresh(d, it, o))?;
    let l = state(d).borrow().sheets[&o].list;
    Ok(d.obj_local(s, l))
}
fn selector_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let text = state(d).borrow().rules[&o].selector.clone().unwrap_or_default();
    d.js_str_s(s, &text)
}
fn rule_text_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let text = old::<E, _>(s, |it| rule_string(d, it, o))?;
    d.js_str_s(s, &text)
}
fn style_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    match old::<E, _>(s, |it| style_of(d, it, o))? {
        Some(st) => Ok(d.obj_local(s, st)),
        None => Ok(s.undefined()),
    }
}
fn type_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let n = {
        let st = state(d).borrow();
        let r = &st.rules[&o];
        match r.kind {
            Kind::Style => 1,
            Kind::Import => 3,
            Kind::Media => 4,
            Kind::FontFace => 5,
            Kind::Keyframes => 7,
            Kind::Keyframe => 8,
            Kind::Supports => 12,
            Kind::Other => match r.raw.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
                "@page" => 6,
                "@namespace" => 10,
                _ => 0,
            },
        }
    };
    Ok(s.number(n as f64))
}
fn parent_rule_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let p = {
        let st = state(d).borrow();
        match st.rules[&o].parent {
            Some(p) if st.lists[&st.rules[&p].list.unwrap()].items.contains(&Entry::Obj(o)) => Some(p),
            _ => None,
        }
    };
    Ok(match p {
        Some(p) => d.obj_local(s, p),
        None => s.null(),
    })
}
fn parent_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    Ok(match attached(d, o) {
        Some(sh) => d.obj_local(s, sh),
        None => s.null(),
    })
}
fn group_rules_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let l = group_s(d, s, t)?.1;
    Ok(d.obj_local(s, l))
}
fn condition_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let text = state(d).borrow().rules[&o].prelude.clone();
    d.js_str_s(s, &text)
}
fn media_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let known = state(d).borrow().rules[&o].media;
    if let Some(m) = known {
        return Ok(d.obj_local(s, m));
    }
    let p = state(d).borrow().media_p;
    let m = old::<E, _>(s, |it| d.new_owned(it, Source::Media, p))?;
    let mut st = state(d).borrow_mut();
    st.medias.insert(m, o);
    st.rules.get_mut(&o).unwrap().media = Some(m);
    drop(st);
    Ok(d.obj_local(s, m))
}
fn media_text_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let r = match obj_id::<E>(s, t) {
        Some(m) => d.cssom.state.get().and_then(|st| st.borrow().medias.get(&m).copied()),
        None => None,
    };
    let Some(r) = r else { return Err(illegal_s::<E>(s)) };
    let (kind, prelude) = {
        let st = state(d).borrow();
        (st.rules[&r].kind, st.rules[&r].prelude.clone())
    };
    let text = if kind == Kind::Import { import_parts(&prelude).1.to_owned() } else { prelude };
    d.js_str_s(s, &text)
}
fn import_href_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = rule_s(d, s, t)?;
    let prelude = state(d).borrow().rules[&o].prelude.clone();
    let h = import_parts(&prelude).0;
    let url = resolve(d, h);
    d.js_str_s(s, url.as_deref().unwrap_or(h))
}
// Deviation: an imported sheet is flattened by the loader, so it has no sheet object here.
fn import_sheet_get_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rule_s(d, s, t)?;
    Ok(s.null())
}
fn list_length_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id::<E>(s, t) else { return Err(illegal_s::<E>(s)) };
    if !old::<E, _>(s, |it| fill(d, it, o, LENGTH_ONLY))? {
        return Err(illegal_s::<E>(s));
    }
    let n = state(d).borrow().lists[&o].items.len();
    Ok(s.number(n as f64))
}
fn list_item_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id::<E>(s, t) else { return Err(illegal_s::<E>(s)) };
    if a.is_empty() {
        return Err(s.type_error("item requires an index"));
    }
    let v = arg_s::<E>(s, a, 0);
    let i = to_u32_s::<E>(s, v)? as usize;
    if !old::<E, _>(s, |it| fill(d, it, o, i.min(u32::MAX as usize - 1) as u32))? {
        return Err(illegal_s::<E>(s));
    }
    let r = match state(d).borrow().lists[&o].items.get(i) {
        Some(&Entry::Obj(r)) => Some(r),
        _ => None,
    };
    Ok(match r {
        Some(r) => d.obj_local(s, r),
        None => s.null(),
    })
}

fn attr<'a>(doc: &'a Document, n: NodeId, key: &str) -> Option<&'a str> {
    doc.attr(n, doc.find_atom(key)?)
}
fn resolve<E: Eng>(d: &Dom<E>, text: &str) -> Option<String> {
    crate::browser_navigation_url::parse(text, crate::browser_elements::base(d).as_ref())
        .ok()
        .map(|r| r.href())
}

// Token spans preserve unknown at-rules, strings, comments and escaped braces without a second CSS lexer.
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    use bare_css::tokenizer::{Token, tokenize};
    let mut t = tokenize(text);
    std::iter::from_fn(move || {
        let (mut start, mut depth) = (None, 0usize);
        while let Some((token, a, b)) = t.next_spanned() {
            if start.is_none() && matches!(token, Token::Whitespace | Token::Cdo | Token::Cdc) {
                continue;
            }
            start.get_or_insert(a);
            match token {
                Token::OpenCurly | Token::OpenSquare | Token::OpenParen | Token::Function(_) => depth += 1,
                Token::CloseCurly | Token::CloseSquare | Token::CloseParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && matches!(token, Token::CloseCurly) {
                        return Some(&text[start.unwrap()..b]);
                    }
                }
                Token::Semicolon if depth == 0 => return Some(&text[start.unwrap()..b]),
                _ => {}
            }
        }
        start.map(|a| &text[a..])
    })
}

fn name_of(doc: &Document, n: NodeId) -> String {
    if doc.element_ns(n) == bare_html::dom::Ns::Html {
        doc.element_name(n).to_owned()
    } else {
        String::new()
    }
}

// ------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The nodes that sheets, and the sheets that a document or a shadow root adopted, name.
pub(crate) fn held<E: Eng>(d: &Dom<E>, out: &mut Vec<NodeId>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let s = cell.borrow();
    out.extend(s.sheets.values().filter(|sh| !sh.ctor).map(|sh| sh.node));
    out.extend(s.sheets.values().flat_map(|sh| sh.text_key.iter().chain(&sh.owned)).flatten().map(|&(n, _)| n));
    out.extend(s.by_node.keys().copied());
    out.extend(s.adopted.keys().copied());
}

/// The spans of the text keys of sheets: the reclaim moves them when it builds the text list again.
pub(crate) fn pins<E: Eng>(d: &Dom<E>, f: &mut dyn FnMut(&mut bare_html::dom::Span)) {
    let Some(cell) = d.cssom.state.get() else { return };
    for sh in cell.borrow_mut().sheets.values_mut() {
        let Sheet { text_key, owned, .. } = sh;
        for (_, sp) in text_key.iter_mut().chain(owned.iter_mut()).flatten() {
            f(sp);
        }
    }
}

//! Bounded CSSOM objects. Sheet and rule lists are live host-indexed objects. All allocation is deferred until a CSSOM read.
//! Inline rule edits update the style text through the DOM. The existing sheet pipeline reads that text.
//! External rules require a supplied sheet and a same-origin URL. `CSSRule` is the base interface of every rule object. The interface shape and the
//! semantics follow Ladybird `LibWeb/CSS/CSSRule.cpp` (BSD-2).
use crate::browser::*;
use crate::browser_dom::{arg, attr_set, next_in, set_text_content, text_content};
use bare_html::dom::{Document, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

type V = R<Value>;
const MAX_OBJECTS: usize = 65536;
const MAX_TEXT: usize = 4 << 20;
#[derive(Clone)]
struct Sheet {
    node: NodeId,
    list: ObjId,
    rules: Vec<ObjId>,
    source: String,
    text_key: Option<Vec<(NodeId, bare_html::dom::Span)>>,
    // `new CSSStyleSheet()`: no owner node (`node` is the document root, unused), the text is in `source`. `stale`: the rules are not parsed yet.
    ctor: bool,
    stale: bool,
}
// The interface of a rule. The value is the index of its prototype in `State::protos`.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Other,
    Style,
}
#[derive(Clone)]
struct Rule {
    sheet: ObjId,
    kind: Kind,
    selector: Option<String>,
    raw: String,
    style: Option<ObjId>,
    parent: Option<ObjId>,
}
#[derive(Clone)]
struct List {
    sheet: Option<ObjId>,
    items: Vec<ObjId>,
    stamp: Option<(u64, u64, u64)>,
}
pub(crate) struct State {
    roots: Vec<ObjId>,
    sheet_p: ObjId,
    // The prototypes of the rule interfaces, indexed by `Kind`.
    protos: Vec<ObjId>,
    list_p: ObjId,
    rule_list_p: ObjId,
    doc_list: Option<ObjId>,
    sheets: IdMap<ObjId, Sheet>,
    by_node: HashMap<NodeId, ObjId>,
    rules: IdMap<ObjId, Rule>,
    lists: IdMap<ObjId, List>,
    // A rule declaration is an ordinary detached declaration. Only its setters and methods get wrappers.
    wrappers: HashMap<u32, ObjId>,
    wrapped: IdMap<ObjId, ObjId>,
    style_rules: IdMap<ObjId, ObjId>,
    // The constructed sheets adopted by the document (key: its root) or a shadow root, in order.
    adopted: HashMap<NodeId, Vec<ObjId>>,
}
#[derive(Default)]
pub(crate) struct Cssom {
    state: OnceCell<RefCell<State>>,
    style_p: OnceCell<ObjId>,
}
fn state(d: &Dom) -> &RefCell<State> {
    d.cssom.state.get().expect("CSSOM initialized")
}
pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.cssom.style_p.get().copied().map(Value::Object));
    if let Some(s) = d.cssom.state.get() {
        out.extend(s.borrow().roots.iter().copied().map(Value::Object));
    }
}
fn keep(d: &Dom, o: ObjId) -> R<ObjId> {
    let mut s = state(d).borrow_mut();
    if s.roots.len() >= MAX_OBJECTS {
        return Err(d.lim("cssom-objects"));
    }
    s.roots.push(o);
    Ok(o)
}
fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Illegal constructor"))
}
#[cold]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.cssom.state.get().is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let (sp, lp, rlp, base) = (d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?);
    let rp = d.obj(it, base)?;
    d.cssom
        .state
        .set(RefCell::new(State {
            roots: vec![sp, base, rp, lp, rlp],
            sheet_p: sp,
            protos: vec![base, rp],
            list_p: lp,
            rule_list_p: rlp,
            doc_list: None,
            sheets: IdMap::default(),
            by_node: HashMap::new(),
            rules: IdMap::default(),
            lists: IdMap::default(),
            wrappers: HashMap::new(),
            wrapped: IdMap::default(),
            style_rules: IdMap::default(),
            adopted: HashMap::new(),
        }))
        .ok();
    let mut ctors: HashMap<ObjId, ObjId> = HashMap::new();
    for (name, p, parent) in [
        ("CSSStyleSheet", sp, None),
        ("CSSRule", base, None),
        ("CSSStyleRule", rp, Some(base)),
        ("StyleSheetList", lp, None),
        ("CSSRuleList", rlp, None),
    ] {
        crate::browser_lazy::tag(d, it, p, name)?;
        let f = if name == "CSSStyleSheet" { d.add_ctor_nt(it, sheet_ctor, name, 0)? } else { d.add_fn(it, illegal, name, 0, false)? };
        keep(d, f)?;
        ctors.insert(p, f);
        if let Some(pf) = parent.and_then(|pp| ctors.get(&pp).copied()) {
            it.heap.set_prototype_of(f, Some(pf)).map_err(|e| it.heap_error(e))?;
        }
        let (kp, kc, k) = (d.key(it, "prototype")?, d.key(it, "constructor")?, d.key(it, name)?);
        it.define_data(f, kp, Value::Object(p), false, false, false)?;
        it.define_data(p, kc, Value::Object(f), true, false, true)?;
        let own = it.heap.peek_own_property(it.global_object(), k).map_err(|e| it.heap_error(e))?;
        if own.is_none() || matches!(own, Some(bare_js::heap::Property::Accessor { .. })) {
            it.define_data(it.global_object(), k, Value::Object(f), true, false, true)?;
        }
    }
    d.def_get(it, sp, "cssRules", rules_get, None)?;
    d.def_get(it, sp, "ownerNode", owner_get, None)?;
    d.def_get(it, sp, "href", href_get, None)?;
    d.def_get(it, sp, "disabled", disabled_get, Some(disabled_set))?;
    d.def_method(it, sp, "insertRule", 1, insert)?;
    d.def_method(it, sp, "deleteRule", 1, delete)?;
    d.def_method(it, sp, "replaceSync", 1, replace_sync)?;
    d.def_method(it, sp, "replace", 1, replace)?;
    d.def_get(it, rp, "selectorText", selector_get, Some(selector_set))?;
    d.def_get(it, rp, "style", style_get, None)?;
    d.def_get(it, base, "cssText", rule_text, None)?;
    d.def_get(it, base, "type", type_get, None)?;
    d.def_get(it, base, "parentStyleSheet", parent_get, None)?;
    d.def_get(it, base, "parentRule", parent_rule_get, None)?;
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
            it.define_data(o, k, Value::Number(n as f64), false, true, false)?;
        }
    }
    // The lists are array-like: their `[Symbol.iterator]` is the one of `Array.prototype`, as in browsers.
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let values = it.get_value(Value::Object(it.array_prototype()), sym)?;
    for p in [lp, rlp] {
        d.def_get(it, p, "length", list_length, None)?;
        d.def_method(it, p, "item", 1, list_item)?;
        it.define_data(p, sym, values, true, false, true)?;
    }
    Ok(())
}
// Resolve only through the existing cold host-property miss path.
#[cold]
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<bool> {
    match name {
        "CSSStyleSheet" | "CSSRule" | "CSSStyleRule" | "CSSRuleList" | "StyleSheetList" => {
            ensure(d, it)?;
            Ok(true)
        }
        "adoptedStyleSheets" => {
            for p in [d.p().document, d.p().shadow] {
                let key = d.key(it, name)?;
                if it.heap.peek_own_property(p, key).map_err(|e| it.heap_error(e))?.is_none() {
                    d.def_get(it, p, name, adopted_get, Some(adopted_set))?;
                }
            }
            ensure(d, it)?;
            Ok(true)
        }
        "styleSheets" => {
            let key = d.key(it, name)?;
            if it.heap.peek_own_property(d.p().document, key).map_err(|e| it.heap_error(e))?.is_none() {
                d.def_get(it, d.p().document, name, document_list, None)?;
            }
            Ok(true)
        }
        "sheet" => {
            let p = style_proto(d, it)?;
            let key = d.key(it, name)?;
            if it.heap.peek_own_property(d.p().link, key).map_err(|e| it.heap_error(e))?.is_none() {
                d.def_get(it, d.p().link, name, sheet_get, None)?;
            }
            let nodes: Vec<_> = d.st.borrow().by_node.iter().map(|(&n, &o)| (n, o)).collect();
            it.charge(nodes.len() as u64)?;
            for (n, o) in nodes {
                if d.doc.borrow().is_html(n, "style") {
                    it.heap.set_prototype_of(o, Some(p)).map_err(|e| it.heap_error(e))?;
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn wrapper_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    if d.html_on.get() {
        crate::browser_html::proto(d, it, "style")
    } else {
        Ok(d.cssom.style_p.get().copied().unwrap_or(d.p().html))
    }
}

// Created only after a sheet or HTMLStyleElement read.
#[cold]
pub(crate) fn style_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    if let Some(&p) = d.cssom.style_p.get() {
        return Ok(p);
    }
    let p = d.obj(it, d.p().html)?;
    d.def_get(it, p, "sheet", sheet_get, None)?;
    d.cssom.style_p.set(p).ok();
    Ok(p)
}
fn new_list(d: &Dom, it: &mut Interp<'_>, sheet: Option<ObjId>) -> R<ObjId> {
    let p = {
        let s = state(d).borrow();
        if sheet.is_some() { s.rule_list_p } else { s.list_p }
    };
    let o = it.heap.new_indexed(Some(p)).map_err(|e| it.heap_error(e))?;
    keep(d, o)?;
    state(d).borrow_mut().lists.insert(
        o,
        List {
            sheet,
            items: vec![],
            stamp: None,
        },
    );
    Ok(o)
}
fn document_list(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    if d.nd(it, this)? != d.doc.borrow().root() {
        return Err(it.type_error("Illegal invocation"));
    }
    ensure(d, it)?;
    let old = state(d).borrow().doc_list;
    let o = match old {
        Some(o) => o,
        None => {
            let o = new_list(d, it, None)?;
            state(d).borrow_mut().doc_list = Some(o);
            o
        }
    };
    Ok(Value::Object(o))
}
fn sheet_of(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<ObjId> {
    ensure(d, it)?;
    if let Some(&o) = state(d).borrow().by_node.get(&n) {
        return Ok(o);
    }
    let p = state(d).borrow().sheet_p;
    let o = d.obj(it, p)?;
    keep(d, o)?;
    let owner = d.wrap(it, n)?;
    if let Value::Object(owner) = owner {
        keep(d, owner)?;
    }
    let list = new_list(d, it, Some(o))?;
    state(d).borrow_mut().sheets.insert(
        o,
        Sheet {
            node: n,
            list,
            rules: vec![],
            source: String::new(),
            text_key: None,
            ctor: false,
            stale: false,
        },
    );
    state(d).borrow_mut().by_node.insert(n, o);
    Ok(o)
}
fn sheet_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let loaded = {
        let doc = d.doc.borrow();
        let tag = name_of(&doc, n);
        tag == "style"
            || tag == "link"
                && attr(&doc, n, "rel").is_some_and(|v| v.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")))
                && attr(&doc, n, "href").is_some_and(|h| d.computed.ext.borrow().contains_key(h))
    };
    if !loaded || !d.in_document(it, n)? {
        return Ok(Value::Null);
    }
    sheet_of(d, it, n).map(Value::Object)
}
fn sheet(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<ObjId> {
    match v {
        Value::Object(o) if d.cssom.state.get().is_some_and(|s| s.borrow().sheets.contains_key(&o)) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}
fn rule(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<ObjId> {
    match v {
        Value::Object(o) if d.cssom.state.get().is_some_and(|s| s.borrow().rules.contains_key(&o)) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}
fn node(d: &Dom, o: ObjId) -> NodeId {
    state(d).borrow().sheets[&o].node
}
fn is_ctor(d: &Dom, o: ObjId) -> bool {
    state(d).borrow().sheets[&o].ctor
}
fn owner_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    if is_ctor(d, o) {
        return Ok(Value::Null);
    }
    d.wrap(it, node(d, o))
}
fn href_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    if is_ctor(d, o) {
        return Ok(Value::Null);
    }
    let n = node(d, o);
    let h = {
        let doc = d.doc.borrow();
        if name_of(&doc, n) == "style" {
            None
        } else {
            attr(&doc, n, "href").map(str::to_owned)
        }
    };
    match h {
        None => Ok(Value::Null),
        Some(h) => {
            let url = resolve(d, &h);
            d.js_str(it, url.as_deref().unwrap_or(&h))
        }
    }
}
fn disabled_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    Ok(Value::Bool(!is_ctor(d, o) && attr(&d.doc.borrow(), node(d, o), "disabled").is_some()))
}
fn disabled_set(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    if is_ctor(d, o) {
        return Ok(Value::Undefined);
    }
    let n = node(d, o);
    let on = it.to_js_bool(arg(a, 0))?;
    if on {
        attr_set(d, it, n, "disabled", "")?;
    } else {
        let v = d.wrap(it, n)?;
        let k = d.js_str(it, "disabled")?;
        crate::browser_dom::remove_attribute(d, it, v, &[k])?;
    }
    Ok(Value::Undefined)
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

fn pending_key(d: &Dom, doc: &bare_html::dom::Document, node: NodeId, rules: &[ObjId]) -> Option<Vec<(NodeId, bare_html::dom::Span)>> {
    if !d.style.has_pending.get() { return None; }
    let s = state(d).borrow();
    rules.iter().any(|r| s.rules[r].style.and_then(|o| d.style_object_inputs(doc, o)).is_some()).then(|| text_key(doc, node))
}

pub(crate) fn sheet_inputs(d: &Dom, doc: &bare_html::dom::Document, node: NodeId, text: &str) -> Option<bare_css::cascade::InputSheet> {
    if !d.style.has_pending.get() { return None; }
    let cell = d.cssom.state.get()?;
    let s = cell.borrow();
    let sheet = s.sheets.get(s.by_node.get(&node)?)?;
    if sheet.source != text || sheet.text_key.as_ref()? != &text_key(doc, node) { return None; }
    let blocks: Vec<_> = sheet.rules.iter().map(|r| s.rules[r].style.and_then(|o| d.style_object_inputs(doc, o))).collect();
    blocks.iter().any(Option::is_some).then_some(blocks)
}

fn source(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<String> {
    let n = node(d, o);
    let (tag, href) = {
        let doc = d.doc.borrow();
        (name_of(&doc, n), attr(&doc, n, "href").unwrap_or("").to_owned())
    };
    if tag == "style" {
        let v = d.wrap(it, n)?;
        let text = text_content(d, it, v, &[])?;
        return d.string_arg(it, text);
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
fn declaration(d: &Dom, it: &mut Interp<'_>, text: &str) -> R<ObjId> {
    let (v, css) = (d.js_str(it, "span")?, d.js_str(it, text)?);
    let doc = d.wrap(it, d.doc.borrow().root())?;
    let el = crate::browser_dom::create_element(d, it, doc, &[v])?;
    let key = d.key(it, "style")?;
    let Value::Object(style) = it.get_value(el, key)? else { unreachable!() };
    let k = d.key(it, "cssText")?;
    it.put_value(Value::Object(style), k, css)?;
    keep(d, style)?;
    Ok(style)
}
fn new_rule(d: &Dom, it: &mut Interp<'_>, sheet: ObjId, sel: Option<String>, raw: String, decl: Option<String>) -> R<ObjId> {
    let kind = if sel.is_some() { Kind::Style } else { Kind::Other };
    let p = state(d).borrow().protos[kind as usize];
    let o = d.obj(it, p)?;
    keep(d, o)?;
    let style = match decl {
        Some(t) => Some(declaration(d, it, &t)?),
        None => None,
    };
    state(d).borrow_mut().rules.insert(
        o,
        Rule {
            sheet,
            kind,
            selector: sel,
            raw,
            style,
            parent: None,
        },
    );
    if let Some(style) = style {
        wrap_declaration(d, it, o, style)?;
    }
    Ok(o)
}
fn parsed_rules(d: &Dom, it: &mut Interp<'_>, sheet: ObjId, text: &str) -> R<Vec<ObjId>> {
    if text.len() > MAX_TEXT {
        return Err(d.lim("cssom-text"));
    }
    it.charge(text.len() as u64)?;
    let mut out = Vec::new();
    for chunk in chunks(text) {
        let mut parsed = bare_css::parser::parse_stylesheet(chunk).into_iter();
        let Some(r) = parsed.next() else { continue };
        if out.len() >= MAX_OBJECTS {
            return Err(d.lim("cssom-rules"));
        }
        match r {
            bare_css::parser::Rule::Style(r) => {
                let mut decl = String::new();
                for p in r.declarations {
                    decl.push_str(p.name.as_ref());
                    decl.push(':');
                    decl.push_str(p.value);
                    if p.important {
                        decl.push_str(" !important");
                    }
                    decl.push(';');
                }
                out.push(new_rule(d, it, sheet, Some(r.prelude.to_owned()), String::new(), Some(decl))?);
            }
            _ => out.push(new_rule(d, it, sheet, None, chunk.to_owned(), None)?),
        }
    }
    Ok(out)
}
#[cold]
fn refresh(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    if is_ctor(d, o) {
        // The text is the source of truth. Parse it once after each `replaceSync`.
        if !state(d).borrow().sheets[&o].stale {
            return Ok(());
        }
        let text = state(d).borrow().sheets[&o].source.clone();
        let rules = parsed_rules(d, it, o, &text)?;
        let mut st = state(d).borrow_mut();
        let sh = st.sheets.get_mut(&o).unwrap();
        sh.rules = rules.clone();
        sh.stale = false;
        let list = sh.list;
        st.lists.get_mut(&list).unwrap().items = rules;
        return Ok(());
    }
    let text = source(d, it, o)?;
    let unchanged = {
        let doc = d.doc.borrow();
        let s = state(d).borrow();
        let sh = &s.sheets[&o];
        sh.source == text && sh.text_key.as_ref().is_none_or(|key| *key == text_key(&doc, sh.node))
    };
    if unchanged { return Ok(()); }
    let rules = parsed_rules(d, it, o, &text)?;
    let key = pending_key(d, &d.doc.borrow(), node(d, o), &rules);
    let mut st = state(d).borrow_mut();
    let sh = st.sheets.get_mut(&o).unwrap();
    sh.rules = rules.clone();
    sh.source = text;
    sh.text_key = key;
    let list = sh.list;
    st.lists.get_mut(&list).unwrap().items = rules;
    Ok(())
}
fn rules_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    refresh(d, it, o)?;
    Ok(Value::Object(state(d).borrow().sheets[&o].list))
}
fn index(it: &mut Interp<'_>, a: &[Value], i: usize) -> R<usize> {
    let n = it.to_js_number(arg(a, i))?;
    Ok(if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as usize } else { 0 })
}
fn insert(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    if a.is_empty() {
        return Err(it.type_error("insertRule requires a rule"));
    }
    let text = d.string_arg(it, arg(a, 0))?;
    let i = index(it, a, 1)?;
    refresh(d, it, o)?;
    if i > state(d).borrow().sheets[&o].rules.len() {
        return Err(d.dom_err(it, "IndexSizeError", "Rule index is out of range"));
    }
    let parsed = bare_css::parser::parse_stylesheet(&text);
    if parsed.len() != 1 {
        return Err(d.dom_err(it, "SyntaxError", "Expected one CSS rule"));
    }
    if let bare_css::parser::Rule::Style(r) = &parsed[0] {
        d.selectors(it, r.prelude)?;
    }
    let rules = parsed_rules(d, it, o, &text)?;
    let mut next = state(d).borrow().sheets[&o].rules.clone();
    next.insert(i, rules[0]);
    publish_rules(d, it, o, next)?;
    Ok(Value::Number(i as f64))
}
fn delete(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let o = sheet(d, it, t)?;
    if a.is_empty() {
        return Err(it.type_error("deleteRule requires an index"));
    }
    let i = index(it, a, 0)?;
    refresh(d, it, o)?;
    if i >= state(d).borrow().sheets[&o].rules.len() {
        return Err(d.dom_err(it, "IndexSizeError", "Rule index is out of range"));
    }
    let mut next = state(d).borrow().sheets[&o].rules.clone();
    next.remove(i);
    publish_rules(d, it, o, next)?;
    Ok(Value::Undefined)
}
fn rule_string(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<String> {
    let r = state(d).borrow().rules[&o].clone();
    match (r.selector, r.style) {
        (Some(sel), Some(style)) => {
            let k = d.key(it, "cssText")?;
            let v = it.get_value(Value::Object(style), k)?;
            Ok(format!("{} {{ {} }}", sel, d.string_arg(it, v)?))
        }
        _ => Ok(r.raw),
    }
}
fn publish(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    let rules = state(d).borrow().sheets[&o].rules.clone();
    publish_rules(d, it, o, rules)
}
fn publish_rules(d: &Dom, it: &mut Interp<'_>, o: ObjId, rules: Vec<ObjId>) -> R<()> {
    let mut sh = state(d).borrow().sheets[&o].clone();
    sh.rules = rules;
    let mut text = String::new();
    for &r in &sh.rules {
        let t = rule_string(d, it, r)?;
        if text.len() + t.len() + 1 > MAX_TEXT {
            return Err(d.lim("cssom-text"));
        }
        text.push_str(&t);
        text.push('\n');
    }
    let tag = if sh.ctor { String::new() } else { name_of(&d.doc.borrow(), sh.node) };
    if sh.ctor {
        d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    } else if tag == "style" {
        let v = d.wrap(it, sh.node)?;
        let s = d.js_str(it, &text)?;
        set_text_content(d, it, v, &[s])?;
    } else {
        return Err(Dom::gate("editing an external stylesheet through CSSOM"));
    }
    let key = if sh.ctor { None } else { pending_key(d, &d.doc.borrow(), sh.node, &sh.rules) };
    let mut s = state(d).borrow_mut();
    let sheet = s.sheets.get_mut(&o).unwrap();
    sheet.source = text;
    sheet.text_key = key;
    sheet.rules = sh.rules.clone();
    s.lists.get_mut(&sh.list).unwrap().items = sh.rules;
    Ok(())
}
fn selector_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    let s = state(d).borrow().rules[&o].selector.clone().unwrap_or_default();
    d.js_str(it, &s)
}
fn selector_set(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let o = rule(d, it, t)?;
    let text = d.string_arg(it, arg(a, 0))?;
    match d.selectors(it, &text) {
        Ok(_) => {}
        Err(Error::Throw(_)) => return Ok(Value::Undefined),
        Err(e) => return Err(e),
    }
    let sh = {
        let mut s = state(d).borrow_mut();
        let r = s.rules.get_mut(&o).unwrap();
        r.selector = Some(text);
        r.sheet
    };
    publish(d, it, sh)?;
    Ok(Value::Undefined)
}
fn rule_text(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    let s = rule_string(d, it, o)?;
    d.js_str(it, &s)
}
fn style_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    Ok(state(d).borrow().rules[&o].style.map(Value::Object).unwrap_or(Value::Undefined))
}
fn type_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    let r = state(d).borrow().rules[&o].clone();
    let n = if r.kind == Kind::Style {
        1
    } else {
        match r.raw.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
            "@import" => 3,
            "@media" => 4,
            "@font-face" => 5,
            "@keyframes" | "@-webkit-keyframes" => 7,
            "@supports" => 12,
            _ => 0,
        }
    };
    Ok(Value::Number(n as f64))
}
fn parent_rule_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    Ok(state(d).borrow().rules[&o].parent.map(Value::Object).unwrap_or(Value::Null))
}
fn parent_get(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let o = rule(d, it, t)?;
    let s = state(d).borrow();
    let sh = s.rules[&o].sheet;
    Ok(if s.sheets[&sh].rules.contains(&o) { Value::Object(sh) } else { Value::Null })
}
fn wrap_declaration(d: &Dom, it: &mut Interp<'_>, rule: ObjId, style: ObjId) -> R<()> {
    use bare_js::heap::{Property, PropertyDescriptor};
    let p = *d.style.proto.get().expect("style installed");
    let keys = it.heap.own_keys(p).map_err(|e| it.heap_error(e))?;
    state(d).borrow_mut().style_rules.insert(style, rule);
    let (set, remove) = (d.key(it, "setProperty")?, d.key(it, "removeProperty")?);
    for key in keys {
        let Some(prop) = it.heap.get_own_property(p, key).map_err(|e| it.heap_error(e))? else {
            continue;
        };
        let original = match prop {
            Property::Accessor { set: Some(f), .. } => f,
            Property::Data { value: Value::Object(f), .. } if key == set || key == remove => f,
            _ => continue,
        };
        let cached = state(d).borrow().wrapped.get(&original).copied();
        let f = if let Some(f) = cached {
            f
        } else {
            let id = d.handlers.borrow().len() as u32;
            let f = d.add_fn(it, declaration_write, "CSSOM declaration update", 0, false)?;
            state(d).borrow_mut().wrappers.insert(id, original);
            state(d).borrow_mut().wrapped.insert(original, f);
            keep(d, f)?;
            f
        };
        let desc = match prop {
            Property::Accessor { get, .. } => PropertyDescriptor {
                get: Some(get),
                set: Some(Some(f)),
                enumerable: Some(true),
                configurable: Some(true),
                ..Default::default()
            },
            _ => PropertyDescriptor {
                value: Some(Value::Object(f)),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..Default::default()
            },
        };
        it.heap.define_own_property(style, key, &desc).map_err(|e| it.heap_error(e))?;
    }
    Ok(())
}
fn declaration_write(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let id = d.style.cur.get();
    let original = state(d).borrow().wrappers[&id];
    let rule = match t {
        Value::Object(o) => state(d).borrow().style_rules.get(&o).copied(),
        _ => None,
    }
    .ok_or_else(|| it.type_error("Illegal invocation"))?;
    let v = it.call_scoped(Value::Object(original), t, a, &[])?;
    let sh = state(d).borrow().rules[&rule].sheet;
    publish(d, it, sh)?;
    Ok(v)
}
// ---------------------------------------------------------------------------------------- constructable style sheets
// `new CSSStyleSheet()`, `replaceSync`, `replace` and `adoptedStyleSheets` of the document and of a shadow root. The sheet keeps its text.
// The cascade reads that text (`adopted`, called by `Dom::plan`) and adds it to the rules of the tree that adopted it, after the
// `<style>` elements of that tree. The rules (JS objects) are parsed only when `cssRules`, `insertRule` or `deleteRule` ask for them.
// Not built: the `media`, `disabled` and `baseURL` options, `@import` in the text (the spec ignores it too), a sheet shared between
// documents, and a live `adoptedStyleSheets` array (the getter returns a copy: `push` on it is lost; assign a new array).
fn sheet_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let sp = state(d).borrow().sheet_p;
    let proto = it.host_proto_from_new_target(sp)?;
    let o = d.obj(it, proto)?;
    keep(d, o)?;
    let list = new_list(d, it, Some(o))?;
    let node = d.doc.borrow().root();
    state(d).borrow_mut().sheets.insert(o, Sheet { node, list, rules: vec![], source: String::new(), text_key: None, ctor: true, stale: false });
    Ok(Value::Object(o))
}
fn constructed(d: &Dom, it: &mut Interp<'_>, t: Value, what: &str) -> R<ObjId> {
    let o = sheet(d, it, t)?;
    if !is_ctor(d, o) {
        return Err(d.dom_err(it, "NotAllowedError", &format!("Can't call {what} on a non-constructed CSSStyleSheet.")));
    }
    Ok(o)
}
fn set_text(d: &Dom, it: &mut Interp<'_>, o: ObjId, text: String) -> R<()> {
    if text.len() > MAX_TEXT {
        return Err(d.lim("cssom-text"));
    }
    it.charge(text.len() as u64 / 32)?;
    let mut st = state(d).borrow_mut();
    let sh = st.sheets.get_mut(&o).unwrap();
    sh.source = text;
    sh.rules.clear();
    sh.stale = true;
    let list = sh.list;
    st.lists.get_mut(&list).unwrap().items.clear();
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(())
}
fn replace_sync(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let o = constructed(d, it, t, "replaceSync")?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'replaceSync' on 'CSSStyleSheet': 1 argument required, but only 0 present."));
    }
    let text = d.string_arg(it, arg(a, 0))?;
    set_text(d, it, o, text)?;
    Ok(Value::Undefined)
}
// Deviation: the text applies at once, not in a later task.
fn replace(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let p = it.host_new_promise()?;
    match constructed(d, it, t, "replace").and_then(|o| Ok((o, d.string_arg(it, arg(a, 0))?))) {
        Ok((o, text)) => {
            set_text(d, it, o, text)?;
            it.host_resolve_promise(p, Value::Object(o))?;
        }
        Err(Error::Throw(v)) => it.host_reject_promise(p, v)?,
        Err(e) => return Err(e),
    }
    Ok(p)
}
// The tree that owns the list: the document (its root) or a shadow root.
fn adopter(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    if n == doc.root() || matches!(doc.data(n), bare_html::dom::NodeData::ShadowRoot) {
        Ok(n)
    } else {
        Err(it.type_error("Illegal invocation"))
    }
}
fn adopted_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = adopter(d, it, this)?;
    ensure(d, it)?;
    let items: Vec<Value> = state(d).borrow().adopted.get(&n).into_iter().flatten().map(|&o| Value::Object(o)).collect();
    it.array_from(&items)
}
fn adopted_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = adopter(d, it, this)?;
    ensure(d, it)?;
    let list = arg(a, 0);
    if !matches!(list, Value::Object(_)) {
        return Err(it.type_error("Failed to set the 'adoptedStyleSheets' property: The provided value is not an object."));
    }
    let k = d.key(it, "length")?;
    let len = it.get_value(list, k)?;
    let len = it.to_js_number(len)?;
    let len = if len.is_finite() { len.max(0.0) as usize } else { 0 };
    if len > MAX_OBJECTS {
        return Err(d.lim("cssom-objects"));
    }
    it.charge(len as u64)?;
    let mut next = Vec::with_capacity(len);
    for i in 0..len {
        let k = d.key(it, &i.to_string())?;
        let v = it.get_value(list, k)?;
        match v {
            Value::Object(o) if state(d).borrow().sheets.contains_key(&o) => {
                if !is_ctor(d, o) {
                    return Err(d.dom_err(it, "NotAllowedError", "Failed to set the 'adoptedStyleSheets' property: Can't adopt non-constructed stylesheets."));
                }
                next.push(o);
            }
            _ => return Err(it.type_error("Failed to set the 'adoptedStyleSheets' property: Failed to convert value to 'CSSStyleSheet'.")),
        }
    }
    {
        let mut st = state(d).borrow_mut();
        if next.is_empty() {
            st.adopted.remove(&n);
        } else {
            st.adopted.insert(n, next);
        }
    }
    d.computed.sheet_gen.set(d.computed.sheet_gen.get() + 1);
    Ok(Value::Undefined)
}
/// The adopted sheets for the cascade: the tree (the document root or a shadow root) and the text, trees in node order.
pub(crate) fn adopted(d: &Dom, out: &mut Vec<(NodeId, String)>) {
    let Some(cell) = d.cssom.state.get() else { return };
    let s = cell.borrow();
    let mut roots: Vec<_> = s.adopted.keys().copied().collect();
    roots.sort_by_key(|n| n.index());
    for r in roots {
        out.extend(s.adopted[&r].iter().map(|o| (r, s.sheets[o].source.clone())));
    }
}
fn stamp(d: &Dom) -> (u64, u64, u64) {
    let doc = d.doc.borrow();
    (doc.revision(), doc.text_revision(), d.computed.sheet_gen.get())
}
#[cold]
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId) -> R<bool> {
    let Some(cell) = d.cssom.state.get() else { return Ok(false) };
    let Some(list) = cell.borrow().lists.get(&owner).cloned() else {
        return Ok(false);
    };
    if let Some(sh) = list.sheet {
        refresh(d, it, sh)?;
    } else {
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
            it.charge(1)?;
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
            items.push(sheet_of(d, it, n)?);
        }
        state(d).borrow_mut().lists.get_mut(&owner).unwrap().items = items;
    }
    state(d).borrow_mut().lists.get_mut(&owner).unwrap().stamp = Some(stamp(d));
    Ok(true)
}
pub(crate) fn len(d: &Dom, o: ObjId) -> Option<Result<usize, HeapError>> {
    let s = d.cssom.state.get()?.borrow();
    let l = s.lists.get(&o)?;
    Some(if l.stamp != Some(stamp(d)) {
        Err(HeapError::NeedsHost { owner: o, index: 0 })
    } else {
        Ok(l.items.len())
    })
}
pub(crate) fn item(d: &Dom, o: ObjId, i: usize) -> Option<Result<Slot, HeapError>> {
    let s = d.cssom.state.get()?.borrow();
    let l = s.lists.get(&o)?;
    Some(if l.stamp != Some(stamp(d)) {
        Err(HeapError::NeedsHost { owner: o, index: i as u32 })
    } else {
        Ok(l.items.get(i).copied().map(Slot::Ready).unwrap_or(Slot::Absent))
    })
}
fn list_length(d: &Dom, it: &mut Interp<'_>, t: Value, _: &[Value]) -> V {
    let Value::Object(o) = t else {
        return Err(it.type_error("Illegal invocation"));
    };
    if !fill(d, it, o)? {
        return Err(it.type_error("Illegal invocation"));
    }
    Ok(Value::Number(state(d).borrow().lists[&o].items.len() as f64))
}
fn list_item(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value]) -> V {
    let Value::Object(o) = t else {
        return Err(it.type_error("Illegal invocation"));
    };
    if a.is_empty() {
        return Err(it.type_error("item requires an index"));
    }
    let i = index(it, a, 0)?;
    if !fill(d, it, o)? {
        return Err(it.type_error("Illegal invocation"));
    }
    Ok(state(d).borrow().lists[&o].items.get(i).copied().map(Value::Object).unwrap_or(Value::Null))
}

fn attr<'a>(doc: &'a Document, n: NodeId, key: &str) -> Option<&'a str> {
    doc.attr(n, doc.find_atom(key)?)
}
fn resolve(d: &Dom, text: &str) -> Option<String> {
    crate::browser_navigation_url::parse(text, crate::browser_elements::base(d).as_ref())
        .ok()
        .map(|r| r.href())
}

// Token spans preserve unknown at-rules, strings, comments and escaped braces without a second CSS lexer.
fn chunks(text: &str) -> Vec<&str> {
    use bare_css::tokenizer::{Token, tokenize};
    let mut t = tokenize(text);
    let (mut start, mut depth) = (None, 0usize);
    let mut out = Vec::new();
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
                    out.push(&text[start.take().unwrap()..b]);
                }
            }
            Token::Semicolon if depth == 0 => {
                out.push(&text[start.take().unwrap()..b]);
            }
            _ => {}
        }
    }
    if let Some(a) = start {
        out.push(&text[a..]);
    }
    out
}

fn name_of(doc: &Document, n: NodeId) -> String {
    if doc.element_ns(n) == bare_html::dom::Ns::Html {
        doc.element_name(n).to_owned()
    } else {
        String::new()
    }
}

//! SVG attribute reads and shape metrics. Matrices use the untransformed identity.
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, Local};
use bare_html::dom::{NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

fn node<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().element_ns(n) != Ns::Svg {
        return Err(s.type_error("Illegal invocation"));
    }
    Ok(n)
}
fn num(d: &Dom, n: NodeId, name: &str) -> f64 {
    {
        let doc = d.doc.borrow();
        doc.find_atom(name)
            .and_then(|a| doc.attr(n, a))
            .and_then(|s| s.trim().strip_suffix("px").unwrap_or(s.trim()).parse().ok())
            .unwrap_or(0.0)
    }
}
fn object<'s>(_d: &Dom, s: &mut Sc<'s, '_>, fields: &[(&str, f64)]) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    for &(name, v) in fields {
        let v = s.number(v);
        s.define_str(o, name, v, Attrs::ALL)?;
    }
    Ok(o)
}
/// The box of an element of an `<svg>` that layout made no box for (a shape, a `g`, a `text`): the bounds of what it draws, through the
/// `transform` of each element above it and the `viewBox`, in the layout. `None` for an `<svg>` (it has fragments), an element that
/// draws nothing (a `defs`, a `g` with no shape, `display: none`), or one that is not in a drawn `<svg>`.
pub(crate) fn layout_box(g: &crate::browser_scroll::Geo, n: NodeId) -> Option<bare_layout::fragment::Rect> {
    let doc = g.doc;
    if doc.element_ns(n) != Ns::Svg {
        return None;
    }
    g.styles.style(n)?;
    let root = std::iter::successors(Some(n), |&c| doc.parent(c).filter(|&p| doc.element_ns(p) == Ns::Svg)).last()?;
    let f = g.layout.fragments.iter().find(|f| f.node == root && f.kind == bare_layout::fragment::Kind::Box)?;
    let area = bare_paint::display::svg_area(f, g.styles.style(root)?);
    let [x0, y0, x1, y1] = bare_paint::svg::pixel_box::<SoftwareMath>(doc, g.styles, root, area, n)?;
    Some(bare_layout::fragment::Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}
/// `getBBox()`: the bounds of what the element draws, in its own user space (before its `transform`).
fn bbox<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = node(d, s, this)?;
    let styles = old(s, |it| d.cascade_now(it))?;
    let [x0, y0, x1, y1] = bare_paint::svg::user_box::<SoftwareMath>(&d.doc.borrow(), &styles, n).unwrap_or([0.0; 4]);
    let f = |v: f32| v as f64;
    object(d, s, &[("x", f(x0)), ("y", f(y0)), ("width", f(x1 - x0)), ("height", f(y1 - y0))])
}
fn ctm<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    node(d, s, this)?;
    old_v(s, |it| crate::browser_matrix::rw_ctor(d, it, Value::Undefined, &[]))
}
fn animated<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = node(d, s, this)?;
    let v = if name == "viewBox" {
        let text = {
            let doc = d.doc.borrow();
            doc.find_atom(name)
                .and_then(|a| doc.attr(n, a))
                .unwrap_or("")
                .to_string()
        };
        let a: Vec<f64> = text
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter_map(|st| st.parse().ok())
            .collect();
        object(
            d,
            s,
            &[
                ("x", *a.first().unwrap_or(&0.0)),
                ("y", *a.get(1).unwrap_or(&0.0)),
                ("width", *a.get(2).unwrap_or(&0.0)),
                ("height", *a.get(3).unwrap_or(&0.0)),
            ],
        )?
    } else {
        object(d, s, &[("value", num(d, n, name))])?
    };
    let o = s.new_object()?;
    for name in ["baseVal", "animVal"] {
        s.define_str(o, name, v, Attrs { writable: false, enumerable: true, configurable: true })?;
    }
    Ok(o)
}
#[cold]
#[inline(never)]
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<bool> {
    let p = d.p().svg;
    if let Some(i) = name
        .starts_with("SVG")
        .then(|| CLASSES.iter().position(|&(n, _)| n == name))
        .flatten()
    {
        enable_classes(d, it)?;
        let (_, f) = interface(d, it, i)?;
        let k = d.key(it, name)?;
        it.define_data(it.global_object(), k, Value::Object(f), true, false, true)?;
        return Ok(true);
    }
    match name {
        "SVGAnimatedString" => {
            let proto = d.obj(it, it.object_prototype())?;
            let f = crate::browser_custom::interface_ctor(d, it, name, false)?;
            let kp = d.key(it, "prototype")?;
            it.define_data(f, kp, Value::Object(proto), false, false, false)?;
            let kc = d.key(it, "constructor")?;
            it.define_data(proto, kc, Value::Object(f), true, false, true)?;
            let k = d.key(it, name)?;
            it.define_data(it.global_object(), k, Value::Object(f), true, false, true)?;
            d.def_get_s(it, p, "className", class_name, None)?;
            d.fixed.borrow_mut().extend([proto, f]);
        }
        "getTotalLength" => {
            d.def_method_s(it, p, name, 0, |d, s, t, a| metrics(d, s, t, a, false))?;
        }
        "getPointAtLength" => {
            d.def_method_s(it, p, name, 1, |d, s, t, a| metrics(d, s, t, a, true))?;
        }
        "getBBox" => {
            d.def_method_s(it, p, name, 0, bbox)?;
        }
        "transform" => {
            // A miss of `transform` on any host object asks once more: install the getter only once.
            let k = d.key(it, name)?;
            if it.heap.peek_own_property(p, k).map_err(|e| it.heap_error(e))?.is_none() {
                let src = it.heap.intern_str(include_str!("shims/svgtransform.js")).map_err(|e| it.heap_error(e))?;
                let Value::Object(get) = it.eval_global(Value::String(src))? else { return Err(it.type_error("svgtransform.js")) };
                d.define_accessor(it, p, k, get, None)?;
            }
        }
        "getComputedTextLength" => {
            d.def_method_s(it, p, name, 0, |d, s, t, a| text_length(d, s, t, a, false))?;
        }
        "getSubStringLength" => {
            d.def_method_s(it, p, name, 2, |d, s, t, a| text_length(d, s, t, a, true))?;
        }
        "getCTM" | "getScreenCTM" => {
            d.def_method_s(it, p, name, 0, ctm)?;
        }
        "width" => d.def_get_s(it, p, name, |d, s, t, _| animated(d, s, t, "width"), None)?,
        "height" => d.def_get_s(
            it,
            p,
            name,
            |d, s, t, _| animated(d, s, t, "height"),
            None,
        )?,
        "viewBox" => d.def_get_s(
            it,
            p,
            name,
            |d, s, t, _| animated(d, s, t, "viewBox"),
            None,
        )?,
        _ => return Ok(false),
    }
    Ok(true)
}
/// `getComputedTextLength()` and `getSubStringLength(start, n)` of a `text` or a `tspan`: the advance of the letters in user units.
fn text_length<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, sub: bool) -> Res<'s, Local<'s>> {
    let n = node(d, s, this)?;
    // WebIDL `unsigned long`: NaN and infinity are 0, and -1 is the biggest number.
    let ulong = |s: &mut Sc<'s, '_>, i| -> Res<'s, usize> {
        let v = arg_s(s, a, i);
        let x = s.to_number(v)?;
        Ok(if x.is_finite() { x.trunc().rem_euclid(4294967296.0) as usize } else { 0 })
    };
    let (start, count) = if sub { (ulong(s, 0)?, ulong(s, 1)?) } else { (0, usize::MAX) };
    // The font of a `tspan` is the one of its `text` (a `tspan` has no style of its own here).
    let text = {
        let doc = d.doc.borrow();
        match doc.element_name(n) {
            "text" => n,
            "tspan" => match std::iter::successors(doc.parent(n), |&p| doc.parent(p)).find(|&p| doc.element_ns(p) == Ns::Svg && doc.element_name(p) == "text") {
                Some(p) => p,
                None => return Err(s.type_error("Illegal invocation")),
            },
            _ => return Err(s.type_error("Illegal invocation")),
        }
    };
    let Some(snap) = old(s, |it| d.snapshot_with(it, text, true))? else { return Ok(s.number(0.0)) };
    let m = bare_layout::font::Metrics::of(&snap.style);
    let width = bare_paint::svg::text_length(&d.doc.borrow(), text, n, &m, start, count);
    match width {
        Some(w) => Ok(s.number(w as f64)),
        None if start == 0 && !sub => Ok(s.number(0.0)),
        None => Err(d.dom_err_s(s, "IndexSizeError", "The start index is out of range")),
    }
}
fn metrics<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, point: bool) -> Res<'s, Local<'s>> {
    let n = node(d, s, this)?;
    let mut at = if point {
        {
            let v = arg_s(s, a, 0);
            s.to_number(v)?.max(0.0)
        }
    } else {
        f64::INFINITY
    };
    let curves = bare_paint::svg::geometry::<SoftwareMath>(&d.doc.borrow(), n);
    let mut length = 0.0;
    let mut last = (0.0, 0.0);
    for curve in curves {
        if let Some(&p) = curve.first() {
            last = (p.0 as f64, p.1 as f64);
        }
        for edge in curve.windows(2) {
            s.charge(1)?;
            let (p, q) = (
                (edge[0].0 as f64, edge[0].1 as f64),
                (edge[1].0 as f64, edge[1].1 as f64),
            );
            let distance = ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt();
            if point && at <= distance && distance > 0.0 {
                return object(
                    d,
                    s,
                    &[
                        ("x", p.0 + (q.0 - p.0) * at / distance),
                        ("y", p.1 + (q.1 - p.1) * at / distance),
                    ],
                );
            }
            length += distance;
            at -= distance;
            last = q;
        }
    }
    if point {
        object(d, s, &[("x", last.0), ("y", last.1)])
    } else {
        Ok(s.number(length))
    }
}

fn class_name<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = node(d, s, this)?;
    let ctor = old_v(s, |it| {
        let k = d.key(it, "SVGAnimatedString")?;
        let g = it.global_object();
        if it.heap.peek_own_property(g, k).map_err(|e| it.heap_error(e))?.is_none() {
            define(d, it, "SVGAnimatedString")?;
        }
        Ok(match it.heap.peek_own_property(g, k).map_err(|e| it.heap_error(e))? {
            Some(bare_js::heap::Property::Data { value, .. }) => value,
            _ => Value::Undefined,
        })
    })?;
    let proto = s.get_str(ctor, "prototype")?;
    let Some(p) = obj_id(s, proto) else {
        return Err(s.type_error("Invalid SVGAnimatedString prototype"));
    };
    let o = d.obj_s(s, p)?;
    let o = d.obj_local(s, o);
    let text = {
        let doc = d.doc.borrow();
        doc.find_atom("class")
            .and_then(|a| doc.attr(n, a))
            .unwrap_or("")
            .to_string()
    };
    let value = d.js_str_s(s, &text)?;
    for name in ["baseVal", "animVal"] {
        s.define_str(o, name, value, Attrs::ALL)?;
    }
    Ok(o)
}

#[cold]
#[inline(never)]
fn init_class_name(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_get_s(it, d.p().svg, "className", class_name, None)?;
    d.els.tagged.set(d.els.tagged.get() | 2);
    Ok(())
}

const CLASSES: &[(&str, usize)] = &[
    ("SVGGraphicsElement", usize::MAX),
    ("SVGGeometryElement", 0),
    ("SVGSVGElement", 0),
    ("SVGPathElement", 1),
    ("SVGRectElement", 1),
    ("SVGCircleElement", 1),
    ("SVGEllipseElement", 1),
    ("SVGLineElement", 1),
    ("SVGPolylineElement", 1),
    ("SVGPolygonElement", 1),
    ("SVGGElement", 0),
    ("SVGTextElement", 0),
    ("SVGAElement", 0),
];
#[cold]
#[inline(never)]
fn interface(d: &Dom, it: &mut Interp<'_>, i: usize) -> R<(ObjId, ObjId)> {
    const OFFSET: usize = crate::browser_html::INTERFACE_COUNT;
    if let Some(Some(pair)) = d.html_ifaces.borrow().get(OFFSET + i) {
        return Ok(*pair);
    }
    let (name, parent) = CLASSES[i];
    let (p, parent_ctor) = if parent == usize::MAX {
        let k = d.key(it, "SVGElement")?;
        (
            d.p().svg,
            match it
                .heap
                .peek_own_property(it.global_object(), k)
                .map_err(|e| it.heap_error(e))?
            {
                Some(bare_js::heap::Property::Data { value, .. }) => value,
                _ => Value::Undefined,
            },
        )
    } else {
        let (p, f) = interface(d, it, parent)?;
        (p, Value::Object(f))
    };
    let proto = d.obj(it, p)?;
    let f = crate::browser_custom::interface_ctor(d, it, name, false)?;
    if let Value::Object(parent) = parent_ctor {
        it.heap
            .set_prototype_of(f, Some(parent))
            .map_err(|e| it.heap_error(e))?;
    }
    let kp = d.key(it, "prototype")?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    let kc = d.key(it, "constructor")?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, name)?;
    d.fixed.borrow_mut().extend([proto, f]);
    let mut classes = d.html_ifaces.borrow_mut();
    if classes.len() <= OFFSET + i {
        classes.resize(OFFSET + i + 1, None);
    }
    classes[OFFSET + i] = Some((proto, f));
    Ok((proto, f))
}
#[inline]
pub(crate) fn wrapper_proto(d: &Dom, it: &mut Interp<'_>, tag: &str) -> R<ObjId> {
    let flags = d.els.tagged.get();
    if flags & 2 == 0 {
        init_class_name(d, it)?;
    }
    if flags & 4 == 0 {
        return Ok(d.p().svg);
    }
    let i = match tag {
        "svg" => 2,
        "path" => 3,
        "rect" => 4,
        "circle" => 5,
        "ellipse" => 6,
        "line" => 7,
        "polyline" => 8,
        "polygon" => 9,
        "g" => 10,
        "text" => 11,
        "a" => 12,
        _ => return Ok(d.p().svg),
    };
    interface(d, it, i).map(|v| v.0)
}

#[cold]
#[inline(never)]
fn enable_classes(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.els.tagged.get() & 4 != 0 {
        return Ok(());
    }
    d.els.tagged.set(d.els.tagged.get() | 4);
    let nodes: Vec<_> =
        d.st.borrow().wrappers().collect();
    it.charge(nodes.len() as u64)?;
    for (n, o) in nodes {
        let tag = {
            let doc = d.doc.borrow();
            if doc.element_ns(n) != Ns::Svg {
                continue;
            }
            doc.element_name(n).to_string()
        };
        if it.heap.get_prototype_of(o).map_err(|e| it.heap_error(e))? == Some(d.p().svg) {
            let p = wrapper_proto(d, it, &tag)?;
            it.heap
                .set_prototype_of(o, Some(p))
                .map_err(|e| it.heap_error(e))?;
        }
    }
    Ok(())
}

struct SoftwareMath;
impl bare_paint::svg::GeometryMath for SoftwareMath {
    fn sin(x: f32) -> f32 {
        bare_js::sin(x as f64) as f32
    }
    fn cos(x: f32) -> f32 {
        bare_js::cos(x as f64) as f32
    }
    fn sin_cos(x: f32) -> (f32, f32) {
        (Self::sin(x), Self::cos(x))
    }
    fn atan2(y: f32, x: f32) -> f32 {
        bare_js::atan2(y as f64, x as f64) as f32
    }
    fn acos(x: f32) -> f32 {
        bare_js::acos(x as f64) as f32
    }
    fn hypot(x: f32, y: f32) -> f32 {
        ((x as f64).powi(2) + (y as f64).powi(2)).sqrt() as f32
    }
}

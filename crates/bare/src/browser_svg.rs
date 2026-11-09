//! SVG attribute reads and shape metrics. Matrices use the untransformed identity.
use crate::browser::*;
use bare_html::dom::{NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

fn node(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    if d.doc.borrow().element_ns(n) != Ns::Svg {
        return Err(it.type_error("Illegal invocation"));
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
fn object(d: &Dom, it: &mut Interp<'_>, fields: &[(&str, f64)]) -> R<Value> {
    let o = d.obj(it, it.object_prototype())?;
    for &(name, v) in fields {
        let k = d.key(it, name)?;
        it.define_data(o, k, Value::Number(v), true, true, true)?;
    }
    Ok(Value::Object(o))
}
fn bbox(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = node(d, it, this)?;
    object(
        d,
        it,
        &[
            ("x", num(d, n, "x")),
            ("y", num(d, n, "y")),
            ("width", num(d, n, "width")),
            ("height", num(d, n, "height")),
        ],
    )
}
fn ctm(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    node(d, it, this)?;
    crate::browser_matrix::rw_ctor(d, it, Value::Undefined, &[])
}
fn animated(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str) -> R<Value> {
    let n = node(d, it, this)?;
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
            .filter_map(|s| s.parse().ok())
            .collect();
        object(
            d,
            it,
            &[
                ("x", *a.first().unwrap_or(&0.0)),
                ("y", *a.get(1).unwrap_or(&0.0)),
                ("width", *a.get(2).unwrap_or(&0.0)),
                ("height", *a.get(3).unwrap_or(&0.0)),
            ],
        )?
    } else {
        object(d, it, &[("value", num(d, n, name))])?
    };
    let o = d.obj(it, it.object_prototype())?;
    for name in ["baseVal", "animVal"] {
        let k = d.key(it, name)?;
        it.define_data(o, k, v, false, true, true)?;
    }
    Ok(Value::Object(o))
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
            d.def_get(it, p, "className", class_name, None)?;
            d.fixed.borrow_mut().extend([proto, f]);
        }
        "getTotalLength" => {
            d.def_method(it, p, name, 0, |d, it, t, a| metrics(d, it, t, a, false))?;
        }
        "getPointAtLength" => {
            d.def_method(it, p, name, 1, |d, it, t, a| metrics(d, it, t, a, true))?;
        }
        "getBBox" => {
            d.def_method(it, p, name, 0, bbox)?;
        }
        "getCTM" | "getScreenCTM" => {
            d.def_method(it, p, name, 0, ctm)?;
        }
        "width" => d.def_get(it, p, name, |d, it, t, _| animated(d, it, t, "width"), None)?,
        "height" => d.def_get(
            it,
            p,
            name,
            |d, it, t, _| animated(d, it, t, "height"),
            None,
        )?,
        "viewBox" => d.def_get(
            it,
            p,
            name,
            |d, it, t, _| animated(d, it, t, "viewBox"),
            None,
        )?,
        _ => return Ok(false),
    }
    Ok(true)
}
fn metrics(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], point: bool) -> R<Value> {
    let n = node(d, it, this)?;
    let mut at = if point {
        it.to_js_number(crate::browser_platform::arg(a, 0))?
            .max(0.0)
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
            it.charge(1)?;
            let (p, q) = (
                (edge[0].0 as f64, edge[0].1 as f64),
                (edge[1].0 as f64, edge[1].1 as f64),
            );
            let distance = ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt();
            if point && at <= distance && distance > 0.0 {
                return object(
                    d,
                    it,
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
        object(d, it, &[("x", last.0), ("y", last.1)])
    } else {
        Ok(Value::Number(length))
    }
}

fn class_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = node(d, it, this)?;
    let k = d.key(it, "SVGAnimatedString")?;
    if it
        .heap
        .peek_own_property(it.global_object(), k)
        .map_err(|e| it.heap_error(e))?
        .is_none()
    {
        define(d, it, "SVGAnimatedString")?;
    }
    let ctor = match it
        .heap
        .peek_own_property(it.global_object(), k)
        .map_err(|e| it.heap_error(e))?
    {
        Some(bare_js::heap::Property::Data { value, .. }) => value,
        _ => Value::Undefined,
    };
    let kp = d.key(it, "prototype")?;
    let Value::Object(p) = it.get_value(ctor, kp)? else {
        return Err(it.type_error("Invalid SVGAnimatedString prototype"));
    };
    let o = d.obj(it, p)?;
    let text = {
        let doc = d.doc.borrow();
        doc.find_atom("class")
            .and_then(|a| doc.attr(n, a))
            .unwrap_or("")
            .to_string()
    };
    let value = d.js_str(it, &text)?;
    for name in ["baseVal", "animVal"] {
        let k = d.key(it, name)?;
        it.define_data(o, k, value, true, true, true)?;
    }
    Ok(Value::Object(o))
}

#[cold]
#[inline(never)]
fn init_class_name(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_get(it, d.p().svg, "className", class_name, None)?;
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
        d.st.borrow()
            .by_node
            .iter()
            .map(|(&n, &o)| (n, o))
            .collect();
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

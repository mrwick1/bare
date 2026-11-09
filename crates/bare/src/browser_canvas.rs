//! A bounded compatibility context. Drawing does not change pixels.
use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, p: ObjId) -> R<()> {
    d.def_method(it, p, "getContext", 1, context)?;
    d.def_method(it, p, "toDataURL", 0, data_url)?;
    Ok(())
}

fn canvas(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    let n = d.el(it, this)?;
    if !d.doc.borrow().is_html(n, "canvas") {
        return Err(it.type_error("Illegal invocation"));
    }
    match this {
        Value::Object(o) => Ok(o),
        _ => unreachable!(),
    }
}

fn noop(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Undefined)
}

fn context(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let c = canvas(d, it, this)?;
    if d.string_arg(it, crate::browser_platform::arg(a, 0))? != "2d" {
        return Ok(Value::Null);
    }
    let key = d.key(it, "__bareCanvas2D")?;
    if let Value::Object(o) = it.get_value(this, key)? {
        return Ok(Value::Object(o));
    }
    let o = d.obj(it, it.object_prototype())?;
    for name in [
        "save",
        "restore",
        "reset",
        "scale",
        "rotate",
        "translate",
        "transform",
        "setTransform",
        "resetTransform",
        "clearRect",
        "fillRect",
        "strokeRect",
        "beginPath",
        "closePath",
        "moveTo",
        "lineTo",
        "bezierCurveTo",
        "quadraticCurveTo",
        "arc",
        "arcTo",
        "ellipse",
        "rect",
        "roundRect",
        "fill",
        "stroke",
        "clip",
        "fillText",
        "strokeText",
        "drawImage",
        "putImageData",
        "setLineDash",
    ] {
        d.def_method(it, o, name, 0, noop)?;
    }
    d.def_method(it, o, "measureText", 1, measure)?;
    let k = d.key(it, "canvas")?;
    it.define_data(o, k, this, false, true, true)?;
    it.define_data(c, key, Value::Object(o), false, false, false)?;
    Ok(Value::Object(o))
}

fn measure(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let text = d.string_arg(it, crate::browser_platform::arg(a, 0))?;
    let o = d.obj(it, it.object_prototype())?;
    let k = d.key(it, "width")?;
    it.define_data(
        o,
        k,
        Value::Number(text.chars().count() as f64 * 6.0),
        false,
        true,
        true,
    )?;
    Ok(Value::Object(o))
}

fn data_url(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    canvas(d, it, this)?;
    d.js_str(it, "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=")
}

//! A bounded compatibility context. Drawing does not change pixels, so a canvas has no pixels to read: `getImageData` and
//! `createImageData` give transparent black (zero bytes). `ImageData` is the HTML Standard interface, lazy (`browser_lazy.rs`).
//! One `ImageData` holds at most [`MAX_BYTES`] (64 MiB) of pixels: a larger one throws a `RangeError` (the same class as a failed
//! allocation in Chrome; the message is ours). `createPattern` gives `null` for an `<img>` that is not complete and an object
//! with a no-op `setTransform` for any other element. A gradient is an object with a no-op `addColorStop`.
//! Deviation: `width`, `height`, `data` and `colorSpace` are own read-only data properties, not prototype accessors.
use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::heap::Elem;
use bare_js::value::{ObjId, Value};

/// The most bytes (`width * height * 4`) of one `ImageData`.
const MAX_BYTES: u64 = 64 << 20;

fn arg(a: &[Value], i: usize) -> Value {
    crate::browser_platform::arg(a, i)
}

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    let g = it.global_object();
    let k = d.key(it, "__bareImageData")?;
    if let Value::Object(f) = it.get_value(Value::Object(g), k)? {
        return Ok(f);
    }
    let proto = d.obj(it, it.object_prototype())?;
    let f = d.add_fn(it, image_data, "ImageData", 2, true)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "ImageData")?;
    it.define_data(g, k, Value::Object(f), false, false, false)?;
    Ok(f)
}

fn lazy_image_data(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let g = it.global_object();
    let k = d.key(it, "ImageData")?;
    let v = match a.first() {
        Some(&v) => v,
        None => Value::Object(ensure(d, it)?),
    };
    it.define_data(g, k, v, true, false, true)?;
    Ok(if a.is_empty() { v } else { Value::Undefined })
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("ImageData", lazy_image_data as H)])
}

/// An `unsigned long` of the constructor (`[EnforceRange]`): a `TypeError` when it is not finite or out of range.
fn ulong(it: &mut Interp<'_>, v: Value, who: &str) -> R<u32> {
    let x = it.to_js_number(v)?.trunc();
    if !(0.0..=4294967295.0).contains(&x) {
        return Err(it.type_error(&format!("Failed to construct 'ImageData': {who} is outside the range of an unsigned long.")));
    }
    Ok(x as u32)
}

/// A size of `createImageData` and `getImageData`: a `long` (`NaN` and infinity are 0), and its absolute value is used.
fn long_abs(it: &mut Interp<'_>, v: Value) -> R<u32> {
    let x = it.to_js_number(v)?;
    Ok(if x.is_finite() { x.trunc().abs().min(4294967295.0) as u32 } else { 0 })
}

fn index_size(d: &Dom, it: &mut Interp<'_>, who: &str, msg: &str) -> Error {
    d.dom_err(it, "IndexSizeError", &format!("{who}: {msg}"))
}

/// A new `ImageData` of `w` by `h` over `data` (zero bytes when `None`). The sizes are not zero.
fn make(d: &Dom, it: &mut Interp<'_>, w: u32, h: u32, data: Option<Value>) -> R<Value> {
    let f = ensure(d, it)?;
    let kp = d.key(it, "prototype")?;
    let Value::Object(proto) = it.get_value(Value::Object(f), kp)? else { unreachable!("made") };
    let o = d.obj(it, proto)?;
    make_on(d, it, o, w, h, data)
}

fn make_on(d: &Dom, it: &mut Interp<'_>, o: ObjId, w: u32, h: u32, data: Option<Value>) -> R<Value> {
    let data = match data {
        Some(v) => v,
        None => {
            let n = w as u64 * h as u64 * 4;
            if n > MAX_BYTES {
                return Err(it.range_error("Out of memory at ImageData creation"));
            }
            it.new_typed_array(Elem::U8C, vec![0; n as usize])?
        }
    };
    let cs = d.js_str(it, "srgb")?;
    for (name, v) in [("width", Value::Number(w as f64)), ("height", Value::Number(h as f64)), ("data", data), ("colorSpace", cs)] {
        let k = d.key(it, name)?;
        it.define_data(o, k, v, false, false, true)?;
    }
    Ok(Value::Object(o))
}

/// `new ImageData(sw, sh)` and `new ImageData(data, sw, sh?)`.
fn image_data(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let who = "Failed to construct 'ImageData'";
    if a.len() < 2 {
        return Err(it.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len())));
    }
    let default = ensure(d, it)?;
    let kp = d.key(it, "prototype")?;
    let Value::Object(default) = it.get_value(Value::Object(default), kp)? else { unreachable!("made") };
    let proto = it.host_proto_from_new_target(default)?;
    let o = d.obj(it, proto)?;
    if let Value::Object(src) = a[0] {
        if it.heap.view_type(src) != Some(Elem::U8C) {
            return Err(it.type_error(&format!("{who}: parameter 1 is not of type 'Uint8ClampedArray'.")));
        }
        let len = it.heap.view_parts(src).map_or(0, |p| p.2) as u64;
        let w = ulong(it, a[1], "The source width")?;
        let given = match arg(a, 2) {
            Value::Undefined => None,
            v => Some(ulong(it, v, "The source height")?),
        };
        if len % 4 != 0 {
            return Err(d.dom_err(it, "InvalidStateError", &format!("{who}: The input data length is not a multiple of 4.")));
        }
        if len == 0 || w == 0 {
            return Err(index_size(d, it, who, "The source width is zero or not a number."));
        }
        let rows = len / 4;
        if rows % w as u64 != 0 {
            return Err(index_size(d, it, who, "The input data length is not a multiple of (4 * width)."));
        }
        let h = (rows / w as u64) as u32;
        if given.is_some_and(|g| g != h) {
            return Err(index_size(d, it, who, "The input data length is not equal to (4 * width * height)."));
        }
        return make_on(d, it, o, w, h, Some(a[0]));
    }
    let (w, h) = (ulong(it, a[0], "The source width")?, ulong(it, a[1], "The source height")?);
    if w == 0 || h == 0 {
        return Err(index_size(d, it, who, "The source width or height is zero or not a number."));
    }
    make_on(d, it, o, w, h, None)
}

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
    for (name, len, h) in [
        ("createImageData", 1, create_image_data as H),
        ("getImageData", 4, get_image_data),
        ("createLinearGradient", 4, gradient),
        ("createRadialGradient", 6, gradient),
        ("createConicGradient", 3, gradient),
        ("createPattern", 2, pattern),
        ("isPointInPath", 2, no),
        ("isPointInStroke", 2, no),
    ] {
        d.def_method(it, o, name, len, h)?;
    }
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

fn no(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(false))
}

/// `createImageData(sw, sh)` and `createImageData(imagedata)`: transparent black.
fn create_image_data(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let who = "Failed to execute 'createImageData' on 'CanvasRenderingContext2D'";
    let (w, h) = match arg(a, 0) {
        Value::Object(src) => {
            let (kw, kh) = (d.key(it, "width")?, d.key(it, "height")?);
            let (w, h) = (it.get_value(Value::Object(src), kw)?, it.get_value(Value::Object(src), kh)?);
            (long_abs(it, w)?, long_abs(it, h)?)
        }
        _ if a.len() < 2 => return Err(it.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len()))),
        w => (long_abs(it, w)?, long_abs(it, a[1])?),
    };
    sized(d, it, who, w, h)
}

/// `getImageData(sx, sy, sw, sh)`: a canvas has no pixels, so the result is transparent black.
fn get_image_data(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let who = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D'";
    if a.len() < 4 {
        return Err(it.type_error(&format!("{who}: 4 arguments required, but only {} present.", a.len())));
    }
    let (w, h) = (long_abs(it, a[2])?, long_abs(it, a[3])?);
    sized(d, it, who, w, h)
}

fn sized(d: &Dom, it: &mut Interp<'_>, who: &str, w: u32, h: u32) -> R<Value> {
    if w == 0 || h == 0 {
        return Err(index_size(d, it, who, "The source width or height is zero or not a number."));
    }
    make(d, it, w, h, None)
}

fn gradient(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let o = d.obj(it, it.object_prototype())?;
    d.def_method(it, o, "addColorStop", 2, noop)?;
    Ok(Value::Object(o))
}

/// `createPattern(image, repetition)`: `null` for an `<img>` that is not complete (the spec case), else an object.
fn pattern(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Value::Object(_) = arg(a, 0) else {
        return Err(it.type_error("Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided value is not of type 'CanvasImageSource'."));
    };
    if let Ok(n) = d.el(it, a[0]) {
        if d.doc.borrow().is_html(n, "img") {
            let k = d.key(it, "complete")?;
            if matches!(it.get_value(a[0], k)?, Value::Bool(false)) {
                return Ok(Value::Null);
            }
        }
    }
    let o = d.obj(it, it.object_prototype())?;
    d.def_method(it, o, "setTransform", 1, noop)?;
    Ok(Value::Object(o))
}

//! A bounded compatibility context. Drawing does not change pixels, so a canvas has no pixels to read: `getImageData` and
//! `createImageData` give transparent black (zero bytes). `ImageData` is the HTML Standard interface, lazy (`browser_lazy.rs`).
//! One `ImageData` holds at most [`MAX_BYTES`] (64 MiB) of pixels: a larger one throws a `RangeError` (the same class as a failed
//! allocation in Chrome; the message is ours). `createPattern` gives `null` for an `<img>` that is not complete and an object
//! with a no-op `setTransform` for any other element. A gradient is an object with a no-op `addColorStop`.
//! Deviation: `width`, `height`, `data` and `colorSpace` are own read-only data properties, not prototype accessors.
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js::eval::Interp;
use bare_js::value::ObjId;
use bare_js_api::{Args, Attrs, Elem, HostFn, Local};

/// The most bytes (`width * height * 4`) of one `ImageData`.
const MAX_BYTES: u64 = 64 << 20;

/// The `ImageData` constructor: made by the first use, kept in a hidden property of the global.
#[cold]
#[inline(never)]
fn ensure_s<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let f = s.get_str(g, "__bareImageData")?;
    if is_object(s, f) {
        return Ok(f);
    }
    let proto = s.new_object()?;
    let f = d.add_fn_s(s, image_data, "ImageData", 2, HostFn::Ctor)?;
    let fixed = Attrs { writable: false, enumerable: false, configurable: false };
    s.define_str(f, "prototype", proto, fixed)?;
    s.define_str(proto, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
    let p = obj_id(s, proto).expect("an object");
    old(s, |it| crate::browser_lazy::tag(d, it, p, "ImageData"))?;
    s.define_str(g, "__bareImageData", f, fixed)?;
    Ok(f)
}

fn lazy_image_data<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = match a.get(0) {
        Some(v) => v,
        None => ensure_s(d, s)?,
    };
    let g = s.global();
    s.define_str(g, "ImageData", v, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(if a.is_empty() { v } else { s.undefined() })
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("ImageData", lazy_image_data as HS)])
}

/// An `unsigned long` of the constructor (`[EnforceRange]`): a `TypeError` when it is not finite or out of range.
fn ulong<'s>(s: &mut Sc<'s, '_>, v: Local<'s>, who: &str) -> Res<'s, u32> {
    let x = s.to_number(v)?.trunc();
    if !(0.0..=4294967295.0).contains(&x) {
        return Err(s.type_error(&format!("Failed to construct 'ImageData': {who} is outside the range of an unsigned long.")));
    }
    Ok(x as u32)
}

/// A size of `createImageData` and `getImageData`: a `long` (`NaN` and infinity are 0), and its absolute value is used.
fn long_abs<'s>(s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, u32> {
    let x = s.to_number(v)?;
    Ok(if x.is_finite() { x.trunc().abs().min(4294967295.0) as u32 } else { 0 })
}

fn index_size<'s>(d: &Dom, s: &mut Sc<'s, '_>, who: &str, msg: &str) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, "IndexSizeError", &format!("{who}: {msg}"))
}

/// A new `ImageData` of `w` by `h` with zero bytes. The sizes are not zero.
fn make<'s>(d: &Dom, s: &mut Sc<'s, '_>, w: u32, h: u32) -> Res<'s, Local<'s>> {
    let f = ensure_s(d, s)?;
    let proto = s.get_str(f, "prototype")?;
    let p = obj_id(s, proto).expect("made");
    let o = d.obj_with_s(s, p)?;
    make_on(d, s, o, w, h, None)
}

fn make_on<'s>(d: &Dom, s: &mut Sc<'s, '_>, o: Local<'s>, w: u32, h: u32, data: Option<Local<'s>>) -> Res<'s, Local<'s>> {
    let data = match data {
        Some(v) => v,
        None => {
            let n = (w as u64).saturating_mul(h as u64).saturating_mul(4);
            if n > MAX_BYTES {
                return Err(s.range_error("Out of memory at ImageData creation"));
            }
            s.new_typed_array(Elem::U8C, vec![0; n as usize])?
        }
    };
    let cs = d.js_str_s(s, "srgb")?;
    let (w, h) = (s.number(w as f64), s.number(h as f64));
    let attrs = Attrs { writable: false, enumerable: false, configurable: true };
    for (name, v) in [("width", w), ("height", h), ("data", data), ("colorSpace", cs)] {
        s.define_str(o, name, v, attrs)?;
    }
    Ok(o)
}

/// `new ImageData(sw, sh)` and `new ImageData(data, sw, sh?)`.
fn image_data<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to construct 'ImageData'";
    if a.len() < 2 {
        return Err(s.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len())));
    }
    let f = ensure_s(d, s)?;
    let default = s.get_str(f, "prototype")?;
    let proto = s.proto_from_new_target(default)?;
    let p = obj_id(s, proto).expect("an object");
    let o = d.obj_with_s(s, p)?;
    let (a0, a1, a2) = (arg_s(s, a, 0), arg_s(s, a, 1), arg_s(s, a, 2));
    if is_object(s, a0) {
        let len = match s.view(a0) {
            Some(v) if v.elem == Elem::U8C => v.len as u64,
            _ => return Err(s.type_error(&format!("{who}: parameter 1 is not of type 'Uint8ClampedArray'."))),
        };
        let w = ulong(s, a1, "The source width")?;
        let given = if matches!(s.type_of(a2), bare_js_api::Type::Undefined) { None } else { Some(ulong(s, a2, "The source height")?) };
        if !len.is_multiple_of(4) {
            return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The input data length is not a multiple of 4.")));
        }
        if len == 0 || w == 0 {
            return Err(index_size(d, s, who, "The source width is zero or not a number."));
        }
        let rows = len / 4;
        if !rows.is_multiple_of(w as u64) {
            return Err(index_size(d, s, who, "The input data length is not a multiple of (4 * width)."));
        }
        let h = (rows / w as u64) as u32;
        if given.is_some_and(|g| g != h) {
            return Err(index_size(d, s, who, "The input data length is not equal to (4 * width * height)."));
        }
        return make_on(d, s, o, w, h, Some(a0));
    }
    let (w, h) = (ulong(s, a0, "The source width")?, ulong(s, a1, "The source height")?);
    if w == 0 || h == 0 {
        return Err(index_size(d, s, who, "The source width or height is zero or not a number."));
    }
    make_on(d, s, o, w, h, None)
}

pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, p: ObjId) -> R<()> {
    d.def_method_s(it, p, "getContext", 1, context)?;
    d.def_method_s(it, p, "toDataURL", 0, data_url)?;
    Ok(())
}

fn canvas<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ()> {
    let n = d.el_s(s, this)?;
    if !d.doc.borrow().is_html(n, "canvas") {
        return Err(s.type_error("Illegal invocation"));
    }
    Ok(())
}

fn noop<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.undefined())
}

fn context<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    let a0 = arg_s(s, a, 0);
    if d.string_arg_s(s, a0)? != "2d" {
        return Ok(s.null());
    }
    let made = s.get_str(this, "__bareCanvas2D")?;
    if is_object(s, made) {
        return Ok(made);
    }
    let o = s.new_object()?;
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
        d.def_method_l(s, o, name, 0, noop, false)?;
    }
    d.def_method_l(s, o, "measureText", 1, measure, false)?;
    for (name, len, h) in [
        ("createImageData", 1, create_image_data as HS),
        ("getImageData", 4, get_image_data),
        ("createLinearGradient", 4, gradient),
        ("createRadialGradient", 6, gradient),
        ("createConicGradient", 3, gradient),
        ("createPattern", 2, pattern),
        ("isPointInPath", 2, no),
        ("isPointInStroke", 2, no),
    ] {
        d.def_method_l(s, o, name, len, h, false)?;
    }
    s.define_str(o, "canvas", this, Attrs { writable: false, enumerable: true, configurable: true })?;
    s.define_str(this, "__bareCanvas2D", o, Attrs { writable: false, enumerable: false, configurable: false })?;
    Ok(o)
}

/// The font of a canvas `font` value (a CSS `font` shorthand): its size in pixels and its face. A value without a size is the
/// default, `10px sans-serif`.
fn canvas_font(v: &str) -> bare_layout::font::Metrics {
    use bare_layout::font::{Face, Metrics};
    let (mut size, mut bold) = (None, false);
    for t in v.split_whitespace() {
        if size.is_some() {
            break;
        }
        bold |= matches!(t, "bold" | "bolder") || t.parse::<f32>().is_ok_and(|w| w >= 600.0);
        let t = t.split('/').next().unwrap_or(t);
        let unit = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
        let scale = match unit {
            "px" => 1.0,
            "pt" => 4.0 / 3.0,
            "em" | "rem" => 16.0,
            _ => continue,
        };
        size = t[..t.len() - unit.len()].parse::<f32>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n * scale);
    }
    let Some(size) = size else { return Metrics::new(10.0, Face::Sans) };
    // Like `monospace`, a serif name anywhere in the value counts (not the first entry only). `monospace` wins. `sans-serif` is not serif.
    let serif = (v.contains("serif") && !v.contains("sans-serif")) || ["Times", "Georgia", "Cambria", "Garamond", "Palatino", "Book Antiqua"].iter().any(|n| v.contains(n));
    Metrics::new(size, match (v.contains("monospace"), serif, bold) {
        (true, _, false) => Face::Mono,
        (true, _, true) => Face::MonoBold,
        (_, true, false) => Face::Serif,
        (_, true, true) => Face::SerifBold,
        (_, false, false) => Face::Sans,
        (_, false, true) => Face::SansBold,
    })
}

/// `measureText(text)`: the width of the text in the font of `this.font`, with the metrics that page text uses.
fn measure<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s(s, a, 0);
    let text = d.string_arg_s(s, a0)?;
    let v = s.get_str(this, "font")?;
    let font = if matches!(s.type_of(v), bare_js_api::Type::Undefined) { String::new() } else { d.string_arg_s(s, v)? };
    let width = bare_layout::controls::text_width(&canvas_font(&font), &text);
    let o = s.new_object()?;
    let w = s.number(width as f64);
    s.define_str(o, "width", w, Attrs { writable: false, enumerable: true, configurable: true })?;
    Ok(o)
}

fn data_url<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    d.js_str_s(s, "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=")
}

fn no<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.boolean(false))
}

/// `createImageData(sw, sh)` and `createImageData(imagedata)`: transparent black.
fn create_image_data<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'createImageData' on 'CanvasRenderingContext2D'";
    let a0 = arg_s(s, a, 0);
    let (w, h) = if is_object(s, a0) {
        let (w, h) = (s.get_str(a0, "width")?, s.get_str(a0, "height")?);
        (long_abs(s, w)?, long_abs(s, h)?)
    } else if a.len() < 2 {
        return Err(s.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len())));
    } else {
        let a1 = arg_s(s, a, 1);
        (long_abs(s, a0)?, long_abs(s, a1)?)
    };
    sized(d, s, who, w, h)
}

/// `getImageData(sx, sy, sw, sh)`: a canvas has no pixels, so the result is transparent black.
fn get_image_data<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D'";
    if a.len() < 4 {
        return Err(s.type_error(&format!("{who}: 4 arguments required, but only {} present.", a.len())));
    }
    let (a2, a3) = (arg_s(s, a, 2), arg_s(s, a, 3));
    let (w, h) = (long_abs(s, a2)?, long_abs(s, a3)?);
    sized(d, s, who, w, h)
}

fn sized<'s>(d: &Dom, s: &mut Sc<'s, '_>, who: &str, w: u32, h: u32) -> Res<'s, Local<'s>> {
    if w == 0 || h == 0 {
        return Err(index_size(d, s, who, "The source width or height is zero or not a number."));
    }
    make(d, s, w, h)
}

fn gradient<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    d.def_method_l(s, o, "addColorStop", 2, noop, false)?;
    Ok(o)
}

/// `createPattern(image, repetition)`: `null` for an `<img>` that is not complete (the spec case), else an object.
fn pattern<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s(s, a, 0);
    if !is_object(s, a0) {
        return Err(s.type_error("Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided value is not of type 'CanvasImageSource'."));
    }
    if let Some(n) = d.node_of_s(s, a0).filter(|&n| d.is_element(n))
        && d.doc.borrow().is_html(n, "img")
    {
        let c = s.get_str(a0, "complete")?;
        if s.as_bool(c) == Some(false) {
            return Ok(s.null());
        }
    }
    let o = s.new_object()?;
    d.def_method_l(s, o, "setTransform", 1, noop, false)?;
    Ok(o)
}

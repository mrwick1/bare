//! The 2D context of `<canvas>`, `ImageData`, `ImageBitmap` and `createImageBitmap` (`docs/internal/design-canvas-2d.md`).
//!
//! - Each canvas records its draws (`bare_paint::raster2d::Record`) and rasters them only when something reads the pixels: paint (a
//!   lazy copy in the document), `getImageData`, `toDataURL`, `toBlob`, `createImageBitmap`, and `drawImage` with the canvas as source.
//! - Draws: `fillRect`, `strokeRect`, `clearRect` (anti-aliased), `drawImage` (3, 5 and 9 arguments) from an `<img>`, a canvas and an
//!   `ImageBitmap` (bilinear, or nearest when `imageSmoothingEnabled` is false), `putImageData` with a dirty rectangle. A solid
//!   `fillStyle` or `strokeStyle` (a CSS color) with `globalAlpha`. Paths, the transform and text do nothing yet.
//! - Reads: `getImageData` gives the pixels, not premultiplied. `toDataURL` and `toBlob` give JPEG for `image/jpeg` (`bare_paint::jpeg`)
//!   and PNG for every other type. `toBlob` encodes on the page thread and calls back in a new task.
//! - `ImageData` is the HTML Standard interface, lazy (`browser_lazy.rs`). One `ImageData` holds at most [`MAX_BYTES`] (64 MiB): a
//!   larger one throws a `RangeError` (the same class as a failed allocation in Chrome; the message is ours).
//! - A canvas over Chrome's size limits (`raster2d::fits`) has no pixels: it draws nothing and reads as blank.
//!   `createPattern` gives `null` for an `<img>` that is not complete and an object with a no-op `setTransform` for any other
//!   element. A gradient is an object with a no-op `addColorStop`.
//!
//! Deviation: `width`, `height`, `data` and `colorSpace` of `ImageData` are own read-only data properties, not prototype accessors.
use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js_api::{Args, Attrs, Elem, HostFn, Local};
use bare_paint::raster2d::{self, Affine, Draw, Image, Mode, Px, Record, Source};
use std::sync::{Arc, Mutex};

/// The most bytes (`width * height * 4`) of one `ImageData`.
const MAX_BYTES: u64 = 64 << 20;

/// The `ImageData` constructor: made by the first use, kept in a hidden property of the global.
#[cold]
#[inline(never)]
fn ensure_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let f = s.get_str(g, "__bareImageData")?;
    if is_object::<E>(s, f) {
        return Ok(f);
    }
    let proto = s.new_object()?;
    let f = d.add_fn_s(s, image_data, "ImageData", 2, HostFn::Ctor)?;
    let fixed = Attrs { writable: false, enumerable: false, configurable: false };
    s.define_str(f, "prototype", proto, fixed)?;
    s.define_str(proto, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
    let p = obj_id::<E>(s, proto).expect("an object");
    old::<E, _>(s, |it| crate::browser_lazy::tag(d, it, p, "ImageData"))?;
    s.define_str(g, "__bareImageData", f, fixed)?;
    Ok(f)
}

fn lazy_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = match a.get(0) {
        Some(v) => v,
        None => ensure_s(d, s)?,
    };
    let g = s.global();
    s.define_str(g, "ImageData", v, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(if a.is_empty() { v } else { s.undefined() })
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "ImageData", lazy_image_data),
        Row::both(1, "ImageBitmap", lazy_image_bitmap),
        Row::method(1, "createImageBitmap", 1, create_image_bitmap),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// An `unsigned long` of the constructor (`[EnforceRange]`): a `TypeError` when it is not finite or out of range.
fn ulong<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>, who: &str) -> Res<'s, u32> {
    let x = s.to_number(v)?.trunc();
    if !(0.0..=4294967295.0).contains(&x) {
        return Err(s.type_error(&format!("Failed to construct 'ImageData': {who} is outside the range of an unsigned long.")));
    }
    Ok(x as u32)
}

/// A size of `createImageData` and `getImageData`: a `long` (`NaN` and infinity are 0), and its absolute value is used.
fn long_abs<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, u32> {
    let x = s.to_number(v)?;
    Ok(if x.is_finite() { x.trunc().abs().min(4294967295.0) as u32 } else { 0 })
}

fn index_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, who: &str, msg: &str) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, "IndexSizeError", &format!("{who}: {msg}"))
}

/// A new `ImageData` of `w` by `h` with zero bytes. The sizes are not zero.
fn make<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, w: u32, h: u32) -> Res<'s, Local<'s>> {
    let f = ensure_s(d, s)?;
    let proto = s.get_str(f, "prototype")?;
    let p = obj_id::<E>(s, proto).expect("made");
    let o = d.obj_with_s(s, p)?;
    make_on(d, s, o, w, h, None)
}

fn make_on<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, w: u32, h: u32, data: Option<Local<'s>>) -> Res<'s, Local<'s>> {
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
fn image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to construct 'ImageData'";
    if a.len() < 2 {
        return Err(s.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len())));
    }
    let f = ensure_s(d, s)?;
    let default = s.get_str(f, "prototype")?;
    let proto = s.proto_from_new_target(default)?;
    let p = obj_id::<E>(s, proto).expect("an object");
    let o = d.obj_with_s(s, p)?;
    let (a0, a1, a2) = (arg_s::<E>(s, a, 0), arg_s::<E>(s, a, 1), arg_s::<E>(s, a, 2));
    if is_object::<E>(s, a0) {
        let len = match s.view(a0) {
            Some(v) if v.elem == Elem::U8C => v.len as u64,
            _ => return Err(s.type_error(&format!("{who}: parameter 1 is not of type 'Uint8ClampedArray'."))),
        };
        let w = ulong::<E>(s, a1, "The source width")?;
        let given = if matches!(s.type_of(a2), bare_js_api::Type::Undefined) { None } else { Some(ulong::<E>(s, a2, "The source height")?) };
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
    let (w, h) = (ulong::<E>(s, a0, "The source width")?, ulong::<E>(s, a1, "The source height")?);
    if w == 0 || h == 0 {
        return Err(index_size(d, s, who, "The source width or height is zero or not a number."));
    }
    make_on(d, s, o, w, h, None)
}

pub(crate) fn members<E: Eng>(d: &Dom<E>, it: &mut E::Vm, p: E::Obj) -> R<E, ()> {
    d.def_method_s(it, p, "getContext", 1, context)?;
    d.def_method_s(it, p, "toDataURL", 0, data_url)?;
    d.def_method_s(it, p, "toBlob", 1, to_blob)?;
    crate::browser_reflect::reflect!(d, it, p, &["canvas"], { "width" => "width": Long(300, 0), "height" => "height": Long(150, 0) });
    Ok(())
}

fn canvas<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, ()> {
    let n = d.el_s(s, this)?;
    if !d.doc.borrow().is_html(n, "canvas") {
        return Err(s.type_error("Illegal invocation"));
    }
    Ok(())
}

fn noop<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.undefined())
}

fn context<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    if d.string_arg_s(s, a0)? != "2d" {
        return Ok(s.null());
    }
    let made = s.get_str(this, "__bareCanvas2D")?;
    if is_object::<E>(s, made) {
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
        "setLineDash",
    ] {
        d.def_method_l(s, o, name, 0, noop, false)?;
    }
    d.def_method_l(s, o, "measureText", 1, measure, false)?;
    d.def_method_l(s, o, "fillRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Fill), false)?;
    d.def_method_l(s, o, "strokeRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Stroke), false)?;
    d.def_method_l(s, o, "clearRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Clear), false)?;
    d.def_method_l(s, o, "drawImage", 3, draw_image, false)?;
    d.def_method_l(s, o, "putImageData", 3, put_image_data, false)?;
    for (name, len, h) in [
        ("createImageData", 1, create_image_data as HS<E>),
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
    // The defaults of the drawing state (HTML, "CanvasFillStrokeStyles", "CanvasCompositing", "CanvasPathDrawingStyles").
    let (black, one, yes) = (d.js_str_s(s, "#000000")?, s.number(1.0), s.boolean(true));
    for (name, v) in [("fillStyle", black), ("strokeStyle", black), ("lineWidth", one), ("globalAlpha", one), ("imageSmoothingEnabled", yes)] {
        s.define_str(o, name, v, Attrs { writable: true, enumerable: true, configurable: true })?;
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
fn measure<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    let text = d.string_arg_s(s, a0)?;
    let v = s.get_str(this, "font")?;
    let font = if matches!(s.type_of(v), bare_js_api::Type::Undefined) { String::new() } else { d.string_arg_s(s, v)? };
    let width = bare_layout::controls::text_width(&canvas_font(&font), &text);
    let o = s.new_object()?;
    let w = s.number(width as f64);
    s.define_str(o, "width", w, Attrs { writable: false, enumerable: true, configurable: true })?;
    Ok(o)
}

fn no<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.boolean(false))
}

/// `createImageData(sw, sh)` and `createImageData(imagedata)`: transparent black.
fn create_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'createImageData' on 'CanvasRenderingContext2D'";
    let a0 = arg_s::<E>(s, a, 0);
    let (w, h) = if is_object::<E>(s, a0) {
        let (w, h) = (s.get_str(a0, "width")?, s.get_str(a0, "height")?);
        (long_abs::<E>(s, w)?, long_abs::<E>(s, h)?)
    } else if a.len() < 2 {
        return Err(s.type_error(&format!("{who}: 2 arguments required, but only {} present.", a.len())));
    } else {
        let a1 = arg_s::<E>(s, a, 1);
        (long_abs::<E>(s, a0)?, long_abs::<E>(s, a1)?)
    };
    sized(d, s, who, w, h)
}

/// `getImageData(sx, sy, sw, sh)`: the pixels of the rectangle, not premultiplied. Pixels outside the canvas are transparent black.
fn get_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D'";
    if a.len() < 4 {
        return Err(s.type_error(&format!("{who}: 4 arguments required, but only {} present.", a.len())));
    }
    let mut v = [0i64; 4];
    for (i, x) in v.iter_mut().enumerate() {
        let l = arg_s::<E>(s, a, i);
        let n = s.to_number(l)?;
        *x = if n.is_finite() { n.trunc().clamp(-(1i64 << 40) as f64, (1i64 << 40) as f64) as i64 } else { 0 };
    }
    let [mut x, mut y, w, h] = v;
    if w == 0 || h == 0 {
        return Err(index_size(d, s, who, "The source width or height is zero or not a number."));
    }
    if w < 0 {
        x += w;
    }
    if h < 0 {
        y += h;
    }
    let (w, h) = (w.unsigned_abs().min(u32::MAX as u64) as u32, h.unsigned_abs().min(u32::MAX as u64) as u32);
    if (w as u64) * (h as u64) * 4 > MAX_BYTES {
        return Err(s.range_error("Out of memory at ImageData creation"));
    }
    let el = s.get_str(this, "canvas")?;
    let n = d.el_s(s, el)?;
    let bytes = match record_of(d, s, el, n)? {
        Some(rec) => lock(&rec).rgba(x, y, w, h),
        None => vec![0; w as usize * h as usize * 4],
    };
    let f = ensure_s(d, s)?;
    let proto = s.get_str(f, "prototype")?;
    let p = obj_id::<E>(s, proto).expect("made");
    let o = d.obj_with_s(s, p)?;
    let data = s.new_typed_array(Elem::U8C, bytes)?;
    make_on(d, s, o, w, h, Some(data))
}

fn sized<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, who: &str, w: u32, h: u32) -> Res<'s, Local<'s>> {
    if w == 0 || h == 0 {
        return Err(index_size(d, s, who, "The source width or height is zero or not a number."));
    }
    make(d, s, w, h)
}

fn gradient<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    d.def_method_l(s, o, "addColorStop", 2, noop, false)?;
    Ok(o)
}

/// `createPattern(image, repetition)`: `null` for an `<img>` that is not complete (the spec case), else an object.
fn pattern<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    if !is_object::<E>(s, a0) {
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

/// The bitmap of a canvas that a script drew on: its record (`bare_paint::raster2d`). The document holds a lazy copy (`Pixels::lazy`)
/// that paint makes at its first read: the copy plays the record, so a canvas that nobody paints or reads never rasters.
pub(crate) struct Raster {
    rec: Arc<Mutex<Record>>,
}

#[cfg(test)]
impl Raster {
    /// How many times the record played its ops.
    pub(crate) fn rasters(&self) -> u64 {
        lock(&self.rec).rasters
    }
}

#[derive(Clone, Copy)]
enum Op {
    Fill,
    Stroke,
    Clear,
}

/// A CSS color as a canvas style: `#hex`, a name, or `rgb()`, `rgba()`, `hsl()`, `hsla()`. `None` for another value (a gradient object).
fn style_color(v: &str) -> Option<bare_css::color::Color> {
    use bare_css::tokenizer::Token;
    let mut t = bare_css::tokenizer::tokenize(v.trim());
    let c = match t.next()? {
        Token::Hash { value, .. } => bare_css::color::Color::hex(&value),
        Token::Ident(name) => bare_css::color::Color::named(&name.to_ascii_lowercase()),
        Token::Function(name) if bare_css::color::is_function(&name) => bare_css::color::parse_function(&name, &mut t),
        _ => None,
    };
    t.next().is_none().then_some(c?)
}

fn lock(r: &Mutex<Record>) -> std::sync::MutexGuard<'_, Record> {
    r.lock().unwrap_or_else(|e| e.into_inner())
}

/// The size of the canvas element `el` (its `width` and `height`).
fn size_of<'s, E: Eng>(s: &mut Sc<'s, E>, el: Local<'s>) -> Res<'s, (u32, u32)> {
    let (w, h) = (s.get_str(el, "width")?, s.get_str(el, "height")?);
    Ok((long_abs::<E>(s, w)?, long_abs::<E>(s, h)?))
}

/// The record of the canvas element `el` (node `n`), made at the first use. A new size clears it. `None` for a canvas of no
/// pixels or over the limits: it draws nothing.
fn record_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, el: Local<'s>, n: NodeId) -> Res<'s, Option<Arc<Mutex<Record>>>> {
    let (w, h) = size_of::<E>(s, el)?;
    if w == 0 || h == 0 || !raster2d::fits(w, h) {
        return Ok(None);
    }
    let mut map = d.canvases.borrow_mut();
    let r = map.entry(n).or_insert_with(|| Raster { rec: Arc::new(Mutex::new(Record::new(w, h))) });
    {
        let mut rec = lock(&r.rec);
        if (rec.w, rec.h) != (w, h) {
            rec.resize(w, h);
        }
    }
    Ok(Some(r.rec.clone()))
}

/// Record `draw` on the canvas of the context `this`, and give the document a new lazy copy if paint read the old one.
fn push<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, draw: Draw) -> Res<'s, ()> {
    let el = s.get_str(this, "canvas")?;
    let n = d.el_s(s, el)?;
    let Some(rec) = record_of(d, s, el, n)? else { return Ok(()) };
    let (w, h) = {
        let mut r = lock(&rec);
        r.push(draw);
        (r.w, r.h)
    };
    let mut doc = d.doc.borrow_mut();
    if doc.image(n).is_none_or(|b| b.2.is_ready() || (b.0, b.1) != (w, h)) {
        doc.set_image(n, Some((w, h, bare_html::dom::Pixels::lazy(move || lock(&rec).straight()))));
    }
    Ok(())
}

/// A rectangle as a closed path.
fn rect_path(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// `globalAlpha` of the context, 0 to 1.
fn global_alpha<'s, E: Eng>(s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, f32> {
    let ga = s.get_str(this, "globalAlpha")?;
    Ok(if matches!(s.type_of(ga), bare_js_api::Type::Number) { s.to_number(ga)?.clamp(0.0, 1.0) as f32 } else { 1.0 })
}

/// `fillRect`, `strokeRect` and `clearRect(x, y, w, h)`. A value that is not finite draws nothing, as the standard says.
fn draw<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, op: Op) -> Res<'s, Local<'s>> {
    let mut r = [0f32; 4];
    for (i, v) in r.iter_mut().enumerate() {
        let x = arg_s::<E>(s, a, i);
        *v = s.to_number(x)? as f32;
    }
    if r.iter().any(|v| !v.is_finite()) {
        return Ok(s.undefined());
    }
    let [x, y, rw, rh] = r;
    let (x0, x1, y0, y1) = (x.min(x + rw), x.max(x + rw), y.min(y + rh), y.max(y + rh));
    let draw = match op {
        Op::Clear => Draw::Fill { path: rect_path(x0, y0, x1, y1), evenodd: false, src: Source::Solid(0), alpha: 1.0, mode: Mode::Clear },
        Op::Fill | Op::Stroke => {
            let v = s.get_str(this, if matches!(op, Op::Fill) { "fillStyle" } else { "strokeStyle" })?;
            let text = if matches!(s.type_of(v), bare_js_api::Type::String) { d.string_arg_s(s, v)? } else { String::new() };
            let Some(c) = style_color(&text) else { return Ok(s.undefined()) };
            let color = raster2d::premul((c.a as u32) << 24 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32);
            let alpha = global_alpha::<E>(s, this)?;
            let path = if matches!(op, Op::Fill) {
                rect_path(x0, y0, x1, y1)
            } else {
                let lw = s.get_str(this, "lineWidth")?;
                let lw = if matches!(s.type_of(lw), bare_js_api::Type::Number) { s.to_number(lw)? as f32 } else { 1.0 };
                let k = if lw.is_finite() && lw > 0.0 { lw / 2.0 } else { 0.5 };
                if rw == 0.0 && rh == 0.0 {
                    return Ok(s.undefined());
                }
                // A line when one side is zero (butt caps). Else the outer box and the inner box the other way round: nonzero
                // leaves the inside empty. The corners are miter joins.
                if rw == 0.0 || rh == 0.0 {
                    if rw == 0.0 { rect_path(x0 - k, y0, x1 + k, y1) } else { rect_path(x0, y0 - k, x1, y1 + k) }
                } else {
                    let mut p = rect_path(x0 - k, y0 - k, x1 + k, y1 + k);
                    if x1 - x0 > 2.0 * k && y1 - y0 > 2.0 * k {
                        p.push((f32::NAN, f32::NAN));
                        p.extend(rect_path(x0 + k, y0 + k, x1 - k, y1 - k).into_iter().rev());
                    }
                    p
                }
            };
            Draw::Fill { path, evenodd: false, src: Source::Solid(color), alpha, mode: Mode::SourceOver }
        }
    };
    push(d, s, this, draw)?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ pictures

/// A picture source of `drawImage` and `createImageBitmap`. `Ok(None)`: an `<img>` with no picture (nothing to draw). A canvas of
/// no pixels is an `InvalidStateError`, a closed `ImageBitmap` too. Another value is a `TypeError` with `ty` as the type list.
fn image_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, who: &str, ty: &str) -> Res<'s, Option<Image>> {
    if let Some(n) = d.node_of_s(s, v).filter(|&n| d.is_element(n)) {
        if d.doc.borrow().is_html(n, "img") {
            return Ok(d.doc.borrow().image(n).map(|(w, h, px)| Image { w: *w, h: *h, px: Px::Straight(px.clone()) }));
        }
        if d.doc.borrow().is_html(n, "canvas") {
            let Some(rec) = record_of(d, s, v, n)? else {
                return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The image argument is a canvas element with a width or height of 0.")));
            };
            return Ok(Some(lock(&rec).image()));
        }
    }
    if let Some(o) = obj_id::<E>(s, v)
        && let Some(b) = d.bitmaps.borrow().get(&o)
    {
        return match b {
            Some(img) => Ok(Some(img.clone())),
            None => Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The image source is detached."))),
        };
    }
    Err(s.type_error(&format!("{who}: The provided value is not of type '{ty}'.")))
}

/// Where the source rectangle `src` (x, y, w, h, in picture pixels) of a `iw` by `ih` picture lands in `dst`: the source clipped to
/// the picture and the destination clipped in the same proportion (HTML, `drawImage`). Gives the destination path, the transform from
/// canvas to picture and the clamp box. `None`: nothing to draw.
fn place(iw: u32, ih: u32, src: [f64; 4], dst: [f64; 4]) -> Option<(Vec<(f32, f32)>, Affine, (u32, u32, u32, u32))> {
    let norm = |[x, y, w, h]: [f64; 4]| [x.min(x + w), y.min(y + h), w.abs(), h.abs()];
    let ([sx, sy, sw, sh], [dx, dy, dw, dh]) = (norm(src), norm(dst));
    if sw == 0.0 || sh == 0.0 || dw == 0.0 || dh == 0.0 {
        return None;
    }
    let (kx, ky) = (dw / sw, dh / sh);
    let (cx0, cy0, cx1, cy1) = (sx.max(0.0), sy.max(0.0), (sx + sw).min(iw as f64), (sy + sh).min(ih as f64));
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let (px0, py0) = (dx + (cx0 - sx) * kx, dy + (cy0 - sy) * ky);
    let (px1, py1) = (dx + (cx1 - sx) * kx, dy + (cy1 - sy) * ky);
    let inv = Affine([1.0 / kx, 0.0, 0.0, 1.0 / ky, sx - dx / kx, sy - dy / ky]);
    let clamp = (cx0.floor() as u32, cy0.floor() as u32, (cx1.ceil() as u32).min(iw), (cy1.ceil() as u32).min(ih));
    Some((rect_path(px0 as f32, py0 as f32, px1 as f32, py1 as f32), inv, clamp))
}

const DRAW_TYPES: &str = "(CSSImageValue or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or OffscreenCanvas or SVGImageElement or VideoFrame)";

/// `drawImage(image, dx, dy)`, `(image, dx, dy, dw, dh)` and `(image, sx, sy, sw, sh, dx, dy, dw, dh)`.
fn draw_image<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'drawImage' on 'CanvasRenderingContext2D'";
    if a.len() < 3 {
        return Err(s.type_error(&format!("{who}: 3 arguments required, but only {} present.", a.len())));
    }
    let n = if a.len() >= 9 { 9 } else if a.len() >= 5 { 5 } else { 3 };
    if a.len() != n && !matches!(a.len(), 3 | 5 | 9) && a.len() < 9 {
        return Err(s.type_error(&format!("{who}: Valid arities are: [3, 5, 9], but {} arguments provided.", a.len())));
    }
    let a0 = arg_s::<E>(s, a, 0);
    let img = image_of(d, s, a0, who, DRAW_TYPES)?;
    let mut v = [0f64; 8];
    for i in 1..n {
        let x = arg_s::<E>(s, a, i);
        v[i - 1] = s.to_number(x)?;
    }
    let Some(img) = img else { return Ok(s.undefined()) };
    if v[..n - 1].iter().any(|x| !x.is_finite()) {
        return Ok(s.undefined());
    }
    let (iw, ih) = (img.w as f64, img.h as f64);
    let (src, dst) = match n {
        3 => ([0.0, 0.0, iw, ih], [v[0], v[1], iw, ih]),
        5 => ([0.0, 0.0, iw, ih], [v[0], v[1], v[2], v[3]]),
        _ => ([v[0], v[1], v[2], v[3]], [v[4], v[5], v[6], v[7]]),
    };
    let Some((path, inv, clamp)) = place(img.w, img.h, src, dst) else { return Ok(s.undefined()) };
    let smooth = s.get_str(this, "imageSmoothingEnabled")?;
    let smooth = s.as_bool(smooth) != Some(false);
    let alpha = global_alpha::<E>(s, this)?;
    push(d, s, this, Draw::Fill { path, evenodd: false, src: Source::Image { img, inv, clamp, smooth }, alpha, mode: Mode::SourceOver })?;
    Ok(s.undefined())
}

/// Is `v` an `ImageData` (is `ImageData.prototype` on its prototype chain)?
fn is_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, bool> {
    if !is_object::<E>(s, v) {
        return Ok(false);
    }
    let f = ensure_s(d, s)?;
    let proto = s.get_str(f, "prototype")?;
    let want = obj_id::<E>(s, proto);
    let mut at = s.get_prototype_of(v)?;
    while let Some(p) = at {
        if obj_id::<E>(s, p) == want {
            return Ok(true);
        }
        at = s.get_prototype_of(p)?;
    }
    Ok(false)
}

/// `putImageData(imagedata, dx, dy, dirtyX?, dirtyY?, dirtyWidth?, dirtyHeight?)`: the pixels replace the canvas pixels, with no
/// blend, transform or clip.
fn put_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'putImageData' on 'CanvasRenderingContext2D'";
    if a.len() < 3 {
        return Err(s.type_error(&format!("{who}: 3 arguments required, but only {} present.", a.len())));
    }
    let data = arg_s::<E>(s, a, 0);
    if !is_image_data(d, s, data)? {
        return Err(s.type_error(&format!("{who}: parameter 1 is not of type 'ImageData'.")));
    }
    let mut v = [0f64; 6];
    for (i, x) in v.iter_mut().enumerate().take(if a.len() >= 7 { 6 } else { 2 }) {
        let l = arg_s::<E>(s, a, i + 1);
        *x = s.to_number(l)?;
    }
    let (w, h, bytes) = {
        let (wv, hv, dv) = (s.get_str(data, "width")?, s.get_str(data, "height")?, s.get_str(data, "data")?);
        let (w, h) = (long_abs::<E>(s, wv)?, long_abs::<E>(s, hv)?);
        let Some(b) = s.bytes(dv) else { return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The source data has been detached."))) };
        (w, h, b.to_vec())
    };
    if v.iter().any(|x| !x.is_finite()) {
        return Ok(s.undefined());
    }
    let (dx, dy) = (v[0].trunc(), v[1].trunc());
    // The dirty rectangle, normalized and clipped to the image data (HTML, "putImageData").
    let (mut x0, mut y0, mut x1, mut y1) = (0.0, 0.0, w as f64, h as f64);
    if a.len() >= 7 {
        let [dxr, dyr, dw, dh] = [v[2].trunc(), v[3].trunc(), v[4].trunc(), v[5].trunc()];
        (x0, x1) = (dxr.min(dxr + dw).max(0.0), dxr.max(dxr + dw).min(w as f64));
        (y0, y1) = (dyr.min(dyr + dh).max(0.0), dyr.max(dyr + dh).min(h as f64));
    }
    if x1 <= x0 || y1 <= y0 || bytes.len() < w as usize * h as usize * 4 {
        return Ok(s.undefined());
    }
    let (pw, ph) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut px = Vec::with_capacity(pw as usize * ph as usize);
    for y in y0 as usize..y1 as usize {
        for x in x0 as usize..x1 as usize {
            let i = (y * w as usize + x) * 4;
            px.push(raster2d::premul(u32::from_be_bytes([bytes[i + 3], bytes[i], bytes[i + 1], bytes[i + 2]])));
        }
    }
    push(d, s, this, Draw::Put { x: (dx + x0) as i32, y: (dy + y0) as i32, w: pw, h: ph, px })?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ encoders

/// The bytes of the canvas `el` in `ty` and its type. `None`: a canvas of no pixels (`data:,` and a `null` Blob).
/// `image/jpeg` at `quality` (0 to 1, else Chrome's 0.92) with the pixels on black. PNG for every other type, as the standard says for a
/// type it does not support.
fn encode<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, el: Local<'s>, ty: &str, quality: Option<f64>) -> Res<'s, Option<(Vec<u8>, &'static str)>> {
    let n = d.el_s(s, el)?;
    let Some(rec) = record_of(d, s, el, n)? else { return Ok(None) };
    let mut r = lock(&rec);
    let (w, h) = (r.w, r.h);
    if ty.eq_ignore_ascii_case("image/jpeg") {
        let q = (quality.unwrap_or(0.92) * 100.0).round().max(1.0) as u8;
        if let Some(b) = bare_paint::jpeg::encode(w, h, r.flush().map(|p| &p[..]), q) {
            return Ok(Some((b, "image/jpeg")));
        }
    }
    let rgba = r.rgba(0, 0, w, h);
    drop(r);
    Ok(bare_paint::png::encode_rgba(w, h, &rgba).ok().map(|b| (b, "image/png")))
}

/// The type and quality arguments of `toDataURL` and `toBlob` at `at` and `at + 1`.
fn type_args<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, at: usize) -> Res<'s, (String, Option<f64>)> {
    let t = arg_s::<E>(s, a, at);
    let ty = if matches!(s.type_of(t), bare_js_api::Type::Undefined) { "image/png".to_string() } else { d.string_arg_s(s, t)? };
    let q = arg_s::<E>(s, a, at + 1);
    let q = if matches!(s.type_of(q), bare_js_api::Type::Number) { Some(s.to_number(q)?).filter(|q| (0.0..=1.0).contains(q)) } else { None };
    Ok((ty, q))
}

fn data_url<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    let (ty, q) = type_args(d, s, a, 0)?;
    let url = match encode(d, s, this, &ty, q)? {
        Some((bytes, mime)) => format!("data:{mime};base64,{}", crate::browser_encoding::base64(&bytes)),
        None => "data:,".to_string(),
    };
    d.js_str_s(s, &url)
}

/// `toBlob(callback, type, quality)`: the callback gets the Blob (or `null` for a canvas of no pixels) in a new task.
fn to_blob<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    let who = "Failed to execute 'toBlob' on 'HTMLCanvasElement'";
    if a.is_empty() {
        return Err(s.type_error(&format!("{who}: 1 argument required, but only 0 present.")));
    }
    let cb = arg_s::<E>(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error(&format!("{who}: The callback provided as parameter 1 is not a function.")));
    }
    let (ty, q) = type_args(d, s, a, 1)?;
    let blob = match encode(d, s, this, &ty, q)? {
        Some((bytes, mime)) => old_v::<E>(s, |it| crate::browser_web::blob_from_bytes(d, it, bytes, mime.to_string()))?,
        None => s.null(),
    };
    crate::browser_scheduler::queue_call(d, s, cb, blob)?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ ImageBitmap

/// The `ImageBitmap` constructor (no script can call it), made at the first use and kept in a hidden property of the global.
#[cold]
#[inline(never)]
fn bitmap_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let f = s.get_str(g, "__bareImageBitmap")?;
    if is_object::<E>(s, f) {
        return Ok(f);
    }
    let proto = s.new_object()?;
    let f = d.add_fn_s(s, illegal, "ImageBitmap", 0, HostFn::Ctor)?;
    let fixed = Attrs { writable: false, enumerable: false, configurable: false };
    s.define_str(f, "prototype", proto, fixed)?;
    s.define_str(proto, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
    d.def_get_l(s, proto, "width", |d, s, t, _| bitmap_size(d, s, t, false), None)?;
    d.def_get_l(s, proto, "height", |d, s, t, _| bitmap_size(d, s, t, true), None)?;
    d.def_method_l(s, proto, "close", 0, bitmap_close, false)?;
    let p = obj_id::<E>(s, proto).expect("an object");
    old::<E, _>(s, |it| crate::browser_lazy::tag(d, it, p, "ImageBitmap"))?;
    s.define_str(g, "__bareImageBitmap", f, fixed)?;
    Ok(f)
}

fn illegal<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

fn lazy_image_bitmap<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = match a.get(0) {
        Some(v) => v,
        None => bitmap_ctor(d, s)?,
    };
    let g = s.global();
    s.define_str(g, "ImageBitmap", v, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(if a.is_empty() { v } else { s.undefined() })
}

fn bitmap_entry<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (E::Obj, Option<(u32, u32)>)> {
    let o = obj_id::<E>(s, this).filter(|o| d.bitmaps.borrow().contains_key(o));
    let Some(o) = o else { return Err(s.type_error("Illegal invocation")) };
    let size = d.bitmaps.borrow()[&o].as_ref().map(|i| (i.w, i.h));
    Ok((o, size))
}

fn bitmap_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, height: bool) -> Res<'s, Local<'s>> {
    let (_, size) = bitmap_entry(d, s, this)?;
    let (w, h) = size.unwrap_or((0, 0));
    Ok(s.number(if height { h } else { w } as f64))
}

fn bitmap_close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, _) = bitmap_entry(d, s, this)?;
    d.bitmaps.borrow_mut().insert(o, None);
    Ok(s.undefined())
}

fn new_bitmap<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, img: Image) -> Res<'s, Local<'s>> {
    let f = bitmap_ctor(d, s)?;
    let proto = s.get_str(f, "prototype")?;
    let p = obj_id::<E>(s, proto).expect("made");
    let o = d.obj_s(s, p)?;
    d.bitmaps.borrow_mut().insert(o, Some(img));
    d.own_obj(s.engine(), o, crate::browser::Owner::Bitmap(o));
    Ok(d.obj_local(s, o))
}

/// `createImageBitmap(image, options?)` and `createImageBitmap(image, sx, sy, sw, sh, options?)`. The image is an `<img>`, a canvas,
/// an `ImageBitmap`, an `ImageData` or a `Blob`. A Blob is decoded on the page thread. The options `resizeWidth`, `resizeHeight`,
/// `resizeQuality` ("pixelated" samples the nearest pixel) and `imageOrientation: "flipY"` work. `premultiplyAlpha` and
/// `colorSpaceConversion` are ignored.
fn create_image_bitmap<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = make_bitmap(d, s, a);
    crate::browser_web::settle::<E>(s, p, r)
}

fn make_bitmap<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'createImageBitmap' on 'Window'";
    if a.is_empty() {
        return Err(s.type_error(&format!("{who}: 1 argument required, but only 0 present.")));
    }
    let src = arg_s::<E>(s, a, 0);
    let ty = "(Blob or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or ImageData or OffscreenCanvas or SVGImageElement or VideoFrame)";
    let img = if let Some(o) = obj_id::<E>(s, src)
        && let Some(bytes) = crate::browser_web::blob_bytes(d, o)
    {
        let Ok(i) = bare_image::decode(&bytes) else {
            return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The source image could not be decoded.")));
        };
        Image { w: i.width, h: i.height, px: Px::Straight(bare_html::dom::Pixels::from(i.pixels.to_vec())) }
    } else if is_image_data(d, s, src)? {
        let (wv, hv, dv) = (s.get_str(src, "width")?, s.get_str(src, "height")?, s.get_str(src, "data")?);
        let (w, h) = (long_abs::<E>(s, wv)?, long_abs::<E>(s, hv)?);
        let Some(b) = s.bytes(dv) else { return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The source data has been detached."))) };
        let px = b.chunks_exact(4).map(|p| raster2d::premul(u32::from_be_bytes([p[3], p[0], p[1], p[2]]))).collect();
        Image { w, h, px: Px::Premul(Arc::new(px)) }
    } else {
        match image_of(d, s, src, who, ty)? {
            Some(i) => i,
            None => return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The image element contains no image data."))),
        }
    };
    // The crop rectangle and the options.
    let crop = a.len() >= 5;
    let mut c = [0.0, 0.0, img.w as f64, img.h as f64];
    if crop {
        for (i, x) in c.iter_mut().enumerate() {
            let v = arg_s::<E>(s, a, i + 1);
            *x = s.to_number(v)?.trunc();
        }
        if c[2] == 0.0 || !c[2].is_finite() {
            return Err(s.range_error(&format!("{who}: The crop rect width is 0.")));
        }
        if c[3] == 0.0 || !c[3].is_finite() {
            return Err(s.range_error(&format!("{who}: The crop rect height is 0.")));
        }
        c = [c[0].min(c[0] + c[2]), c[1].min(c[1] + c[3]), c[2].abs(), c[3].abs()];
    }
    let opts = arg_s::<E>(s, a, if crop { 5 } else { 1 });
    let (mut rw, mut rh, mut pixelated, mut flip) = (None, None, false, false);
    if is_object::<E>(s, opts) {
        for (name, out) in [("resizeWidth", &mut rw), ("resizeHeight", &mut rh)] {
            let v = s.get_str(opts, name)?;
            if !matches!(s.type_of(v), bare_js_api::Type::Undefined) {
                let x = s.to_number(v)?;
                if !(x.is_finite() && x >= 1.0 && x <= 4294967295.0) {
                    return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The {name} provided is 0.")));
                }
                *out = Some(x.trunc());
            }
        }
        let q = s.get_str(opts, "resizeQuality")?;
        pixelated = matches!(s.type_of(q), bare_js_api::Type::String) && d.string_arg_s(s, q)? == "pixelated";
        let o = s.get_str(opts, "imageOrientation")?;
        flip = matches!(s.type_of(o), bare_js_api::Type::String) && d.string_arg_s(s, o)? == "flipY";
    }
    let (ow, oh) = match (rw, rh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, (w * c[3] / c[2]).ceil()),
        (None, Some(h)) => ((h * c[2] / c[3]).ceil(), h),
        (None, None) => (c[2], c[3]),
    };
    let (ow, oh) = (ow as u32, oh as u32);
    if !raster2d::fits(ow, oh) {
        return Err(d.dom_err_s(s, "InvalidStateError", &format!("{who}: The ImageBitmap could not be allocated.")));
    }
    // No crop, resize or flip: share the pixels of the source.
    let same = [c[0], c[1]] == [0.0, 0.0] && (ow, oh) == (img.w, img.h) && !flip;
    let img = if same {
        img
    } else {
        let mut r = Record::new(ow, oh);
        if let Some((mut path, mut inv, clamp)) = place(img.w, img.h, c, [0.0, 0.0, ow as f64, oh as f64]) {
            if flip {
                // The point (x, y) shows what (x, oh - y) shows without the flip.
                inv = inv.then(&Affine([1.0, 0.0, 0.0, -1.0, 0.0, oh as f64]));
                path.iter_mut().for_each(|p| p.1 = oh as f32 - p.1);
            }
            r.push(Draw::Fill { path, evenodd: false, src: Source::Image { img, inv, clamp, smooth: !pixelated }, alpha: 1.0, mode: Mode::SourceOver });
        }
        r.image()
    };
    new_bitmap(d, s, img)
}

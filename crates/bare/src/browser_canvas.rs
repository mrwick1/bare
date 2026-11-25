//! A bounded compatibility context. Drawing does not change pixels, so a canvas has no pixels to read: `getImageData` and
//! `createImageData` give transparent black (zero bytes). `ImageData` is the HTML Standard interface, lazy (`browser_lazy.rs`).
//! One `ImageData` holds at most [`MAX_BYTES`] (64 MiB) of pixels: a larger one throws a `RangeError` (the same class as a failed
//! allocation in Chrome; the message is ours). `createPattern` gives `null` for an `<img>` that is not complete and an object
//! with a no-op `setTransform` for any other element. A gradient is an object with a no-op `addColorStop`.
//! Deviation: `width`, `height`, `data` and `colorSpace` are own read-only data properties, not prototype accessors.
//!
//! Pixels: `fillRect`, `strokeRect` and `clearRect` draw into the bitmap of the canvas (`Raster`), in a solid `fillStyle` or
//! `strokeStyle` (a CSS color) with `globalAlpha`. A pixel is in a rectangle when its center is (no anti-aliasing). The other drawing
//! calls and the transform do nothing yet. Paint shows the bitmap like the picture of an `<img>` (`Document::set_image`).
use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, Elem, HostFn, Local};

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
        "drawImage",
        "putImageData",
        "setLineDash",
    ] {
        d.def_method_l(s, o, name, 0, noop, false)?;
    }
    d.def_method_l(s, o, "measureText", 1, measure, false)?;
    d.def_method_l(s, o, "fillRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Fill), false)?;
    d.def_method_l(s, o, "strokeRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Stroke), false)?;
    d.def_method_l(s, o, "clearRect", 4, |d, s, t, a| draw(d, s, t, a, Op::Clear), false)?;
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
    let (black, one) = (d.js_str_s(s, "#000000")?, s.number(1.0));
    for (name, v) in [("fillStyle", black), ("strokeStyle", black), ("lineWidth", one), ("globalAlpha", one)] {
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

fn data_url<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    canvas(d, s, this)?;
    d.js_str_s(s, "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=")
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

/// `getImageData(sx, sy, sw, sh)`: a canvas has no pixels, so the result is transparent black.
fn get_image_data<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D'";
    if a.len() < 4 {
        return Err(s.type_error(&format!("{who}: 4 arguments required, but only {} present.", a.len())));
    }
    let (a2, a3) = (arg_s::<E>(s, a, 2), arg_s::<E>(s, a, 3));
    let (w, h) = (long_abs::<E>(s, a2)?, long_abs::<E>(s, a3)?);
    sized(d, s, who, w, h)
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

/// The bitmap of a canvas that a script drew on, `0xAARRGGBB` not premultiplied. The document holds a lazy copy (`Pixels::lazy`) that
/// paint makes at its first read, so the many calls of one frame copy the pixels once.
pub(crate) struct Raster {
    w: u32,
    h: u32,
    px: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
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

/// `fillRect`, `strokeRect` and `clearRect(x, y, w, h)`. A value that is not finite draws nothing, as the standard says.
fn draw<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, op: Op) -> Res<'s, Local<'s>> {
    let mut r = [0f32; 4];
    for (i, v) in r.iter_mut().enumerate() {
        let x = arg_s::<E>(s, a, i);
        *v = s.to_number(x)? as f32;
    }
    let el = s.get_str(this, "canvas")?;
    let n = d.el_s(s, el)?;
    if r.iter().any(|v| !v.is_finite()) {
        return Ok(s.undefined());
    }
    let color = match op {
        Op::Clear => 0,
        Op::Fill | Op::Stroke => {
            let v = s.get_str(this, if matches!(op, Op::Fill) { "fillStyle" } else { "strokeStyle" })?;
            let text = if matches!(s.type_of(v), bare_js_api::Type::String) { d.string_arg_s(s, v)? } else { String::new() };
            let Some(c) = style_color(&text) else { return Ok(s.undefined()) };
            let ga = s.get_str(this, "globalAlpha")?;
            let ga = if matches!(s.type_of(ga), bare_js_api::Type::Number) { s.to_number(ga)?.clamp(0.0, 1.0) } else { 1.0 };
            let alpha = (c.a as f64 * ga).round() as u32;
            if alpha == 0 {
                return Ok(s.undefined());
            }
            alpha << 24 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
        }
    };
    let (w, h) = (s.get_str(el, "width")?, s.get_str(el, "height")?);
    let (w, h) = (long_abs::<E>(s, w)?, long_abs::<E>(s, h)?);
    if w == 0 || h == 0 || (w as u64) * (h as u64) * 4 > MAX_BYTES {
        return Ok(s.undefined());
    }
    let mut map = d.canvases.borrow_mut();
    let raster = map.entry(n).or_insert_with(|| Raster { w, h, px: Default::default() });
    {
        let mut px = raster.px.lock().unwrap_or_else(|e| e.into_inner());
        // A new size clears the bitmap (HTML, "the canvas element": setting `width` or `height`).
        if (raster.w, raster.h) != (w, h) || px.len() != (w * h) as usize {
            (raster.w, raster.h) = (w, h);
            *px = vec![0; (w * h) as usize];
        }
        let [x, y, rw, rh] = r;
        let (x0, x1, y0, y1) = (x.min(x + rw), x.max(x + rw), y.min(y + rh), y.max(y + rh));
        match op {
            Op::Fill | Op::Clear => fill(&mut px, w, h, (x0, y0, x1, y1), color),
            Op::Stroke => {
                let lw = s.get_str(this, "lineWidth")?;
                let lw = if matches!(s.type_of(lw), bare_js_api::Type::Number) { s.to_number(lw)? as f32 } else { 1.0 };
                let k = if lw.is_finite() && lw > 0.0 { lw / 2.0 } else { 0.5 };
                // The four sides, each a rectangle of the line width centered on the path.
                for e in [(x0 - k, y0 - k, x1 + k, y0 + k), (x0 - k, y1 - k, x1 + k, y1 + k), (x0 - k, y0 + k, x0 + k, y1 - k), (x1 - k, y0 + k, x1 + k, y1 - k)] {
                    fill(&mut px, w, h, e, color);
                }
            }
        }
    }
    // Paint reads a lazy copy. A copy that paint made already, or one of another size, is out of date: give a new one.
    let mut doc = d.doc.borrow_mut();
    if doc.image(n).is_none_or(|b| b.2.is_ready() || (b.0, b.1) != (w, h)) {
        let px = raster.px.clone();
        doc.set_image(n, Some((w, h, bare_html::dom::Pixels::lazy(move || px.lock().unwrap_or_else(|e| e.into_inner()).clone()))));
    }
    Ok(s.undefined())
}

/// Paint the pixels whose centers are in `(x0, y0, x1, y1)` with `color` over the old ones (source-over). `0` clears them.
fn fill(px: &mut [u32], w: u32, h: u32, (x0, y0, x1, y1): (f32, f32, f32, f32), color: u32) {
    let span = |lo: f32, hi: f32, max: u32| ((lo - 0.5).ceil().max(0.0) as u32, ((hi - 0.5).ceil().max(0.0) as u32).min(max));
    let ((cx0, cx1), (cy0, cy1)) = (span(x0, x1, w), span(y0, y1, h));
    let a = color >> 24;
    for y in cy0..cy1 {
        for p in &mut px[(y * w + cx0) as usize..(y * w + cx1) as usize] {
            *p = if color == 0 || a == 255 { color } else { over(color, *p) };
        }
    }
}

/// `src` over `dst`, both `0xAARRGGBB` not premultiplied.
fn over(src: u32, dst: u32) -> u32 {
    let (sa, da) = ((src >> 24) as f32 / 255.0, (dst >> 24) as f32 / 255.0);
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return 0;
    }
    let ch = |sh: u32| {
        let (s, d) = (((src >> sh) & 255) as f32, ((dst >> sh) & 255) as f32);
        (((s * sa + d * da * (1.0 - sa)) / oa).round() as u32).min(255) << sh
    };
    ((oa * 255.0).round() as u32) << 24 | ch(16) | ch(8) | ch(0)
}

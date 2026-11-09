//! `HTMLImageElement` and the `Image` constructor.
//!
//! What is built: `src`, `srcset`, `alt`, `width`, `height` (the content attributes), `naturalWidth`, `naturalHeight`, `complete`, `currentSrc`,
//! `onload`, `onerror`, `decode()` and `new Image(width, height)`. Every `<img>` wrapper has this prototype.
//!
//! Loading is the code of `images.rs`. The page asks the document for the pictures once it has loaded, and a script can ask for more:
//! the IDL setter `src`, `setAttribute('src', ...)` and `new Image()` + `src` push the element on a list (`requests`). `Page::images_step`
//! takes the list, downloads and decodes the pictures on the worker threads, gives each element its bitmap and then calls
//! [`Browser::img_settled`]. That queues one task (a `Task::Media` with the step `LOADED` or `FAILED`), which runs the promises of `decode()`
//! and fires `load` or `error` at the element. So a detached `new Image()` loads like an `<img>` in the tree.
//!
//! Deviations from Chrome:
//! - `complete` is true until a download starts, and true again when it ends (a failed picture too). Chrome says false for an `<img>` whose
//!   `src` the parser has seen but whose request has not started; the page here starts the requests at `interactive`.
//! - `width` and `height` give the content attribute (0 when missing or not a number), not the rendered size.
//! - A picture that this engine cannot decode (GIF, AVIF, a file over the limits) fires `error`.
//! - No `sizes`, `crossOrigin`, `loading`, `decoding`, `referrerPolicy`, `x`, `y`, `isMap`, `useMap`, `align`, and no `Image.prototype.fetchPriority`.
//! - Only these attribute changes start a request: the `src` setter and `setAttribute('src')`. A `srcset` change does not.

use crate::browser::*;
use crate::browser_dom::attr_set;
use crate::browser_lazy::lazy_global;
use crate::browser_platform::arg;
use bare_html::dom::NodeId;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

type V = R<Value>;

/// Steps of a `Task::Media` that belong to an `<img>` (`browser_media.rs` uses the steps below 100).
pub(crate) const LOADED: u8 = 200;
pub(crate) const FAILED: u8 = 201;
/// Requests that wait for the page. More are dropped.
const MAX_REQUESTS: usize = 4096;

#[derive(Clone, Copy)]
pub(crate) struct Made {
    pub(crate) proto: ObjId,
    /// `HTMLImageElement` and `Image`.
    ctors: [ObjId; 2],
}

#[derive(Default)]
pub(crate) struct Img {
    pub(crate) made: Cell<Option<Made>>,
    /// Elements whose `src` was set by a script and that the page has not taken yet.
    requests: RefCell<Vec<NodeId>>,
    /// Elements with a download on the way (asked for, not settled), so `complete` is false.
    pending: RefCell<std::collections::HashSet<NodeId>>,
    /// The promises of `decode()` for each element.
    decodes: RefCell<HashMap<NodeId, Vec<Value>>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if let Some(m) = d.img.made.get() {
        out.push(Value::Object(m.proto));
        out.extend(m.ctors.into_iter().map(Value::Object));
        out.extend(d.img.decodes.borrow().values().flatten());
    }
}

/// The prototype of the wrapper of an `<img>`.
pub(crate) fn proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    ensure(d, it).map(|m| m.proto)
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    match d.img.made.get() {
        Some(m) => Ok(m),
        None => make(d, it),
    }
}

fn ens(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

fn pick_el(d: &Dom) -> ObjId {
    d.img.made.get().expect("made").ctors[0]
}

fn pick_image(d: &Dom) -> ObjId {
    d.img.made.get().expect("made").ctors[1]
}

lazy_global!(lazy_el, "HTMLImageElement", ens, pick_el);
lazy_global!(lazy_image, "Image", ens, pick_image);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("HTMLImageElement", lazy_el as H), ("Image", lazy_image)])
}

fn illegal_ctor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'HTMLImageElement': Illegal constructor"))
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let proto = d.obj(it, d.p().html)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let el = d.add_fn(it, illegal_ctor, "HTMLImageElement", 0, true)?;
    it.define_data(el, kp, Value::Object(proto), false, false, false)?;
    crate::browser_lazy::tag(d, it, proto, "HTMLImageElement")?;
    // `Image` has the prototype of `HTMLImageElement`.
    let image = d.add_fn(it, image_ctor, "Image", 0, true)?;
    it.define_data(image, kp, Value::Object(proto), false, false, false)?;
    for (name, g, s) in [
        ("src", src_get as H, Some(src_set as H)),
        ("srcset", attr_get::<0>, Some(attr_set_::<0>)),
        ("alt", attr_get::<1>, Some(attr_set_::<1>)),
        ("currentSrc", src_get, None),
        ("width", size::<0>, Some(set_size::<0>)),
        ("height", size::<1>, Some(set_size::<1>)),
        ("naturalWidth", natural::<0>, None),
        ("naturalHeight", natural::<1>, None),
        ("complete", complete, None),
        ("onload", on_get::<0>, Some(on_set::<0>)),
        ("onerror", on_get::<1>, Some(on_set::<1>)),
    ] {
        d.def_get(it, proto, name, g, s)?;
    }
    let (key, f) = (d.key(it, "decode")?, d.add_fn(it, decode, "decode", 0, false)?);
    it.define_data(proto, key, Value::Object(f), true, true, true)?;
    it.define_data(proto, kc, Value::Object(el), true, false, true)?;
    let m = Made { proto, ctors: [el, image] };
    d.img.made.set(Some(m));
    Ok(m)
}

const ATTRS: [&str; 2] = ["srcset", "alt"];
const EVENTS: [&str; 2] = ["load", "error"];

/// The `<img>` that `this` wraps, else `Illegal invocation`.
fn img_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    if d.doc.borrow().element_name(n) == "img" { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

fn attr_of(d: &Dom, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
}

fn src_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let s = attr_of(d, n, "src").map_or_else(String::new, |r| crate::browser_media::resolved(d, &r));
    d.js_str(it, &s)
}

fn src_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, "src", &s)?;
    request(d, n);
    Ok(Value::Undefined)
}

fn attr_get<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let s = attr_of(d, n, ATTRS[I]).unwrap_or_default();
    d.js_str(it, &s)
}

fn attr_set_<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, ATTRS[I], &s)?;
    Ok(Value::Undefined)
}

/// `width` and `height`: the content attribute as an unsigned integer, else 0.
fn size<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let s = attr_of(d, n, ["width", "height"][I]).unwrap_or_default();
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    Ok(Value::Number(t[..digits].parse::<u32>().ok().filter(|&v| v <= i32::MAX as u32).unwrap_or(0) as f64))
}

fn set_size<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let v = bare_js::value::to_uint32(it.to_js_number(arg(a, 0))?);
    attr_set(d, it, n, ["width", "height"][I], &(if v > i32::MAX as u32 { 0 } else { v }).to_string())?;
    Ok(Value::Undefined)
}

fn natural<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    let doc = d.doc.borrow();
    Ok(Value::Number(doc.image(n).map_or(0, |b| if I == 0 { b.0 } else { b.1 }) as f64))
}

fn complete(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    Ok(Value::Bool(!d.img.pending.borrow().contains(&n)))
}

fn on_get<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    Ok(crate::browser_handlers::handler_get(d, Target::Node(n), EVENTS[N]))
}

fn on_set<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = img_el(d, it, this)?;
    crate::browser_handlers::handler_set(d, Target::Node(n), EVENTS[N], a.first().copied())?;
    Ok(Value::Undefined)
}

/// `decode()`: fulfils when the picture is there (now, or when the download ends), else rejects with `EncodingError`.
fn decode(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = match img_el(d, it, this) {
        Ok(n) => n,
        Err(Error::Throw(v)) => {
            let p = it.host_new_promise()?;
            it.host_reject_promise(p, v)?;
            return Ok(p);
        }
        Err(e) => return Err(e),
    };
    let p = it.host_new_promise()?;
    if d.img.pending.borrow().contains(&n) {
        d.img.decodes.borrow_mut().entry(n).or_default().push(p);
    } else if d.doc.borrow().image(n).is_some() {
        it.host_resolve_promise(p, Value::Undefined)?;
    } else {
        let Error::Throw(v) = d.dom_err(it, "EncodingError", "The source image cannot be decoded.") else { return Ok(p) };
        it.host_reject_promise(p, v)?;
    }
    Ok(p)
}

/// `new Image(width, height)`: a detached `img` with the attributes `width` and `height` when the arguments are not `undefined`.
fn image_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let mut sizes = [None, None];
    for (i, s) in sizes.iter_mut().enumerate() {
        if !matches!(arg(a, i), Value::Undefined) {
            *s = Some(bare_js::value::to_uint32(it.to_js_number(arg(a, i))?));
        }
    }
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, 0)?;
        if doc.find_atom("img").is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim("dom-names"));
        }
        doc.create_element("img")
    };
    let w = d.wrap(it, n)?;
    for (name, v) in ["width", "height"].into_iter().zip(sizes) {
        if let Some(v) = v {
            attr_set(d, it, n, name, &v.to_string())?;
        }
    }
    Ok(w)
}

/// A script set the `src` of `n`: the page loads the picture.
fn request(d: &Dom, n: NodeId) {
    let has = attr_of(d, n, "src").is_some_and(|s| !s.trim().is_empty());
    if !has {
        d.img.pending.borrow_mut().remove(&n);
        return;
    }
    let mut r = d.img.requests.borrow_mut();
    if r.len() < MAX_REQUESTS && !r.contains(&n) {
        r.push(n);
        d.img.pending.borrow_mut().insert(n);
    }
}

/// `setAttribute('src', ...)` on any element: an `<img>` loads the new picture.
pub(crate) fn src_changed(d: &Dom, n: NodeId) {
    if d.img.made.get().is_some() && d.doc.borrow().element_name(n) == "img" {
        request(d, n);
    }
}

impl Dom {
    /// Run a step of an `<img>`: settle the promises of `decode()` and fire `load` or `error`.
    pub(crate) fn img_task(&self, it: &mut Interp<'_>, n: NodeId, step: u8) -> R<()> {
        let list = self.img.decodes.borrow_mut().remove(&n).unwrap_or_default();
        for p in list {
            if step == LOADED {
                it.host_resolve_promise(p, Value::Undefined)?;
            } else if let Error::Throw(v) = self.dom_err(it, "EncodingError", "The source image cannot be decoded.") {
                it.host_reject_promise(p, v)?;
            }
        }
        let e = self.new_event(it, EVENTS[usize::from(step == FAILED)], false, false, false, None)?;
        self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), e).map(|_| ())
    }
}

impl Browser {
    /// Take the elements that scripts asked a picture for.
    pub(crate) fn img_requests(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.dom.img.requests.borrow_mut())
    }

    /// Is a request waiting?
    pub(crate) fn img_requested(&self) -> bool {
        !self.dom.img.requests.borrow().is_empty()
    }

    /// The picture of `n` arrived (`ok`) or failed: `complete` is true, and `load` or `error` fires in a task.
    pub(crate) fn img_settled(&self, n: NodeId, ok: bool) {
        self.dom.img.pending.borrow_mut().remove(&n);
        let _ = self.dom.queue_media(n, 0, if ok { LOADED } else { FAILED }, 0.0);
    }

    /// The page starts to load the picture of `n`: `complete` is false until `img_settled`.
    pub(crate) fn img_started(&self, n: NodeId) {
        if self.dom.img.made.get().is_some() {
            self.dom.img.pending.borrow_mut().insert(n);
        }
    }
}

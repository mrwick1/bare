//! `HTMLImageElement` and the `Image` constructor.
//!
//! `HTMLImageElement` itself is in `browser_elements.rs`. This file adds what loads pictures: `new Image(width, height)`, `decode()`, `onload`,
//! `onerror` and the events.
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
//! - A picture that this engine cannot decode (GIF, AVIF, a file over the limits) fires `error`.
//! - Only these attribute changes start a request: the `src` setter and `setAttribute('src')`. A `srcset` change does not.

use crate::eng::{Eng, Fault, Ops};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_dom::attr_set_s;
use crate::browser_lazy::lazy_global;
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js_api::{Args, HostFn, Local, Type};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Steps of a `Task::Media` that belong to an `<img>` (`browser_media.rs` uses the steps below 100).
pub(crate) const LOADED: u8 = 200;
pub(crate) const FAILED: u8 = 201;
/// Requests that wait for the page. More are dropped.
const MAX_REQUESTS: usize = 4096;

#[derive(Default)]
pub(crate) struct Img<E: Eng> {
    /// The `Image` constructor, made by the first read of the global.
    pub(crate) made: Cell<Option<<E as Eng>::Obj>>,
    /// Elements whose `src` was set by a script and that the page has not taken yet.
    requests: RefCell<Vec<NodeId>>,
    /// The Blob of a request with a `blob:` URL, read when `src` was set: the page can revoke the URL before the load starts.
    blobs: RefCell<HashMap<NodeId, std::sync::Arc<[u8]>>>,
    /// Elements with a download on the way (asked for, not settled), so `complete` is false.
    pub(crate) pending: RefCell<std::collections::HashSet<NodeId>>,
    /// The promises of `decode()` for each element.
    decodes: RefCell<HashMap<NodeId, Vec<bare_js_api::Persistent>>>,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    out.extend(d.img.made.get().map(E::obj_raw));
}

fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    if d.img.made.get().is_none() {
        let proto = crate::browser_elements::proto(d, it, "img")?;
        let f = d.add_fn_s_obj(it, image_ctor, "Image", 0, HostFn::Ctor)?;
        let kp = d.key(it, "prototype")?;
        it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
        d.img.made.set(Some(f));
    }
    Ok(())
}

lazy_global!(lazy_image, "Image", ensure, |d| d.img.made.get().expect("made"));

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "Image", lazy_image),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// The members of `HTMLImageElement.prototype` that load pictures (`browser_elements.rs` calls this when it makes the prototype).
pub(crate) fn members<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj) -> R<E, ()> {
    d.def_get_s(it, proto, "onload", on_get::<0, _>, Some(on_set::<0, _>))?;
    d.def_get_s(it, proto, "onerror", on_get::<1, _>, Some(on_set::<1, _>))?;
    let (key, f) = (d.key(it, "decode")?, d.add_fn_s_obj(it, decode, "decode", 0, HostFn::Method)?);
    it.define_data(proto, key, E::obj_raw(f), true, true, true)
}

const EVENTS: [&str; 2] = ["load", "error"];

/// The `<img>` that `this` wraps, else `Illegal invocation`.
fn img_el<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().element_name(n) == "img" { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn attr_of<E: Eng>(d: &Dom<E>, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
}

fn on_get<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = img_el(d, s, this)?;
    old_v::<E>(s, |it| Ok(crate::browser_handlers::handler_get(d, it, Target::Node(n), EVENTS[N])))
}

fn on_set<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = img_el(d, s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| crate::browser_handlers::handler_set(d, it, Target::Node(n), EVENTS[N], v))?;
    Ok(s.undefined())
}

/// `decode()`: fulfils when the picture is there (now, or when the download ends), else rejects with `EncodingError`.
fn decode<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = match img_el(d, s, this) {
        Ok(n) => n,
        Err(bare_js_api::Error::Throw(v)) => {
            let p = s.new_promise()?;
            s.reject_promise(p, v)?;
            return Ok(p);
        }
        Err(e) => return Err(e),
    };
    let p = s.new_promise()?;
    if d.img.pending.borrow().contains(&n) {
        let held = s.persist(p);
        d.img.decodes.borrow_mut().entry(n).or_default().push(held);
    } else if d.doc.borrow().image(n).is_some() {
        let u = s.undefined();
        s.resolve_promise(p, u)?;
    } else {
        let bare_js_api::Error::Throw(v) = d.dom_err_s(s, "EncodingError", "The source image cannot be decoded.") else { return Ok(p) };
        s.reject_promise(p, v)?;
    }
    Ok(p)
}

/// `new Image(width, height)`: a detached `img` with the attributes `width` and `height` when the arguments are not `undefined`.
fn image_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let mut sizes = [None, None];
    for (i, size) in sizes.iter_mut().enumerate() {
        let v = arg_s::<E>(s, a, i);
        if s.type_of(v) != Type::Undefined {
            *size = Some(crate::js_num::to_uint32(s.to_number(v)?));
        }
    }
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, 0)?;
        if doc.find_atom("img").is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim_s(s, "dom-names"));
        }
        doc.create_element("img")
    };
    let w = d.wrap_s(s, n)?;
    for (name, v) in ["width", "height"].into_iter().zip(sizes) {
        if let Some(v) = v {
            attr_set_s(d, s, n, name, &v.to_string())?;
        }
    }
    Ok(w)
}

/// A script set the `src` of `n`: the page loads the picture.
pub(crate) fn request<E: Eng>(d: &Dom<E>, n: NodeId) {
    let src = attr_of(d, n, "src").filter(|s| !s.trim().is_empty());
    let Some(src) = src else {
        d.img.pending.borrow_mut().remove(&n);
        return;
    };
    match crate::browser_web::is_blob_url(&src).then(|| crate::browser_web::blob_url_bytes(d, &src)).flatten() {
        Some(bytes) => d.img.blobs.borrow_mut().insert(n, bytes),
        None => d.img.blobs.borrow_mut().remove(&n),
    };
    let mut r = d.img.requests.borrow_mut();
    if r.len() < MAX_REQUESTS && !r.contains(&n) {
        r.push(n);
        d.img.pending.borrow_mut().insert(n);
    }
}

/// `setAttribute('src', ...)` on any element: an `<img>` loads the new picture.
pub(crate) fn src_changed<E: Eng>(d: &Dom<E>, n: NodeId) {
    if d.doc.borrow().element_name(n) == "img" {
        request(d, n);
    }
}

impl<E: Eng> Dom<E> {
    /// Run a step of an `<img>`: settle the promises of `decode()` and fire `load` or `error`.
    pub(crate) fn img_task(&self, it: &mut E::Vm, n: NodeId, step: u8) -> R<E, ()> {
        let list = self.img.decodes.borrow_mut().remove(&n).unwrap_or_default();
        for p in list {
            let p = it.persistent_value(&p);
            if step == LOADED {
                it.host_resolve_promise(p, E::undef())?;
            } else if let Fault::Throw(v) = self.dom_err(it, "EncodingError", "The source image cannot be decoded.") {
                it.host_reject_promise(p, v)?;
            }
        }
        let e = self.new_event(it, EVENTS[usize::from(step == FAILED)], false, false, false, None)?;
        self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), e).map(|_| ())
    }
}

impl<E: Eng> Browser<E> {
    /// Take the elements that scripts asked a picture for.
    pub(crate) fn img_requests(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.dom.img.requests.borrow_mut())
    }

    /// The bytes of the `blob:` URL `url` that `n` loads: the Blob read when a script set `src`, else the Blob in the store now.
    pub(crate) fn img_blob(&self, n: NodeId, url: &str) -> Option<std::sync::Arc<[u8]>> {
        self.dom.img.blobs.borrow_mut().remove(&n).or_else(|| crate::browser_web::blob_url_bytes(&self.dom, url))
    }

    /// The bytes of the Blob under the `blob:` URL `url` (a CSS picture).
    pub(crate) fn blob_bytes(&self, url: &str) -> Option<std::sync::Arc<[u8]>> {
        crate::browser_web::blob_url_bytes(&self.dom, url)
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
        self.dom.img.pending.borrow_mut().insert(n);
    }
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The elements with a picture request, a download on the way or a `decode()` promise.
pub(crate) fn held<E: Eng>(d: &Dom<E>, out: &mut Vec<NodeId>) {
    out.extend(d.img.requests.borrow().iter().copied());
    out.extend(d.img.pending.borrow().iter().copied());
    out.extend(d.img.decodes.borrow().keys().copied());
}

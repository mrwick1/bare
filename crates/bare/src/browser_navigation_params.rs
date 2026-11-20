//! JS `URL` and `URLSearchParams` over the native records of `browser_navigation_url.rs`.
//!
//! A `URL` object owns a `Rec`. `url.searchParams` is one live `URLSearchParams` for that URL: a change on either side
//! shows on the other (`url_set`, `commit_params`). The wrappers keep each other alive through `Host::trace`, so
//! an unreachable pair (a cycle) is collected together. Metadata is bounded (`NavLimits`): wrapper count, pairs per
//! list, bytes of all lists, bytes of a URL string. No native borrow is held while JS runs (coercion, `forEach`).
//!
//! Supported: `new URL(url[, base])`, `URL.canParse`, the components `href origin protocol username password host
//! hostname port pathname search searchParams hash` (read/write where the standard allows), `toString`, `toJSON`;
//! `new URLSearchParams(string | iterable of pairs | record)`, `append delete get getAll has set sort toString forEach keys
//! values entries size` and `[Symbol.iterator]` (the same function as `entries`).
//!
//! Iteration: `keys()`, `values()` and `entries()` return live iterator objects (they read the list at each `next`, so a change shows).
//! They inherit from `%IteratorPrototype%`, so they are iterable. A constructor argument with `Symbol.iterator` (read once) is a
//! sequence of pairs, read with the engine's own iteration (`Interp::host_iter_*`, the one `for-of` uses). A pair is an object
//! that is iterable with exactly two values; at most three are read. An ordinary error closes the open iterators (inner first);
//! a limit, `Unsupported` or a heap fault closes nothing and runs no more JS. Any other object is a record: own enumerable keys,
//! an enumerable symbol key is a `TypeError`. Caps (pairs, bytes) are checked before the list grows. A byte view with more
//! keys than `max_params` is refused before its keys are listed.
//!
//! Gates (never a silent wrong answer):
//! - Anything `browser_navigation_url.rs` gates (other schemes, user info, IDNA, backslashes...) is `Unsupported`.
//! - `URL.parse` and subclassing are absent. `URL.createObjectURL` and `revokeObjectURL` are in `browser_web.rs`.

use crate::browser::*;
use crate::browser_navigation_url::{self as nu, Part, Rec, UErr, form_parse, form_serialize};
use crate::browser_scoped::*;
use bare_js::eval::{Error, HostIter, Interp};
use bare_js::value::{ObjId, PropertyKey, Value};
use bare_js_api::{Args, Engine as _, Local, Traced, Type};

/// The three kinds are owners (`Owner::Url`, `Owner::Params`, `Owner::Iter`): `Host::trace(key)` visits their edges and `Host::prune`
/// drops their entries (`browser_navigation::trace_owner`, `prune_owner`).
pub(crate) struct UrlObj {
    pub rec: Rec,
    pub sp: Option<Traced<Value>>,
}

pub(crate) struct ParamsObj {
    pub list: Vec<(String, String)>,
    pub url: Option<Traced<Value>>,
    pub bytes: usize,
}

pub(crate) struct IterObj {
    pub sp: Traced<Value>,
    pub idx: usize,
    /// 0 keys, 1 values, 2 entries.
    pub kind: u8,
}

/// The object that an edge holds. An edge is made from an object only.
pub(crate) fn edge(t: &Traced<Value>) -> ObjId {
    match *t.raw() {
        Value::Object(o) => o,
        _ => unreachable!("an edge holds an object"),
    }
}

/// Make `o` the owner of its edges. Call it once the entry is in its table and nothing can fail any more.
fn own<'s>(d: &Dom, s: &mut Sc<'s, '_>, o: Local<'s>, w: Owner) -> Res<'s, ()> {
    let k = d.st.borrow_mut().owners.key(w);
    s.set_host_owner(o, k)
}

/// The argument `i` that `need` has checked.
fn at<'s>(a: Args<'s>, i: usize) -> Local<'s> {
    a.get(i).expect("checked by need")
}

fn need<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, n: usize, what: &str) -> Res<'s, ()> {
    if a.len() < n {
        return Err(s.type_error(&format!("Failed to execute '{what}': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as a USVString (a lone surrogate becomes U+FFFD, as WebIDL says).
pub(crate) fn usv<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, String> {
    // A string is read in place: `to_string` pushes a local for another value only.
    let max = d.nav.lim.get().max_url_bytes;
    let read = |s: &Sc<'_, '_>, t| s.as_str(t).map(|t| (t.len(), (t.len() <= max).then(|| t.to_string_lossy())));
    let (len, out) = match read(s, v) {
        Some(r) => r,
        None => {
            let t = s.to_string(v)?;
            read(s, t).ok_or_else(|| s.type_error("not a string"))?
        }
    };
    let Some(out) = out else { return Err(d.lim_s(s, "url-string")) };
    s.charge((len / 32) as u64)?;
    Ok(out)
}

/// `usv` for the helpers that hold an `Interp` (the init of `URLSearchParams`).
fn usv_i(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<String> {
    let s = it.to_js_string(v)?;
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    if len > d.nav.lim.get().max_url_bytes {
        return Err(d.lim("url-string"));
    }
    it.charge((len / 32) as u64)?;
    match it.heap.to_string_lossy(s) {
        Ok(s) => Ok(s),
        Err(e) => Err(it.heap_error(e)),
    }
}

/// `usv` of the optional argument `i`: `None` when it is absent or `undefined`.
fn usv_opt<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Res<'s, Option<String>> {
    match a.get(i) {
        Some(v) if s.type_of(v) != Type::Undefined => usv(d, s, v).map(Some),
        _ => Ok(None),
    }
}

/// The error a script sees for a failed parse. `what` is the `TypeError` text (a `SyntaxError` for `Location`).
pub(crate) fn uerr<'s>(d: &Dom, s: &mut Sc<'s, '_>, e: UErr, msg: &str, syntax: bool) -> bare_js_api::Error<'s> {
    match e {
        UErr::Invalid if syntax => d.dom_err_s(s, "SyntaxError", msg),
        UErr::Invalid => s.type_error(msg),
        UErr::Scheme => lift_err(s, Error::Unsupported(unsupported("URL scheme other than http and https"))),
        UErr::Gate(w) => lift_err(s, Error::Unsupported(unsupported(w))),
    }
}

fn room(d: &Dom) -> R<()> {
    let n = d.nav.s.borrow();
    if n.urls.len() + n.params.len() + n.iters.len() >= d.nav.lim.get().max_objects {
        return Err(d.lim("url-objects"));
    }
    Ok(())
}

fn fit(d: &Dom, r: &Rec) -> R<()> {
    if r.href().len() > d.nav.lim.get().max_url_bytes {
        return Err(d.lim("url-string"));
    }
    Ok(())
}

fn pair_bytes(list: &[(String, String)]) -> usize {
    list.iter().map(|(n, v)| n.len() + v.len() + 32).sum()
}

/// Replace the list of `sp` (caps checked first).
fn set_list(d: &Dom, sp: ObjId, list: Vec<(String, String)>) -> R<()> {
    let lim = d.nav.lim.get();
    let bytes = pair_bytes(&list);
    let mut n = d.nav.s.borrow_mut();
    let n = &mut *n;
    let Some(p) = n.params.get_mut(&sp) else { return Ok(()) };
    if list.len() > lim.max_params || n.param_bytes - p.bytes + bytes > lim.max_param_bytes {
        return Err(d.lim("url-params"));
    }
    n.param_bytes = n.param_bytes - p.bytes + bytes;
    p.bytes = bytes;
    p.list = list;
    Ok(())
}

/// The list of `sp` changed: write it into the query of its URL (the whole URL must still fit).
fn commit_params(d: &Dom, sp: ObjId) -> R<()> {
    let (url, ser) = {
        let n = d.nav.s.borrow();
        let p = &n.params[&sp];
        match &p.url {
            Some(u) => (edge(u), form_serialize(&p.list)),
            None => return Ok(()),
        }
    };
    let Some(mut rec) = d.nav.s.borrow().urls.get(&url).map(|u| u.rec.clone()) else { return Ok(()) };
    rec.set_query(if ser.is_empty() { None } else { Some(&ser) });
    fit(d, &rec)?;
    if let Some(u) = d.nav.s.borrow_mut().urls.get_mut(&url) {
        u.rec = rec;
    }
    Ok(())
}

fn new_url<'s>(d: &Dom, s: &mut Sc<'s, '_>, rec: Rec, proto: ObjId) -> Res<'s, Local<'s>> {
    lift(s, room(d))?;
    lift(s, fit(d, &rec))?;
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    d.nav.s.borrow_mut().urls.insert(o, UrlObj { rec, sp: None });
    own(d, s, l, Owner::Url(o))?;
    Ok(l)
}

/// A new `URLSearchParams` for `list`. `url` is the object of its `URL`, if it has one. The object and its local.
fn new_params<'s>(d: &Dom, s: &mut Sc<'s, '_>, list: Vec<(String, String)>, url: Option<Local<'s>>) -> Res<'s, (ObjId, Local<'s>)> {
    new_params_in(d, s, list, url, d.nav.p.get().expect("installed").params)
}

fn new_params_in<'s>(d: &Dom, s: &mut Sc<'s, '_>, list: Vec<(String, String)>, url: Option<Local<'s>>, proto: ObjId) -> Res<'s, (ObjId, Local<'s>)> {
    lift(s, room(d))?;
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    let url = url.map(|u| s.trace_ref(u));
    d.nav.s.borrow_mut().params.insert(o, ParamsObj { list: Vec::new(), url, bytes: 0 });
    if let Err(e) = set_list(d, o, list) {
        d.nav.s.borrow_mut().params.remove(&o);
        return Err(lift_err(s, e));
    }
    own(d, s, l, Owner::Params(o))?;
    Ok((o, l))
}

// ------------------------------------------------------------------------------------------ URL

#[inline(always)]
fn url_id<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    match obj_id(s, this) {
        Some(o) if d.nav.s.borrow().urls.contains_key(&o) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

/// The prototype of a constructor call: the `prototype` of `new.target`, else `default`.
fn proto_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, default: ObjId) -> Res<'s, ObjId> {
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    Ok(obj_id(s, proto).expect("a prototype"))
}

pub(crate) fn url_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let proto = proto_of(d, s, d.nav.p.get().expect("installed").url)?;
    need(s, a, 1, "construct 'URL'")?;
    let text = usv(d, s, at(a, 0))?;
    let base = usv_opt(d, s, a, 1)?;
    let msg = "Failed to construct 'URL': Invalid URL";
    let base = match base {
        Some(b) => match nu::parse_any(&b, None) {
            Ok(r) => Some(r),
            Err(e) => return Err(uerr(d, s, e, msg, false)),
        },
        None => None,
    };
    let rec = match nu::parse_any(&text, base.as_ref()) {
        Ok(r) => r,
        Err(e) => return Err(uerr(d, s, e, msg, false)),
    };
    new_url(d, s, rec, proto)
}

pub(crate) fn url_can_parse<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, 1, "canParse' on 'URL")?;
    let text = usv(d, s, at(a, 0))?;
    let base = usv_opt(d, s, a, 1)?;
    let base = match base {
        Some(b) => match nu::parse_any(&b, None) {
            Ok(r) => Some(r),
            Err(UErr::Invalid) => return Ok(s.boolean(false)),
            Err(e) => return Err(uerr(d, s, e, "", false)),
        },
        None => None,
    };
    match nu::parse_any(&text, base.as_ref()) {
        Ok(_) => Ok(s.boolean(true)),
        Err(UErr::Invalid) => Ok(s.boolean(false)),
        Err(e) => Err(uerr(d, s, e, "", false)),
    }
}

pub(crate) fn url_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, part: Part) -> Res<'s, Local<'s>> {
    let id = url_id(d, s, this)?;
    let text = part.get(&d.nav.s.borrow().urls[&id].rec);
    d.js_str_s(s, &text)
}

pub(crate) fn url_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, part: Part) -> Res<'s, Local<'s>> {
    let id = url_id(d, s, this)?;
    need(s, a, 1, "set a property on 'URL'")?;
    let v = usv(d, s, at(a, 0))?;
    let mut cand = d.nav.s.borrow().urls[&id].rec.clone();
    if part == Part::Href {
        cand = match nu::parse_any(&v, None) {
            Ok(r) => r,
            Err(e) => return Err(uerr(d, s, e, "Failed to set the 'href' property on 'URL': Invalid URL", false)),
        };
    } else if let Err(e) = part.set(&mut cand, &v) {
        return Err(uerr(d, s, e, "Invalid URL", false));
    }
    lift(s, fit(d, &cand))?;
    // The query may have changed: reload the live `searchParams` first (its caps may refuse), then keep the URL.
    let sp = d.nav.s.borrow().urls[&id].sp.as_ref().map(edge);
    if let (Some(sp), true) = (sp, matches!(part, Part::Href | Part::Search)) {
        lift(s, set_list(d, sp, form_parse(cand.query().unwrap_or(""))))?;
    }
    d.nav.s.borrow_mut().urls.get_mut(&id).expect("checked").rec = cand;
    Ok(s.undefined())
}

pub(crate) fn url_origin<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let id = url_id(d, s, this)?;
    let text = d.nav.s.borrow().urls[&id].rec.origin();
    d.js_str_s(s, &text)
}

pub(crate) fn url_search_params<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let id = url_id(d, s, this)?;
    let (sp, q) = {
        let n = d.nav.s.borrow();
        let u = &n.urls[&id];
        (u.sp.as_ref().map(edge), u.rec.query().unwrap_or("").to_string())
    };
    if let Some(sp) = sp {
        return Ok(d.obj_local(s, sp));
    }
    let (_, sp) = new_params(d, s, form_parse(&q), Some(this))?;
    let edge = s.trace_ref(sp);
    d.nav.s.borrow_mut().urls.get_mut(&id).expect("checked").sp = Some(edge);
    Ok(sp)
}

pub(crate) fn url_to_string<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    url_get(d, s, this, Part::Href)
}

// ------------------------------------------------------------------------------------------ URLSearchParams

#[inline(always)]
fn params_id<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    match obj_id(s, this) {
        Some(o) if d.nav.s.borrow().params.contains_key(&o) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn key_text(it: &mut Interp<'_>, k: PropertyKey) -> R<Option<String>> {
    Ok(match k {
        PropertyKey::Symbol(_) => None,
        PropertyKey::Index(i) => Some(i.to_string()),
        PropertyKey::String(s) => match it.heap.to_string_lossy(s) {
            Ok(s) => Some(s),
            Err(e) => return Err(it.heap_error(e)),
        },
    })
}

/// Room for one more pair of init (both caps, before the list grows). `bytes` is what this init holds so far.
fn init_room(d: &Dom, list: &[(String, String)], bytes: usize, add: usize) -> R<()> {
    let lim = d.nav.lim.get();
    if list.len() >= lim.max_params || d.nav.s.borrow().param_bytes.saturating_add(bytes).saturating_add(add) > lim.max_param_bytes {
        return Err(d.lim("url-params"));
    }
    Ok(())
}

const SEQ: &str = "Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.";

/// One element of a sequence init: an object that is iterable and holds exactly two values. At most three values are
/// read (a longer or endless iterable is a `TypeError`, and its iterator is closed).
fn init_pair(d: &Dom, it: &mut Interp<'_>, x: Value) -> R<(String, String)> {
    if !matches!(x, Value::Object(_)) {
        return Err(it.type_error(SEQ));
    }
    let Some(mut inner) = it.host_iter_open(x)? else { return Err(it.type_error(SEQ)) };
    let mut got = Vec::with_capacity(2);
    loop {
        let step = match it.host_iter_next(&mut inner) {
            Ok(Some(_)) if got.len() == 2 => {
                let e = it.type_error("Failed to construct 'URLSearchParams': Each pair must have exactly 2 elements.");
                return Err(it.host_iter_fail(&mut inner, e));
            }
            Ok(Some(v)) => usv_i(d, it, v).map(Some),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };
        match step {
            Ok(Some(s)) => got.push(s),
            Ok(None) => break,
            Err(e) => return Err(it.host_iter_fail(&mut inner, e)),
        }
    }
    match <[String; 2]>::try_from(got) {
        Ok([n, v]) => Ok((n, v)),
        Err(_) => Err(it.type_error("Failed to construct 'URLSearchParams': Each pair must have exactly 2 elements.")),
    }
}

/// One step of a sequence init. `false` at the end.
fn init_step(d: &Dom, it: &mut Interp<'_>, outer: &mut HostIter, list: &mut Vec<(String, String)>, bytes: &mut usize) -> R<bool> {
    let Some(x) = it.host_iter_next(outer)? else { return Ok(false) };
    init_room(d, list, *bytes, 0)?;
    let (n, v) = init_pair(d, it, x)?;
    let add = n.len() + v.len() + 32;
    init_room(d, list, *bytes, add)?;
    *bytes += add;
    list.push((n, v));
    Ok(true)
}

/// Pairs of a sequence of sequences. An error closes the open iterators (inner first), unless it is a limit.
fn init_sequence(d: &Dom, it: &mut Interp<'_>, mut outer: HostIter) -> R<Vec<(String, String)>> {
    let (mut list, mut bytes) = (Vec::new(), 0);
    loop {
        match init_step(d, it, &mut outer, &mut list, &mut bytes) {
            Ok(true) => {}
            Ok(false) => return Ok(list),
            Err(e) => return Err(it.host_iter_fail(&mut outer, e)),
        }
    }
}

/// A record: own enumerable keys in order. An enumerable symbol key is a `TypeError`, as `ToString(symbol)` is.
fn init_record(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<Vec<(String, String)>> {
    let cap = d.nav.lim.get().max_params;
    // A byte view lists one key for each byte: refuse a long one before the list is made.
    if it.heap.view_parts(o).is_some_and(|(_, _, len)| len > cap) {
        return Err(d.lim("url-params"));
    }
    let keys = match it.heap.own_keys(o) {
        Ok(k) => k,
        Err(e) => return Err(it.heap_error(e)),
    };
    let (mut list, mut bytes) = (Vec::new(), 0);
    for k in keys {
        it.charge(1)?;
        match it.heap.get_own_property(o, k) {
            Ok(Some(p)) if p.enumerable() => {}
            Ok(_) => continue,
            Err(e) => return Err(it.heap_error(e)),
        }
        let Some(name) = key_text(it, k)? else { return Err(it.type_error("Cannot convert a Symbol value to a string")) };
        init_room(d, &list, bytes, 0)?;
        let v = it.get_value(Value::Object(o), k)?;
        let v = usv_i(d, it, v)?;
        let add = name.len() + v.len() + 32;
        init_room(d, &list, bytes, add)?;
        bytes += add;
        list.push((name, v));
    }
    Ok(list)
}

/// `sequence<sequence<USVString>>` when the object has `Symbol.iterator` (read once), else a record.
fn init_from_object(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<Vec<(String, String)>> {
    match it.host_iter_open(Value::Object(o))? {
        Some(iter) => init_sequence(d, it, iter),
        None => init_record(d, it, o),
    }
}

pub(crate) fn params_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    // Chrome reads `Get(newTarget, "prototype")` before it converts the init.
    let proto = proto_of(d, s, d.nav.p.get().expect("installed").params)?;
    let init = a.get(0).unwrap_or_else(|| s.undefined());
    let list = match s.type_of(init) {
        Type::Undefined => Vec::new(),
        Type::Object | Type::Function => {
            let o = obj_id(s, init).expect("an object");
            old(s, |it| init_from_object(d, it, o))?
        }
        _ => {
            let t = usv(d, s, init)?;
            form_parse(t.strip_prefix('?').unwrap_or(&t))
        }
    };
    Ok(new_params_in(d, s, list, None, proto)?.1)
}

/// Set the list of `sp` and the byte count that goes with it.
fn store(d: &Dom, sp: ObjId, list: Vec<(String, String)>, bytes: usize) {
    let mut n = d.nav.s.borrow_mut();
    let n = &mut *n;
    let p = n.params.get_mut(&sp).expect("checked");
    n.param_bytes = n.param_bytes - p.bytes + bytes;
    p.bytes = bytes;
    p.list = list;
}

/// Change the list with `f` (no JS runs inside), then write it through to the URL. `f` returns the change in
/// bytes (`None`: count them again). A failed `f` or a URL that does not fit leaves the list as it was.
fn edit(d: &Dom, sp: ObjId, f: impl FnOnce(&mut Vec<(String, String)>) -> R<Option<isize>>) -> R<()> {
    let (mut list, old_bytes, attached) = {
        let mut n = d.nav.s.borrow_mut();
        let p = n.params.get_mut(&sp).expect("checked");
        (std::mem::take(&mut p.list), p.bytes, p.url.is_some())
    };
    let snap = attached.then(|| list.clone());
    let r = f(&mut list);
    match r {
        Ok(delta) => {
            let bytes = delta.map_or_else(|| pair_bytes(&list), |x| old_bytes.saturating_add_signed(x));
            store(d, sp, list, bytes);
        }
        Err(e) => {
            store(d, sp, list, old_bytes);
            return Err(e);
        }
    }
    if let Err(e) = commit_params(d, sp) {
        if let Some(snap) = snap {
            let bytes = pair_bytes(&snap);
            store(d, sp, snap, bytes);
        }
        return Err(e);
    }
    Ok(())
}

/// Room for one more pair of `n` bytes: both caps, before the list grows.
fn push(d: &Dom, sp: ObjId, list: &mut Vec<(String, String)>, n: String, v: String) -> R<Option<isize>> {
    let lim = d.nav.lim.get();
    let add = n.len() + v.len() + 32;
    if list.len() >= lim.max_params || d.nav.s.borrow().param_bytes + add > lim.max_param_bytes {
        return Err(d.lim("url-params"));
    }
    let _ = sp;
    list.push((n, v));
    Ok(Some(add as isize))
}

pub(crate) fn p_append<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 2, "append' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let v = usv(d, s, at(a, 1))?;
    lift(s, edit(d, sp, |l| push(d, sp, l, n, v)))?;
    Ok(s.undefined())
}

pub(crate) fn p_delete<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 1, "delete' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let v = usv_opt(d, s, a, 1)?;
    lift(
        s,
        edit(d, sp, |l| {
            l.retain(|(k, x)| !(*k == n && v.as_ref().is_none_or(|v| v == x)));
            Ok(None)
        }),
    )?;
    Ok(s.undefined())
}

pub(crate) fn p_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 1, "get' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let found = d.nav.s.borrow().params[&sp].list.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone());
    match found {
        Some(v) => d.js_str_s(s, &v),
        None => Ok(s.null()),
    }
}

pub(crate) fn p_get_all<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 1, "getAll' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let found: Vec<String> = d.nav.s.borrow().params[&sp].list.iter().filter(|(k, _)| *k == n).map(|(_, v)| v.clone()).collect();
    let mut vals = Vec::with_capacity(found.len());
    for v in &found {
        vals.push(d.js_str_s(s, v)?);
    }
    s.new_array(&vals)
}

pub(crate) fn p_has<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 1, "has' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let v = usv_opt(d, s, a, 1)?;
    let has = d.nav.s.borrow().params[&sp].list.iter().any(|(k, x)| *k == n && v.as_ref().is_none_or(|v| v == x));
    Ok(s.boolean(has))
}

pub(crate) fn p_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    need(s, a, 2, "set' on 'URLSearchParams")?;
    let n = usv(d, s, at(a, 0))?;
    let v = usv(d, s, at(a, 1))?;
    {
        let (cost, over) = {
            let state = d.nav.s.borrow();
            let p = &state.params[&sp];
            let removed: usize = p.list.iter().filter(|(k, _)| *k == n).map(|(k, v)| k.len() + v.len() + 32).sum();
            let added = n.len() + v.len() + 32;
            ((p.bytes / 32 + p.list.len()) as u64, state.param_bytes.saturating_sub(removed).saturating_add(added) > d.nav.lim.get().max_param_bytes)
        };
        s.charge(cost)?;
        if over {
            return Err(d.lim_s(s, "url-params"));
        }
    }
    lift(
        s,
        edit(d, sp, |l| {
            match l.iter().position(|(k, _)| *k == n) {
                Some(i) => {
                    l[i].1 = v;
                    let mut first = true;
                    l.retain(|(k, _)| *k != n || std::mem::replace(&mut first, false));
                    Ok(None)
                }
                None => push(d, sp, l, n, v),
            }
        }),
    )?;
    Ok(s.undefined())
}

pub(crate) fn p_sort<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    let len = d.nav.s.borrow().params[&sp].list.len();
    s.charge(len as u64 * 4)?;
    lift(
        s,
        edit(d, sp, |l| {
            l.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            Ok(Some(0))
        }),
    )?;
    Ok(s.undefined())
}

pub(crate) fn p_to_string<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    let text = form_serialize(&d.nav.s.borrow().params[&sp].list);
    s.charge((text.len() / 32) as u64)?;
    d.js_str_s(s, &text)
}

pub(crate) fn p_size<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    let n = d.nav.s.borrow().params[&sp].list.len();
    Ok(s.number(n as f64))
}

pub(crate) fn p_for_each<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let sp = params_id(d, s, this)?;
    let cb = arg_s(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'forEach' on 'URLSearchParams': parameter 1 is not of type 'Function'."));
    }
    let this_arg = arg_s(s, a, 1);
    let base = s.engine().stack_len();
    let mut i = 0;
    loop {
        s.charge(1)?;
        let pair = d.nav.s.borrow().params.get(&sp).and_then(|p| p.list.get(i).cloned());
        let Some((n, v)) = pair else { return Ok(s.undefined()) };
        let (v, n) = (d.js_str_s(s, &v)?, d.js_str_s(s, &n)?);
        s.call(cb, this_arg, &[v, n, this])?;
        // The locals of a step are dead: a long list does not pile them up.
        s.engine().stack_truncate(base);
        i += 1;
    }
}

fn make_iter<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, kind: u8) -> Res<'s, Local<'s>> {
    params_id(d, s, this)?;
    lift(s, room(d))?;
    let o = d.obj_s(s, d.nav.p.get().expect("installed").iter)?;
    let l = d.obj_local(s, o);
    let sp = s.trace_ref(this);
    d.nav.s.borrow_mut().iters.insert(o, IterObj { sp, idx: 0, kind });
    own(d, s, l, Owner::Iter(o))?;
    Ok(l)
}

pub(crate) fn p_keys<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 0)
}
pub(crate) fn p_values<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 1)
}
pub(crate) fn p_entries<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 2)
}

pub(crate) fn iter_next<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let id = match obj_id(s, this) {
        Some(o) if d.nav.s.borrow().iters.contains_key(&o) => o,
        _ => return Err(s.type_error("Illegal invocation")),
    };
    s.charge(1)?;
    let item = {
        let mut n = d.nav.s.borrow_mut();
        let n = &mut *n;
        let i = n.iters.get_mut(&id).expect("checked");
        let pair = n.params.get(&edge(&i.sp)).and_then(|p| p.list.get(i.idx).cloned());
        match pair {
            Some(p) => {
                i.idx += 1;
                Some((i.kind, p))
            }
            None => None,
        }
    };
    // The old calls are cheaper here: the keys are cached and an item is one native step (no scope entry per step).
    old_v(s, |it| {
        let (value, done) = match item {
            None => (Value::Undefined, true),
            Some((0, (n, _))) => (d.js_str(it, &n)?, false),
            Some((1, (_, v))) => (d.js_str(it, &v)?, false),
            Some((_, (n, v))) => {
                let (n, v) = (d.js_str(it, &n)?, d.js_str(it, &v)?);
                (it.array_from(&[n, v])?, false)
            }
        };
        let o = d.obj(it, it.object_prototype())?;
        let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
        it.define_data(o, kv, value, true, true, true)?;
        it.define_data(o, kd, Value::Bool(done), true, true, true)?;
        Ok(Value::Object(o))
    })
}

//! Lazy globals. A global that is an accessor on `window` until the first read or write: a realm that never uses the interface pays for
//! the accessor function only, not for the prototypes and methods (`browser_abort.rs` was the first user).
//!
//! The read makes the interface (`ensure`, once) and turns that one global into a data property. The other globals of the same group stay
//! accessors until they are read too. A write defines a plain data property (`window.X = 1` works as in a browser). Deviation: until the first
//! read, `Object.getOwnPropertyDescriptor(window, 'X')` shows an accessor, not a data property.

use crate::browser::*;
use crate::browser_scoped::{old, Res, Sc, HS};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Attrs, HostFn, Local};

/// The accessor body: the setter when there is an argument, else the getter.
pub(crate) fn lazy<'s>(d: &Dom, s: &mut Sc<'s, '_>, name: &str, a: Args<'s>, ensure: fn(&Dom, &mut Interp<'_>) -> R<()>, pick: fn(&Dom) -> ObjId) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, name, v, attrs)?;
        return Ok(s.undefined());
    }
    old(s, |it| ensure(d, it))?;
    let ctor = d.obj_local(s, pick(d));
    s.define_str(g, name, ctor, attrs)?;
    Ok(ctor)
}

/// Define each name as a lazy accessor of `window`.
pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>, list: &[(&str, HS)]) -> Res<'s, ()> {
    let g = s.global();
    for &(name, h) in list {
        let f = d.add_fn_s(s, h, name, 0, HostFn::Method)?;
        s.define_accessor_str(g, name, Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })?;
    }
    Ok(())
}

/// `fn $f` is the accessor body of the global `$name`: `$ensure` makes the interface, `$pick` gives its constructor.
macro_rules! lazy_global {
    ($f:ident, $name:literal, $ensure:path, $pick:expr) => {
        fn $f<'s>(d: &Dom, s: &mut $crate::browser_scoped::Sc<'s, '_>, _: bare_js_api::Local<'s>, a: bare_js_api::Args<'s>) -> $crate::browser_scoped::Res<'s, bare_js_api::Local<'s>> {
            crate::browser_lazy::lazy(d, s, $name, a, $ensure, $pick)
        }
    };
}
pub(crate) use lazy_global;

/// `proto[Symbol.toStringTag] = name`, so `Object.prototype.toString` says `[object name]`.
pub(crate) fn tag(d: &Dom, it: &mut Interp<'_>, proto: ObjId, name: &str) -> R<()> {
    let g = it.global_object();
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sy, kt)? {
        // Not `js_str`: a constant of the setup is not script data, and a small `max_string_bytes` must not stop the browser.
        let t = Value::String(it.heap.intern_str(name).map_err(|e| it.heap_error(e))?);
        it.define_data(proto, bare_js::value::PropertyKey::Symbol(s), t, false, false, true)?;
    }
    Ok(())
}

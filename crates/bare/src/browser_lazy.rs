//! Lazy globals. A global that is an accessor on `window` until the first read or write: a realm that never uses the interface pays for
//! the accessor function only, not for the prototypes and methods (`browser_abort.rs` was the first user).
//!
//! The read makes the interface (`ensure`, once) and turns that one global into a data property. The other globals of the same group stay
//! accessors until they are read too. A write defines a plain data property (`window.X = 1` works as in a browser). Deviation: until the first
//! read, `Object.getOwnPropertyDescriptor(window, 'X')` shows an accessor, not a data property.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::heap::PropertyDescriptor;
use bare_js::value::{ObjId, Value};

/// The accessor body: the setter when there is an argument, else the getter.
pub(crate) fn lazy(d: &Dom, it: &mut Interp<'_>, name: &str, a: &[Value], ensure: fn(&Dom, &mut Interp<'_>) -> R<()>, pick: fn(&Dom) -> ObjId) -> R<Value> {
    let g = *d.global.get().expect("installed");
    let k = d.key(it, name)?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    ensure(d, it)?;
    let ctor = pick(d);
    it.define_data(g, k, Value::Object(ctor), true, false, true)?;
    Ok(Value::Object(ctor))
}

/// Define each name as a lazy accessor of `window`.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, list: &[(&str, H)]) -> R<()> {
    let g = *d.global.get().expect("installed");
    for &(name, h) in list {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, h, name, 0, false)?;
        let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        if let Err(e) = it.heap.define_own_property(g, key, &desc) {
            return Err(it.heap_error(e));
        }
    }
    Ok(())
}

/// `fn $f` is the accessor body of the global `$name`: `$ensure` makes the interface, `$pick` gives its constructor.
macro_rules! lazy_global {
    ($f:ident, $name:literal, $ensure:path, $pick:expr) => {
        fn $f(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
            crate::browser_lazy::lazy(d, it, $name, a, $ensure, $pick)
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

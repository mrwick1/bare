//! Lazy globals. A global that is an accessor on `window` until the first read or write: a realm that never uses the interface pays for
//! the accessor function only, not for the prototypes and methods (`browser_abort.rs` was the first user).
//!
//! The read makes the interface (`ensure`, once) and turns that one global into a data property. The other globals of the same group stay
//! accessors until they are read too. A write defines a plain data property (`window.X = 1` works as in a browser). Deviation: until the first
//! read, `Object.getOwnPropertyDescriptor(window, 'X')` shows an accessor, not a data property.

use crate::eng::{Eng, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::{old, Res, Sc};
use bare_js_api::{Args, Attrs, Local};

/// The accessor body: the setter when there is an argument, else the getter.
pub(crate) fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str, a: Args<'s>, ensure: fn(&Dom<E>, &mut E::Vm) -> R<E, ()>, pick: fn(&Dom<E>) -> E::Obj) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, name, v, attrs)?;
        return Ok(s.undefined());
    }
    old::<E, _>(s, |it| ensure(d, it))?;
    let ctor = d.obj_local(s, pick(d));
    s.define_str(g, name, ctor, attrs)?;
    Ok(ctor)
}

/// `fn $f` is the accessor body of the global `$name`: `$ensure` makes the interface, `$pick` gives its constructor.
macro_rules! lazy_global {
    ($f:ident, $name:literal, $ensure:path, $pick:expr) => {
        fn $f<'s, E: Eng>(d: &Dom<E>, s: &mut $crate::browser_scoped::Sc<'s, E>, _: bare_js_api::Local<'s>, a: bare_js_api::Args<'s>) -> $crate::browser_scoped::Res<'s, bare_js_api::Local<'s>> {
            crate::browser_lazy::lazy(d, s, $name, a, $ensure, $pick)
        }
    };
}
pub(crate) use lazy_global;

/// `proto[Symbol.toStringTag] = name`, so `Object.prototype.toString` says `[object name]`.
pub(crate) fn tag<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, name: &str) -> R<E, ()> {
    let g = it.global_object();
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(E::obj_raw(g), ks)?;
    if let Val::Symbol(s) = E::kind(it.get_value(sy, kt)?) {
        // Not `js_str`: a constant of the setup is not script data, and it is not charged.
        let t = E::str_raw(it.h_intern(name)?);
        it.define_data(proto, E::sym_key(s), t, false, false, true)?;
    }
    Ok(())
}

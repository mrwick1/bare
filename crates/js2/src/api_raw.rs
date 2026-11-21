//! Raw-value helpers of the interface, for a host that keeps `Value`s and not `Local`s (`crates/bare/src/eng_new.rs`). Each of
//! these is one read of the realm that `Engine` does through a handle. They are in this file so that the host can read a value
//! with `&self`.

use crate::interp_rt::Realm;
use crate::object::Obj;
use crate::string::{Chars, Str};
use crate::value::Value;
use bare_js_api::Intrinsic;
use std::borrow::Cow;

fn units_of(s: Str, out: &mut Vec<u16>) {
    let s = s.resolve();
    match s.chars() {
        Some(Chars::One(b)) => out.extend(b.iter().map(|&x| x as u16)),
        Some(Chars::Two(u)) => out.extend_from_slice(u),
        None => {
            for c in s.children().into_iter().flatten() {
                units_of(c, out);
            }
        }
    }
}

impl Realm {
    /// The UTF-16 code units of a string value (or of a symbol description cell). A rope is read without a flatten. `None` when `v` is no string.
    pub fn raw_units(&self, v: Value) -> Option<Cow<'static, [u16]>> {
        let s = Realm::str_of(v)?.resolve();
        let mut out = Vec::new();
        units_of(s, &mut out);
        Some(Cow::Owned(out))
    }

    /// An intrinsic of the realm as a value.
    pub fn raw_intrinsic(&self, which: Intrinsic) -> Value {
        match which {
            Intrinsic::ObjectPrototype => self.object_proto.value(),
            Intrinsic::ArrayPrototype => self.array_proto.value(),
            Intrinsic::ErrorPrototype => self.error_proto.value(),
            // Made at the first ask (`Engine::intrinsic`); a `&self` read has the object prototype until then.
            Intrinsic::IteratorPrototype => self.object_proto.value(),
            Intrinsic::SymbolIterator => self.wk[crate::bi_symbol::ITERATOR].key_value(),
            Intrinsic::SymbolToStringTag => self.wk[crate::bi_symbol::TO_STRING_TAG].key_value(),
        }
    }

    /// Can `v` be called?
    pub fn raw_callable(&self, v: Value) -> bool {
        Realm::obj(v).is_some_and(|o: Obj| o.is_callable())
    }

    /// The state of a promise: 0 pending, 1 fulfilled, 2 rejected, and the value. `None` for a value that is no promise.
    pub fn raw_promise(&self, v: Value) -> Option<(i32, Value)> {
        Realm::promise_status(self, v)
    }

    /// The `new.target` of the host call that runs (`None` for a plain call).
    pub fn raw_new_target(&self) -> Option<Value> {
        Some(self.nt).filter(|v| !v.is_undefined())
    }
}

impl Realm {
    /// The host key of a host-filled object (`Engine::host_key` for a value).
    pub fn raw_host_key(&self, v: Value) -> Option<u32> {
        self.hs_key(v)
    }
}

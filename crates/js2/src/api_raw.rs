//! Raw-value helpers of the interface, for a host that keeps `Value`s and not `Local`s (`crates/bare/src/eng_new.rs`). Each of
//! these is one read of the realm that `Engine` does through a handle. They are in this file so that the host can read a value
//! with `&self`.

use crate::api::Clock;
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

/// JS objects, string cells and symbols released by an explicit collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawCollected {
    pub objects: usize,
    pub strings: usize,
    pub symbols: usize,
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
            Intrinsic::IteratorPrototype => self.host_iterator_prototype(),
            Intrinsic::SymbolIterator => self.wk[crate::bi_symbol::ITERATOR].key_value(),
            Intrinsic::SymbolToStringTag => self.wk[crate::bi_symbol::TO_STRING_TAG].key_value(),
        }
    }

    /// Can `v` be called?
    pub fn raw_callable(&self, v: Value) -> bool {
        Realm::obj(v).is_some_and(|o: Obj| o.is_callable())
    }

    /// Read the description from the symbol's own string cell.
    /// The caller uses this value for a string read and keeps the symbol alive while it does so.
    pub fn raw_symbol_description(&self, v: Value) -> Option<Option<Value>> {
        let s = Realm::sym_of(v)?;
        Some((!s.symbol_has_no_desc()).then(|| s.value()))
    }

    /// Can the runtime construct `v`?
    pub fn raw_constructor(&self, v: Value) -> bool {
        self.is_constructor(v)
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
    /// Collect now. The host is between tasks: no script runs and no host call is on the stack. `extra` are values that the caller still holds.
    pub fn raw_collect(&mut self, extra: &[Value]) -> RawCollected {
        self.api.freed = Some(RawCollected::default());
        self.collect(&mut [], |m| extra.iter().for_each(|v| m.value(*v)));
        self.api.external_since = 0;
        self.api.freed.take().expect("explicit collection")
    }

    pub fn raw_external_live(&self) -> usize {
        self.api.external_live
    }

    pub fn raw_set_external_live(&mut self, bytes: usize) {
        self.api.external_live = bytes;
    }

    pub fn raw_add_external(&mut self, bytes: usize) {
        self.api.external_since = self.api.external_since.saturating_add(bytes);
    }

    pub fn raw_clock(&self) -> &Clock {
        &self.api.clock
    }

    /// Select a wall clock. An invalid virtual time leaves the clock unchanged.
    pub fn raw_set_clock(&mut self, clock: Clock) -> bool {
        if let Clock::Virtual(t) = clock
            && !(t.is_finite() && t.fract() == 0.0 && t.abs() <= crate::bi_date_calc::MAX_TIME)
        {
            return false;
        }
        self.api.clock = clock;
        true
    }

    pub(crate) fn clock_now_ms(&self) -> f64 {
        match &self.api.clock {
            Clock::Virtual(t) => *t,
            Clock::Host(Some(now)) => now(),
            Clock::System | Clock::Host(None) => match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_millis() as f64,
                Err(e) => -(e.duration().as_millis() as f64),
            },
        }
    }

    /// The host key of a host-filled object (`Engine::host_key` for a value).
    pub fn raw_host_key(&self, v: Value) -> Option<u32> {
        self.hs_key(v)
    }
}

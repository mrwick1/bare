//! Owners, host-filled objects and lazy members of the interface (`docs/design-js2-host-api.md`, section 12).
//!
//! **Owners.** `Engine::set_host_owner` puts the cell of an object in [`HostState::owners`] with its host key. The
//! marker (`gc.rs`) has no test for owners. After the mark loop, `collect` asks [`crate::api::ApiState::trace_owners`] in rounds: each
//! round lists the owners that are marked and not traced yet ([`HostState::reached`]), calls `Host::trace` for them, marks the edges
//! and drains, until a round finds none. After the marking, [`HostState::prune`] removes the
//! entries of unmarked cells and the host's `Host::prune` gets their keys once.
//!
//! **Host-filled objects.** `new_host_indexed` and `new_host_named` make a plain object with `F_HOST` and one hidden property
//! (`\0host`, slot 0, no attributes) that holds `kind << 32 | key`. The slow paths of property access ask the host through
//! [`Realm::hs_walk`] (reads and `in`), [`Realm::get_own_h`], [`Realm::own_key_list_h`] and [`Realm::hs_write_blocked`]. None of them
//! costs anything while `HostState::active` is false. An inline cache never fills for a host object (`get_field_slow`,
//! `set_field_slow`), and the shape of such an object has the hidden key, so no other object shares it.
//!
//! **Lazy members.** `set_lazy` sets `F_LAZYM`. A read that misses on a marked object asks `Host::lazy_member` once for each (object,
//! name) until the next collection, and then reads again.
use crate::bi_object::Own;
use crate::cell::Layout;
use crate::gc::{AddrHasher, Live, Marker};
use crate::interp::STOP;
use crate::interp_rt::{Got, Key, R, Realm, Slot};
use crate::object::{F_HOST, F_LAZYM, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W};
use crate::string::{Chars, Str};
use crate::value::{Kind, Value};
use bare_js_api::{Abort, Engine, Error, Host, Item, Lazy, Local, Named, Pending};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::ptr::NonNull;

const INDEXED: u64 = 1;
const NAMED: u64 = 2;
const NAMED_OPEN: u64 = 3;

/// What the interface keeps for owners, host-filled objects and lazy members.
#[derive(Default)]
pub(crate) struct HostState {
    /// The host key of each owner, by the address of its cell. A new key for the same cell replaces the old one.
    pub(crate) owners: HashMap<NonNull<u8>, u32, BuildHasherDefault<AddrHasher>>,
    /// The owners that the marking of this collection has not shown reached yet (cell, key): refilled at round 0.
    waiting: Vec<(NonNull<u8>, u32)>,
    /// A host-filled or lazy object was made: the slow paths of property access look at the flags of the chain.
    pub(crate) active: bool,
    /// The name `\0host` (made at the first host-filled object).
    k_host: Option<Str>,
    /// The (object, kind, name) triples that `Host::lazy_member` answered since the last collection.
    asked: HashSet<(usize, u8, u64)>,
}

impl HostState {
    pub(crate) fn mark(&self, m: &mut Marker) {
        if let Some(k) = self.k_host {
            m.value(k.value());
        }
    }

    /// The keys of the owners that are marked now and were not at an earlier round of this collection. Round 0 lists every owner first.
    pub(crate) fn reached(&mut self, live: &Live, round: u32) -> Vec<u32> {
        if round == 0 {
            self.waiting.clear();
            self.waiting.extend(self.owners.iter().map(|(&p, &k)| (p, k)));
        }
        let mut keys = Vec::new();
        let mut i = 0;
        while i < self.waiting.len() {
            let (p, k) = self.waiting[i];
            // SAFETY: an owner is a cell of this heap that no collection freed (`prune`).
            if unsafe { live.value(Value::pointer(Kind::Object, p)) } {
                keys.push(k);
                self.waiting.swap_remove(i);
            } else {
                i += 1;
            }
        }
        keys
    }

    /// Remove the owners that the marking did not reach. Gives their keys. Also forget the lazy asks: an address may name another
    /// object after the sweep.
    pub(crate) fn prune(&mut self, live: &Live) -> Vec<u32> {
        self.asked.clear();
        let mut dead = Vec::new();
        self.owners.retain(|&p, &mut k| {
            // SAFETY: an owner is a cell of this heap. A dead one is removed in the collection that frees it, so the map holds no freed cell.
            let alive = unsafe { live.value(Value::pointer(Kind::Object, p)) };
            if !alive {
                dead.push(k);
            }
            alive
        });
        dead
    }
}

/// What a walk of the prototype chain found.
pub(crate) enum Walk {
    /// No object of the chain is host-filled or lazy: the ordinary lookup answers.
    NoHost,
    Hit(Got),
    Miss,
}

impl Realm {
    /// Make `o` an owner with host key `key`.
    pub(crate) fn hs_owner(&mut self, o: Obj, key: u32) {
        self.api.hs.owners.insert(o.cell(), key);
    }

    fn hs_host<'a>(&self) -> Option<&'a dyn Host<Realm>> {
        // SAFETY: as in `api::host_call`: the pointer is into the `Rc` of `api.host`, which `set_host` does not replace while a host call runs.
        self.api.host_ptr().map(|h| unsafe { h.as_ref() })
    }

    /// The host stopped a read: the reason is recorded and the run stops (`STOP`), never an empty answer.
    fn hs_pend<T>(&mut self, r: Result<T, Pending>) -> R<T> {
        r.map_err(|p| {
            self.api.set_abort(match p {
                Pending::Busy => Abort::Limit("the host is busy"),
                Pending::Fill(_) => Abort::Unsupported("the host fill step"),
            });
            STOP
        })
    }

    fn hs_fill_missing<T>(&mut self) -> R<T> {
        self.api.set_abort(Abort::Unsupported("the host fill step"));
        Err(STOP)
    }

    /// The kind and the host key of a host-filled object.
    fn hs_info(&self, o: Obj) -> Option<(u64, u32)> {
        if !o.has_flag(F_HOST) {
            return None;
        }
        let n = o.slot(&self.shapes, 0).as_f64()? as u64;
        Some((n >> 32, n as u32))
    }

    /// `Engine::host_key`.
    pub(crate) fn hs_key(&self, v: Value) -> Option<u32> {
        self.hs_info(Realm::obj(v)?).map(|i| i.1)
    }

    /// `new_host_indexed` (`kind` 1) and `new_host_named` (2, or 3 for an open one).
    pub(crate) fn hs_new<'s>(&mut self, proto: Option<Local<'_>>, kind: u64, key: u32) -> Result<Local<'s>, Error<'s>> {
        let proto = match proto {
            Some(p) => Some(self.a_obj(p)?),
            None => None,
        };
        let k = match self.api.hs.k_host {
            Some(k) => k,
            None => {
                let k = self.intern("\0host");
                self.api.hs.k_host = Some(k);
                k
            }
        };
        let root = self.shapes.root(Layout::Object, proto.map(Obj::cell), 4);
        let shape = self.shapes.add(root, k, 0).expect("shape");
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_slot(&self.shapes, 0, Realm::num(((kind << 32) | key as u64) as f64));
        o.set_flag(F_HOST);
        self.api.hs.active = true;
        Ok(self.a_push(o.value()))
    }

    /// `set_lazy`: mark `o`, and its prototype chain when `chain`.
    pub(crate) fn hs_set_lazy(&mut self, o: Obj, chain: bool) {
        self.api.hs.active = true;
        let mut c = Some(o);
        while let Some(x) = c {
            x.set_flag(F_LAZYM);
            c = if chain { self.proto_of(x) } else { None };
        }
    }

    /// The code units of a key, for the host. `None` for a symbol (never a name).
    fn hs_units(&self, key: Key) -> Option<Vec<u16>> {
        match key {
            Key::Idx(i) => Some(i.to_string().encode_utf16().collect()),
            Key::Name(k) if k.is_symbol() => None,
            Key::Name(k) => match k.resolve().chars()? {
                Chars::One(b) => Some(b.iter().map(|&x| x as u16).collect()),
                Chars::Two(u) => Some(u.to_vec()),
            },
        }
    }

    /// A string value of code units.
    fn hs_str(&mut self, u: &[u16]) -> Value {
        let s = if u.iter().all(|&x| x < 256) {
            let b: Vec<u8> = u.iter().map(|&x| x as u8).collect();
            Str::new(&mut self.heap, Chars::One(&b))
        } else {
            Str::new(&mut self.heap, Chars::Two(u))
        };
        s.expect("memory").value()
    }

    /// The own ordinary property `key` of `c`: no host, no lazy.
    fn hs_plain(&mut self, c: Obj, key: Key) -> Option<Slot> {
        let k = match key {
            Key::Idx(i) => {
                if c.is_indexed()
                    && let Some(v) = c.get_element(i)
                {
                    return Some(Slot::Data(v));
                }
                self.idx_atom(i)
            }
            Key::Name(k) => {
                if k == self.k_length && self.shapes.get(c.shape(&self.shapes)).layout == Layout::Indexed {
                    return Some(Slot::Data(Realm::num(c.len() as f64)));
                }
                self.fn_prep(c, k);
                k
            }
        };
        let e = self.shapes.find(c.shape(&self.shapes), k)?;
        Some(if e.attrs() & A_ACC != 0 {
            Slot::Acc(c.slot(&self.shapes, e.slot()), c.slot(&self.shapes, e.slot() + 1))
        } else {
            Slot::Data(c.slot(&self.shapes, e.slot()))
        })
    }

    /// Does `key` name an ordinary property of `c` or of its prototype chain? (An open named object does not answer such a name.)
    fn hs_plain_chain(&mut self, c: Obj, key: Key) -> bool {
        let mut x = Some(c);
        while let Some(o) = x {
            self.fn_props(o);
            if self.hs_plain(o, key).is_some() {
                return true;
            }
            x = self.proto_of(o);
        }
        false
    }

    /// The property at `index` of the indexed object `c`.
    fn hs_item(&mut self, c: Obj, hk: u32, index: u32, want: bool) -> R<Option<Value>> {
        let Some(h) = self.hs_host() else { return Ok(None) };
        let r = h.indexed_item(&c.value(), hk, index as usize);
        match self.hs_pend(r)? {
            Item::Absent => Ok(None),
            Item::Value(v) => Ok(Some(v)),
            Item::Missing if want => self.hs_fill_missing(),
            Item::Missing => Ok(Some(Value::UNDEFINED)),
        }
    }

    /// The named property `key` of the named object `c`.
    fn hs_named(&mut self, c: Obj, hk: u32, key: Key, want: bool) -> R<Option<Value>> {
        let (Some(h), Some(u)) = (self.hs_host(), self.hs_units(key)) else { return Ok(None) };
        let r = h.named_has(&c.value(), hk, &u);
        if !self.hs_pend(r)? {
            return Ok(None);
        }
        if !want {
            return Ok(Some(Value::UNDEFINED));
        }
        let r = h.named_get(&c.value(), hk, &u);
        Ok(match self.hs_pend(r)? {
            Some(Named::Text(t)) => Some(self.hs_str(&t)),
            Some(Named::Value(v)) => Some(v),
            None => None,
        })
    }

    /// The index that `key` is a non-enumerable alias of, on the indexed object `c`: only when no object of the chain has the name.
    fn hs_alias(&mut self, c: Obj, hk: u32, key: Key) -> R<Option<u32>> {
        let (Some(h), Some(u)) = (self.hs_host(), self.hs_units(key)) else { return Ok(None) };
        if self.hs_plain_chain(c, key) {
            return Ok(None);
        }
        let r = h.indexed_name(&c.value(), hk, &u);
        self.hs_pend(r)
    }

    /// Ask `Host::lazy_member` for `what` on `c`, once for each (object, name) until the next collection. True when the host was asked.
    fn hs_lazy(&mut self, c: Obj, key: Option<Key>) -> R<bool> {
        let (kind, tag, name) = match key {
            None => (0, 0, String::new()),
            Some(Key::Idx(i)) => (1, i as u64, i.to_string()),
            Some(Key::Name(k)) if k.is_symbol() => (2, 0, String::new()),
            Some(Key::Name(k)) => (3, k.value().as_pointer().map_or(0, |p| p.1.as_ptr().addr() as u64), self.text(k)),
        };
        if !self.api.hs.asked.insert((c.cell().as_ptr().addr(), kind, tag)) {
            return Ok(false);
        }
        let Some(h) = self.hs_host() else { return Ok(false) };
        let what = match kind {
            0 => Lazy::All,
            2 => Lazy::Symbols,
            _ => Lazy::Name(&name),
        };
        let slot = self.stack_len();
        self.a_push(c.value());
        self.nest += 1;
        let held = self.heap.hold_task();
        let r = bare_js_api::enter(&mut *self, |s| match h.lazy_member(s, Local::from_slot(slot), what) {
            Ok(()) => Ok(()),
            Err(Error::Throw(l)) => Err(s.engine().a_at(l)),
            Err(Error::Abort(a)) => {
                s.engine().api.set_abort(a);
                Err(STOP)
            }
        });
        self.heap.release_task(held);
        self.nest -= 1;
        self.stack_truncate(slot);
        r.map(|()| true)
    }

    fn hs_chain_hosty(&self, o: Obj) -> bool {
        let mut c = Some(o);
        while let Some(x) = c {
            if x.has_flag(F_HOST | F_LAZYM) {
                return true;
            }
            c = self.proto_of(x);
        }
        false
    }

    /// `o[key]` (`want`) or `key in o` through a chain that has a host-filled or a lazy object. At each object: the host's own
    /// properties first (an index of an indexed object, a name of a closed named object), then the ordinary own property, then a lazy
    /// member. After the chain: the names of open named objects, then the aliases of indexed objects.
    pub(crate) fn hs_walk(&mut self, o: Obj, key: Key, want: bool) -> R<Walk> {
        if !self.hs_chain_hosty(o) {
            return Ok(Walk::NoHost);
        }
        let mut open: Vec<(Obj, u32)> = Vec::new();
        let mut c = Some(o);
        while let Some(x) = c {
            self.fn_props(x);
            if let Some((kind, hk)) = self.hs_info(x) {
                match (kind, key) {
                    (INDEXED, Key::Idx(i)) => {
                        if let Some(v) = self.hs_item(x, hk, i, want)? {
                            return Ok(Walk::Hit(Got::Val(v)));
                        }
                    }
                    (NAMED, _) => {
                        if let Some(v) = self.hs_named(x, hk, key, want)? {
                            return Ok(Walk::Hit(Got::Val(v)));
                        }
                    }
                    (NAMED_OPEN, _) => open.push((x, hk)),
                    _ => {}
                }
            }
            let mut p = self.hs_plain(x, key);
            if p.is_none() && x.has_flag(F_LAZYM) && self.hs_lazy(x, Some(key))? {
                p = self.hs_plain(x, key);
            }
            if p.is_some() {
                return Ok(Walk::Hit(Realm::got(p)));
            }
            c = self.proto_of(x);
        }
        for (x, hk) in open {
            if let Some(v) = self.hs_named(x, hk, key, want)? {
                return Ok(Walk::Hit(Got::Val(v)));
            }
        }
        if matches!(key, Key::Name(_)) {
            let mut c = Some(o);
            while let Some(x) = c {
                if let Some((INDEXED, hk)) = self.hs_info(x)
                    && let Some(i) = self.hs_alias(x, hk, key)?
                {
                    let v = if want { self.hs_item(x, hk, i, true)?.unwrap_or(Value::UNDEFINED) } else { Value::UNDEFINED };
                    return Ok(Walk::Hit(Got::Val(v)));
                }
                c = self.proto_of(x);
            }
        }
        Ok(Walk::Miss)
    }

    /// `get_own` that asks the host first. Index properties of an indexed object are not writable; a name of a named object is a
    /// writable data property (`heap_named.rs`); an alias is not enumerable.
    pub(crate) fn get_own_h(&mut self, o: Obj, key: Key) -> R<Option<Own>> {
        if !self.api.hs.active || !o.has_flag(F_HOST | F_LAZYM) {
            return Ok(self.get_own(o, key));
        }
        let info = self.hs_info(o);
        match (info, key) {
            (Some((INDEXED, hk)), Key::Idx(i)) => {
                if let Some(v) = self.hs_item(o, hk, i, true)? {
                    return Ok(Some(Own::Data(v, A_E | A_C)));
                }
            }
            (Some((NAMED, hk)), _) => {
                if let Some(v) = self.hs_named(o, hk, key, true)? {
                    return Ok(Some(Own::Data(v, A_W | A_E | A_C)));
                }
            }
            _ => {}
        }
        let mut p = self.get_own(o, key);
        if p.is_none() && o.has_flag(F_LAZYM) && self.hs_lazy(o, Some(key))? {
            p = self.get_own(o, key);
        }
        if p.is_some() {
            return Ok(p);
        }
        match info {
            Some((NAMED_OPEN, hk)) if !self.hs_plain_chain(o, key) => {
                Ok(self.hs_named(o, hk, key, true)?.map(|v| Own::Data(v, A_W | A_E | A_C)))
            }
            Some((INDEXED, hk)) if matches!(key, Key::Name(_)) => match self.hs_alias(o, hk, key)? {
                Some(i) => Ok(self.hs_item(o, hk, i, true)?.map(|v| Own::Data(v, A_C))),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// `own_key_list` with the keys of the host in front: the indices of an indexed object (and its aliases, when not only the
    /// enumerable keys are asked), the names of a named object. A lazy object lets the host define all its members first.
    pub(crate) fn own_key_list_h(&mut self, o: Obj, enumerable: bool) -> R<Vec<Value>> {
        if !self.api.hs.active || !o.has_flag(F_HOST | F_LAZYM) {
            return Ok(self.own_key_list(o, enumerable));
        }
        if o.has_flag(F_LAZYM) {
            self.hs_lazy(o, None)?;
        }
        let mut out = Vec::new();
        if let (Some((kind, hk)), Some(h)) = (self.hs_info(o), self.hs_host()) {
            let v = o.value();
            match kind {
                INDEXED => {
                    let n = h.indexed_len(&v, hk);
                    let n = self.hs_pend(n)?;
                    out.extend((0..n).map(|i| self.hs_str(&i.to_string().encode_utf16().collect::<Vec<_>>())));
                    if !enumerable {
                        let names = h.indexed_names(&v, hk);
                        for u in self.hs_pend(names)? {
                            out.push(self.hs_str(&u));
                        }
                    }
                }
                _ => {
                    let names = h.named_keys(&v, hk);
                    for u in self.hs_pend(names)? {
                        out.push(self.hs_str(&u));
                    }
                }
            }
        }
        out.extend(self.own_key_list(o, enumerable));
        Ok(out)
    }

    /// Does the host refuse a write, define or delete of `key` on the host-filled object `o`? An indexed object has no setter, so any
    /// index is refused; a named object refuses a name that it has (the trait has no write hook).
    pub(crate) fn hs_write_blocked(&mut self, o: Obj, key: Key) -> R<bool> {
        let Some((kind, hk)) = self.hs_info(o) else { return Ok(false) };
        Ok(match kind {
            INDEXED => matches!(key, Key::Idx(_)),
            NAMED_OPEN if self.hs_plain_chain(o, key) => false,
            _ => self.hs_named(o, hk, key, false)?.is_some(),
        })
    }

    /// `clear_lazy`.
    pub(crate) fn hs_clear_lazy(&mut self, o: Obj) {
        o.clear_flag(F_LAZYM);
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
#[path = "api_host_tests.rs"]
mod tests;

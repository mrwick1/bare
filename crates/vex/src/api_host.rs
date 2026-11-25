//! Owners, host-filled objects and lazy members of the interface (`docs/internal/design-js2-host-api.md`, section 12).
//!
//! **Owners.** `Engine::set_host_owner` puts the cell of an object in [`HostState::owners`] with its host key. The
//! marker (`gc.rs`) has no test for owners. After the mark loop, `collect` asks [`crate::api::ApiState::trace_owners`] in rounds: each
//! round lists the owners that are marked and not traced yet ([`HostState::reached`]), calls `Host::trace` for them, marks the edges
//! and drains, until a round finds none. After the marking, [`HostState::prune`] removes the
//! entries of unmarked cells and the host's `Host::prune` gets their keys once.
//!
//! **Host-filled objects.** `new_host_indexed` and `new_host_named` make a plain object with `F_HOST` and one hidden property
//! (`\0host`, slot 0, no attributes) that holds `kind << 32 | key`. The slow paths of property access ask the host through
//! [`Realm::hs_walk`] (reads and `in`), [`Realm::get_own_h`], [`Realm::own_key_list_h`] and [`Realm::hs_write`] (the host's `named_write`).
//! A read that stops with `Pending::Fill` or `Item::Missing` runs `Host::fill` and reads again (`Realm::hs_ask`). None of them costs
//! anything while `HostState::active` is false. An inline cache never fills for a host object (`get_field_slow`, `set_field_slow`), and
//! the shape of such an object has the hidden key, so no other object shares it.
//!
//! **Lazy members.** `set_lazy` sets `F_LAZYM`. A property lookup that misses on a marked object asks `Host::lazy_member` once for each (object,
//! name) until the next collection, and then reads again.
use crate::bi_object::{Desc, Own};
use crate::cell::Layout;
use crate::gc::{AddrHasher, Live, Marker};
use crate::interp::STOP;
use crate::interp_rt::{Got, Key, R, Realm, Slot};
use crate::object::{K_PROXY, K_TYPED};
use crate::object::{F_HOST, F_LAZYM, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W};
use crate::string::{Chars, Str};
use crate::value::{Kind, Value};
use bare_js_api::{Abort, Descriptor, Engine, Error, Host, Item, Lazy, Local, Named, Pending, Scope, Write};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::ptr::NonNull;

const INDEXED: u64 = 1;
const NAMED: u64 = 2;
const NAMED_OPEN: u64 = 3;
/// The asks of one read: the receiver, a prototype and a lazy name (as `MAX_FILLS` of the old engine). One more is a host bug.
const MAX_FILLS: u32 = 3;
const ASK_ALL: (u8, u64) = (0, 0);

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

/// What a script does to a host-filled object ([`Realm::hs_write`]).
#[derive(Clone, Copy)]
pub(crate) enum HsWrite<'a> {
    Set(Value),
    Define(&'a Desc),
    Delete,
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

    /// Run the hook of the host `f`. While it answers `Pending::Fill`, run the fill step for `o` and ask again. `Pending::Busy` stops the
    /// run (`STOP`), never an empty answer, and so does a host that is still not ready after `MAX_FILLS` fills.
    fn hs_ask<T>(&mut self, o: Obj, hk: u32, mut f: impl FnMut() -> Result<T, Pending>) -> R<T> {
        for fills in 0..=MAX_FILLS {
            match f() {
                Ok(v) => return Ok(v),
                Err(Pending::Fill(i)) if fills < MAX_FILLS => self.hs_fill(o, hk, i)?,
                Err(Pending::Fill(_)) => break,
                Err(Pending::Busy) => {
                    self.api.set_abort(Abort::Limit("the host is busy"));
                    return Err(STOP);
                }
            }
        }
        self.hs_unfilled()
    }

    #[cold]
    fn hs_unfilled<T>(&mut self) -> R<T> {
        self.api.set_abort(Abort::Limit("the host fill step made nothing"));
        Err(STOP)
    }

    /// Run a hook of the host that may run script, with the values `vals` on the handle stack from slot `base`. No collection runs meanwhile.
    fn hs_scope<T>(&mut self, vals: &[Value], f: impl for<'s> FnOnce(&mut Scope<'s, Realm>, u32) -> Result<T, Error<'s>>) -> R<T> {
        let base = self.stack_len();
        for &v in vals {
            self.a_push(v);
        }
        self.nest += 1;
        let held = self.heap.hold_task();
        let r = bare_js_api::enter(&mut *self, |s| match f(s, base) {
            Ok(x) => Ok(x),
            Err(Error::Throw(l)) => Err(s.engine().a_at(l)),
            Err(Error::Abort(a)) => {
                s.engine().api.set_abort(a);
                Err(STOP)
            }
        });
        self.heap.release_task(held);
        self.nest -= 1;
        self.stack_truncate(base);
        r
    }

    /// The fill step of the host for `index` of the object `o`.
    #[cold]
    fn hs_fill(&mut self, o: Obj, hk: u32, index: u32) -> R<()> {
        let Some(h) = self.hs_host() else { return self.hs_unfilled() };
        self.hs_scope(&[o.value()], |s, base| h.fill(s, Local::from_slot(base), hk, index))
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
                self.find_index_name(c, i)?
            }
            Key::Name(k) => {
                if k == self.k_length && Realm::is_array(self, c) {
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
        let v = c.value();
        for fills in 0..=MAX_FILLS {
            match self.hs_ask(c, hk, || h.indexed_item(&v, hk, index as usize))? {
                Item::Absent => return Ok(None),
                Item::Value(x) => return Ok(Some(x)),
                Item::Missing if !want => return Ok(Some(Value::UNDEFINED)),
                Item::Missing if fills < MAX_FILLS => self.hs_fill(c, hk, index)?,
                Item::Missing => break,
            }
        }
        self.hs_unfilled()
    }

    /// The named property `key` of the named object `c`.
    fn hs_named(&mut self, c: Obj, hk: u32, key: Key, want: bool) -> R<Option<Value>> {
        let (Some(h), Some(u)) = (self.hs_host(), self.hs_units(key)) else { return Ok(None) };
        let v = c.value();
        if !self.hs_ask(c, hk, || h.named_has(&v, hk, &u))? {
            return Ok(None);
        }
        if !want {
            return Ok(Some(Value::UNDEFINED));
        }
        Ok(match self.hs_ask(c, hk, || h.named_get(&v, hk, &u))? {
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
        let v = c.value();
        self.hs_ask(c, hk, || h.indexed_name(&v, hk, &u))
    }

    /// A new enumeration would skip the host callback for this object in the current collection cycle.
    pub(crate) fn hs_all_asked(&self, o: Obj) -> bool {
        self.api.hs.asked.contains(&(o.cell().as_ptr().addr(), ASK_ALL.0, ASK_ALL.1))
    }

    fn hs_lazy_name(&self, key: Key) -> String {
        #[cfg(test)]
        lazy_name_tests::CONVERSIONS.with(|n| n.set(n.get() + 1));
        match key {
            Key::Idx(i) => i.to_string(),
            Key::Name(k) => self.text(k),
        }
    }

    /// Ask `Host::lazy_member` for `what` on `c`, once for each (object, name) until the next collection. True when the host was asked.
    pub(crate) fn hs_lazy(&mut self, c: Obj, key: Option<Key>) -> R<bool> {
        let (kind, tag) = match key {
            None => ASK_ALL,
            Some(Key::Idx(i)) => (1, i as u64),
            Some(Key::Name(k)) if k.is_symbol() => (2, 0),
            Some(Key::Name(k)) => (3, k.value().as_pointer().map_or(0, |p| p.1.as_ptr().addr() as u64)),
        };
        if !self.api.hs.asked.insert((c.cell().as_ptr().addr(), kind, tag)) {
            return Ok(false);
        }
        let Some(h) = self.hs_host() else { return Ok(false) };
        let name;
        let what = match kind {
            0 => Lazy::All,
            2 => Lazy::Symbols,
            _ => {
                name = self.hs_lazy_name(key.expect("a named ask has a key"));
                Lazy::Name(&name)
            }
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
        let mut again = true;
        while let Some(x) = c {
            if x.is_kind(K_PROXY) {
                return if want {
                    self.proxy_get(x, key, o.value()).map(|v| Walk::Hit(Got::Val(v)))
                } else {
                    self.proxy_has(x, key).map(|has| if has { Walk::Hit(Got::Val(Value::UNDEFINED)) } else { Walk::Miss })
                };
            }
            if x.is_kind(K_TYPED) && let Some(i) = crate::bi_typed::numeric_key(key) {
                let Some(i) = i.filter(|&i| i < self.typed_len(x)) else { return Ok(Walk::Miss) };
                let v = if want { self.typed_get(x, i)?.unwrap_or(Value::UNDEFINED) } else { Value::UNDEFINED };
                return Ok(Walk::Hit(Got::Val(v)));
            }
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
                // The host may put the member on an object nearer in the chain (an interface prototype, asked from the end of the
                // chain). Read the chain again, once: the objects before `x` were asked already, so they are not asked twice.
                if p.is_none() && x != o && std::mem::take(&mut again) {
                    open.clear();
                    c = Some(o);
                    continue;
                }
            }
            if p.is_some() {
                return self.got(p, key, o.value()).map(Walk::Hit);
            }
            // A plain function makes its `prototype` at the first read of it (as `lookup` and `get_prop_g` do).
            if matches!(key, Key::Name(k) if k == self.k_prototype) && self.needs_prototype(x) {
                return Ok(Walk::Hit(Got::Val(self.make_prototype(x)?)));
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
        if o.is_kind(K_PROXY) {
            return self.proxy_descriptor(o, key);
        }
        if o.is_kind(K_TYPED) && let Some(i) = crate::bi_typed::numeric_key(key) {
            let Some(i) = i else { return Ok(None) };
            let attrs = if self.typed_immutable(o) { A_E } else { A_W | A_E | A_C };
            return Ok(self.typed_get(o, i)?.map(|v| Own::Data(v, attrs)));
        }
        self.prepare_own_prototype(o, key)?;
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
        if o.is_kind(K_PROXY) {
            let keys = self.proxy_keys(o)?;
            let mut strings = Vec::new();
            for v in keys.into_iter().filter(|v| Self::str_of(*v).is_some()) {
                if enumerable {
                    let k = self.key_of(v)?;
                    if !self.get_own_h(o, k)?.is_some_and(|d| d.attrs() & A_E != 0) {
                        continue;
                    }
                }
                strings.push(v);
            }
            return Ok(strings);
        }
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
                    let n = self.hs_ask(o, hk, || h.indexed_len(&v, hk))?;
                    out.extend((0..n).map(|i| self.hs_str(&i.to_string().encode_utf16().collect::<Vec<_>>())));
                    if !enumerable {
                        for u in self.hs_ask(o, hk, || h.indexed_names(&v, hk))? {
                            out.push(self.hs_str(&u));
                        }
                    }
                }
                _ => {
                    for u in self.hs_ask(o, hk, || h.named_keys(&v, hk))? {
                        out.push(self.hs_str(&u));
                    }
                }
            }
        }
        out.extend(self.own_key_list(o, enumerable));
        Ok(out)
    }

    /// A write, define or delete of `key` on the host-filled object `o` (the receiver). `Some(answer)`: the host handled it (`false`: it refused).
    /// `None`: the ordinary rules run (a symbol, an open object with an ordinary property of that name, a delete of a name the host has not).
    /// An indexed object has no write hook: its indices are refused.
    pub(crate) fn hs_write(&mut self, o: Obj, key: Key, op: HsWrite<'_>) -> R<Option<bool>> {
        let Some((kind, hk)) = self.hs_info(o) else { return Ok(None) };
        if kind == INDEXED {
            return Ok(matches!(key, Key::Idx(_)).then_some(false));
        }
        let (Some(h), Some(u)) = (self.hs_host(), self.hs_units(key)) else { return Ok(None) };
        if kind == NAMED_OPEN && self.hs_plain_chain(o, key) {
            return Ok(None);
        }
        let v = o.value();
        if matches!(op, HsWrite::Delete) && !self.hs_ask(o, hk, || h.named_has(&v, hk, &u))? {
            return Ok(None);
        }
        let vals: Vec<Value> = match op {
            HsWrite::Set(x) => vec![v, x],
            HsWrite::Define(d) => [Some(v), d.value, d.get, d.set].into_iter().flatten().collect(),
            HsWrite::Delete => vec![v],
        };
        let flags = match op {
            HsWrite::Define(d) => Some((d.w, d.e, d.c, d.value.is_some(), d.get.is_some(), d.set.is_some())),
            _ => None,
        };
        let is_set = matches!(op, HsWrite::Set(_));
        self.hs_scope(&vals, |s, base| {
            let mut next = base;
            let mut take = |on: bool| {
                on.then(|| {
                    next += 1;
                    Local::from_slot(next)
                })
            };
            let object = Local::from_slot(base);
            let w = match flags {
                Some((writable, enumerable, configurable, value, get, set)) => {
                    Write::Define(Descriptor { value: take(value), get: take(get), set: take(set), writable, enumerable, configurable })
                }
                None if is_set => Write::Set(Local::from_slot(base + 1)),
                None => Write::Delete,
            };
            h.named_write(s, object, hk, &u, w)
        })
        .map(Some)
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

#[cfg(test)]
#[path = "api_host_typed_index_tests.rs"]
mod typed_index_tests;

#[cfg(test)]
#[path = "api_host_index_probe_tests.rs"]
mod index_probe_tests;

#[cfg(test)]
#[path = "api_host_lazy_name_tests.rs"]
mod lazy_name_tests;

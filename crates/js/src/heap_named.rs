//! Host named properties: an object whose own string-keyed properties are answered by the host. This is the WebIDL
//! "legacy platform object" with named properties and `[LegacyOverrideBuiltIns]` (`DOMStringMap`). A child module of `heap.rs`.
//!
//! The object is an exotic object (`Exotic::Named`, flags `exotic` and `named`). It holds no data: the host keeps the truth,
//! so every answer is live. The heap asks the host by `owner`, the object that was asked. A key is a string key or an array
//! index (`ds[7]` is the name `"7"`). A symbol key is never a name: it follows the ordinary rules.
//!
//! One hook for each internal method, all behind flags that an ordinary object never has:
//!
//! | Internal method | Dispatch point (all in `heap.rs` unless noted) | Answer |
//! |---|---|---|
//! | `[[GetOwnProperty]]`, `get`, `lookup` | `get_own_property`, `lookup`, `peek_own_property` via `exotic_own` (`heap_binary.rs`, `named_has`), then `realize` for the value | `NamedSource::named_has`, then `named_get`: a writable, enumerable, configurable data property. An unknown name falls through to the ordinary own properties and the prototype chain. |
//! | `[[HasProperty]]`, `hasOwnProperty` | `has_property`, `has_own_property` via `exotic_own` | `NamedSource::named_has` |
//! | `[[OwnPropertyKeys]]` | `own_keys` | `NamedSource::named_keys`, in front of the ordinary keys |
//! | `[[Set]]` (receiver is the object) | `exotic_set` (`heap_binary.rs`) | request `NamedOp::Set` |
//! | `[[DefineOwnProperty]]` | `define_own_property` | request `NamedOp::Define` |
//! | `[[Delete]]` | `delete` | request `NamedOp::Delete`, only for a name that is supported |
//!
//! Why two kinds of answer. A read must not run script, and every read path of the heap is `&self` or has no evaluator, so
//! the host answers at once (`NamedSource`, like `IndexedSource`). A write can throw a `DOMException`, can run the `toString` of
//! the value, must charge the step budget and must obey the host caps. Only the evaluator can do that. So the heap does not
//! change anything: it stores a `NamedRequest`, answers `HeapError::NeedsNamed`, and the evaluator (`hxw!`, the write form of `hx!`) calls
//! `Host::named_write`, gives the boolean back (`complete_named`) and repeats the operation. The repeat finds the answer and
//! returns it. `[[Set]]` is `Done` or `Rejected`, `define` and `delete` return the boolean. An operation that is not behind `hxw!`
//! (`h!`) gets `Error::Heap(NeedsNamed)`, never a silent success.
//!
//! Rules of WebIDL that this follows: `[[Set]]` runs the named setter only when the receiver is the object itself; with another
//! receiver (the object as a prototype) the ordinary algorithm runs and creates an own property on the receiver. `[[DefineOwnProperty]]`
//! of a data descriptor runs the setter whatever its flags say; an accessor descriptor is the host's to refuse (`false`).
//! `[[Delete]]` of an unsupported name is `true` with no change. Named properties come first in `own_keys` and are enumerable.
//! `Object.freeze` and `Object.seal` of an object that has names return `false` (the same rule as a byte view with bytes).
//!
//! Not done: `[LegacyOverrideBuiltIns]` is always on (an own name hides the prototype chain). An interface without it (a named
//! property that must not hide a prototype property) needs a visibility check on the chain. `[[PreventExtensions]]` is not
//! forced to `false`. `peek_own_property` gives a placeholder `undefined` value for a name (its callers read only the flags).
//!
//! `Proxy` is built (`heap_proxy.rs`). It does not use the request slot: a proxy is another exotic flag (`EXOTIC_PROXY`) tested at the
//! same points, and each internal method answers `HeapError::Proxy(p)` and changes nothing. The evaluator runs the trap, checks the
//! invariants against the target and calls the heap again for the target. A proxy needs no request record, because its answer is
//! made in the evaluator and never has to go back through the heap.

use super::binary::Exotic;
use super::*;

/// The read side of a host named object (`Heap::new_named`). The host keeps the truth. The methods get `&self`, must be
/// cheap, must not allocate JavaScript objects and must not run JavaScript. `owner` is the object that was asked, which can be
/// a prototype of the object that the page touched. A key is UTF-16 code units. `Err` means the host cannot answer now (for
/// example its state is borrowed); the error stops the script and is never an empty answer.
pub trait NamedSource {
    /// Is `key` the name of a property of `owner` now? `Ok(false)`: no, or `owner` is not a live object of this host.
    fn named_has(&self, owner: ObjId, key: &[u16]) -> R<bool>;
    /// The value of the property `key` of `owner` now. `Ok(None)`: there is none. The heap calls it only right after `named_has`
    /// answered `true` for the same key, so a host that charges for a long scan has charged already and must not charge again.
    fn named_get(&self, owner: ObjId, key: &[u16]) -> R<Option<Box<[u16]>>>;
    /// How many names `owner` has now (cheap: no list). The evaluator charges it before `named_keys` runs.
    fn named_len(&self, owner: ObjId) -> R<usize>;
    /// The names of `owner` now, in the host's order.
    fn named_keys(&self, owner: ObjId) -> R<Vec<Box<[u16]>>>;
    /// Does a name hide the properties of the prototype chain (`[LegacyOverrideBuiltIns]`)? `false` (`Storage`): a string name
    /// is not an own property of `owner` when `owner` or its chain has an ordinary property of that name. A write, a define and
    /// a delete of such a name follow the ordinary rules and make or change an ordinary property. A name that is an array
    /// index never hides. The names stay in `named_keys` (so in `Reflect.ownKeys`). An object with `false` refuses
    /// `preventExtensions`, `seal` and `freeze` (`named_open`).
    fn named_masking(&self, _owner: ObjId) -> bool {
        true
    }
}

/// What a write asks of the host.
#[derive(Clone, Copy, Debug)]
pub enum NamedOp {
    Set(Value),
    Define(PropertyDescriptor),
    Delete,
}

/// A write that waits for the host (see `HeapError::NeedsNamed`). `key` is a string key or an index, never a symbol.
#[derive(Clone, Copy, Debug)]
pub struct NamedRequest {
    pub owner: ObjId,
    pub key: PropertyKey,
    pub op: NamedOp,
}

/// What the heap keeps for named objects. It is one box, so the heap grows by one pointer: the layout of the hot fields stays put.
#[derive(Default)]
pub(super) struct NamedState {
    src: Option<std::rc::Rc<dyn NamedSource>>,
    req: Option<NamedRequest>,
    done: Option<(ObjId, PropertyKey, bool)>,
}

impl Heap {
    /// An object whose string-keyed own properties come from the host (`set_named_source`). It starts with no names of its own.
    pub fn new_named(&mut self, proto: Option<ObjId>) -> R<ObjId> {
        let id = self.make_exotic(proto, || Exotic::Named)?;
        self.objects.get_mut(id.index, id.gn).expect("fresh object").exotic = EXOTIC_NAMED;
        Ok(id)
    }

    /// Install the host that answers for every `new_named` object of this heap. The heap owns the `Rc` (see `set_indexed_source`).
    pub fn set_named_source(&mut self, source: std::rc::Rc<dyn NamedSource>) {
        self.named.src = Some(source);
    }

    fn named_source(&self) -> R<std::rc::Rc<dyn NamedSource>> {
        self.named.src.clone().ok_or(HeapError::HostUnavailable)
    }

    /// The key as UTF-16 units. `None` for a symbol (never a name).
    fn key_units(&self, key: PropertyKey) -> R<Option<std::borrow::Cow<'_, [u16]>>> {
        Ok(match key {
            PropertyKey::String(s) => Some(std::borrow::Cow::Borrowed(self.str_units(s)?)),
            PropertyKey::Index(i) => Some(std::borrow::Cow::Owned(i.to_string().encode_utf16().collect())),
            PropertyKey::Symbol(_) => None,
        })
    }

    /// `[[GetOwnProperty]]` without the value, for the `&self` paths (`in`, `hasOwnProperty`, `peek_own_property`): a name is
    /// a data property whose value is a placeholder. `None`: not a name, the ordinary rules apply.
    pub(super) fn named_exists(&self, o: ObjId, key: PropertyKey) -> R<Option<Option<Property>>> {
        let Some(units) = self.key_units(key)? else { return Ok(None) };
        if !self.named_source()?.named_has(o, &units)? || self.named_hidden(o, key)? {
            return Ok(None);
        }
        Ok(Some(Some(Property::Data { value: Value::Undefined, writable: true, enumerable: true, configurable: true })))
    }

    /// The answer of `exotic_own` for `key` of `o`, with the real value when `o` is a named object. `exotic_own` takes `&self`,
    /// so for a name it gives a placeholder value; a real value needs the interner. Only a hit comes here. The paths that miss
    /// do not change, so an ordinary lookup costs what it cost before names existed.
    /// `realize` for `lookup`, which needs the holder with the property. It is a call that ends `lookup`, so the loop of `lookup`
    /// keeps no value alive across it.
    #[cold]
    #[inline(never)]
    pub(super) fn realize_hit(&mut self, o: ObjId, key: PropertyKey, r: Option<Property>) -> R<Option<(ObjId, Property)>> {
        Ok(self.realize(o, key, r)?.map(|p| (o, p)))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn realize(&mut self, o: ObjId, key: PropertyKey, r: Option<Property>) -> R<Option<Property>> {
        if r.is_none() {
            return Ok(r);
        }
        match self.obj(o)?.exotic {
            EXOTIC_NAMED => {}
            EXOTIC_STR => return self.prim_realize(o, key, r),
            _ => return Ok(r),
        }
        let Some(units) = self.key_units(key)? else { return Ok(r) };
        let Some(text) = self.named_source()?.named_get(o, &units)? else { return Ok(r) };
        let value = Value::String(self.intern_units(&text)?);
        Ok(Some(Property::Data { value, writable: true, enumerable: true, configurable: true }))
    }

    pub(super) fn named_len(&self, o: ObjId) -> R<usize> {
        self.named_source()?.named_len(o)
    }

    /// The names of `o` as keys, interned. The memory for the list is checked first.
    pub(super) fn named_keys(&mut self, o: ObjId) -> R<Vec<PropertyKey>> {
        let src = self.named_source()?;
        self.charge(src.named_len(o)?.saturating_mul(std::mem::size_of::<PropertyKey>()))?;
        src.named_keys(o)?.iter().map(|k| self.intern_units(k).and_then(|s| self.key_from_string(s))).collect()
    }

    /// Is the string name `key` hidden on `o` (see `NamedSource::named_masking`)? Then the ordinary rules apply to it.
    pub(super) fn named_hidden(&self, o: ObjId, key: PropertyKey) -> R<bool> {
        if !matches!(key, PropertyKey::String(_)) || self.named.src.as_ref().is_none_or(|s| s.named_masking(o)) {
            return Ok(false);
        }
        let ob = self.obj(o)?;
        if self.own(ob, key).is_some() {
            return Ok(true);
        }
        match ob.proto {
            Some(p) => self.has_property(p, key),
            None => Ok(false),
        }
    }

    /// Is `o` a host named object that refuses `preventExtensions`, `seal` and `freeze` (`named_masking` is `false`)?
    pub fn named_open(&self, o: ObjId) -> bool {
        matches!(self.obj(o), Ok(ob) if ob.exotic == EXOTIC_NAMED) && self.named.src.as_ref().is_some_and(|s| !s.named_masking(o))
    }

    /// Ask the host to write. The first call stores the request and answers `NeedsNamed`; the call after the evaluator
    /// completed it returns the answer.
    pub(super) fn named_write(&mut self, o: ObjId, key: PropertyKey, op: NamedOp) -> R<bool> {
        if let Some((owner, k, ok)) = self.named.done.take() && owner == o && k == key {
            return Ok(ok);
        }
        self.named.req = Some(NamedRequest { owner: o, key, op });
        Err(HeapError::NeedsNamed)
    }

    /// `[[Delete]]` of a name: `Some(answer)` when the host handles it, `None` for a key that is not a name.
    pub(super) fn named_delete(&mut self, o: ObjId, key: PropertyKey) -> R<Option<bool>> {
        if let Some((owner, k, ok)) = self.named.done.take_if(|(owner, k, _)| *owner == o && *k == key) {
            let _ = (owner, k);
            return Ok(Some(ok));
        }
        let Some(units) = self.key_units(key)? else { return Ok(None) };
        if !self.named_source()?.named_has(o, &units)? || self.named_hidden(o, key)? {
            return Ok(None);
        }
        self.named_write(o, key, NamedOp::Delete).map(Some)
    }

    /// The request that `NeedsNamed` announced. The evaluator takes it, runs the host, and calls `complete_named`.
    pub fn take_named_request(&mut self) -> Option<NamedRequest> {
        self.named.req.take()
    }

    /// Give the host's answer for `req`. The next identical operation on the same object and key returns it.
    pub fn complete_named(&mut self, req: &NamedRequest, ok: bool) {
        self.named.done = Some((req.owner, req.key, ok));
    }
}

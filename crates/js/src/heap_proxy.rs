//! `Proxy` objects in the heap. A child module of `heap.rs`.
//!
//! A proxy is an exotic object (`Exotic::Proxy`, flag `EXOTIC_PROXY`) with no properties and no prototype of its own. Its payload
//! is `(target, handler)`, or `None` after `revoke`. The collector marks both from the payload (`Heap::collect`).
//!
//! The heap cannot run a trap: a trap is script, and the heap has no evaluator. So every internal method that meets a proxy
//! changes nothing and answers `HeapError::Proxy(p)`, where `p` is the proxy it met. The dispatch points are the ones that
//! already test `exotic != 0` (`exotic_own`, `exotic_set`) and the few entry points that read a flag of the object itself
//! (`get_prototype_of`, `set_prototype_of`, `is_extensible`, `prevent_extensions`, `is_array`, `own_keys`, `define_own_property`,
//! `set_integrity`, `test_integrity`). An ordinary object takes none of these branches, so it pays nothing. The evaluator
//! (`eval_proxy.rs`) continues the internal method from `p`: it runs the trap, checks the invariants against the target, and
//! calls the heap again for the target.
//!
//! A proxy in the middle of a prototype chain is found by the chain walks themselves (`lookup`, `has_property`), because the
//! walk calls `exotic_own` for each object. The evaluator then redoes the operation from the first object with the generic
//! algorithm of the specification (`o_get`, `o_set`, `o_has_property`), which is cold and handles the receiver.
//!
//! `callable` is not stored here. The evaluator keeps `Callee::Proxy` in its function table for a proxy whose target is callable.

use super::binary::Exotic;
use super::*;

impl Heap {
    /// A proxy for `target` with `handler`. Both are objects (the caller checked the types, as `ProxyCreate` does).
    pub fn new_proxy(&mut self, target: ObjId, handler: ObjId) -> R<ObjId> {
        self.obj(target)?;
        self.obj(handler)?;
        let id = self.make_exotic(None, || Exotic::Proxy(Some((target, handler))))?;
        self.objects.get_mut(id.index, id.gn).expect("fresh object").exotic = EXOTIC_PROXY;
        Ok(id)
    }

    /// Is `o` a proxy (revoked or not)? A stale handle is not.
    #[inline]
    pub fn is_proxy(&self, o: ObjId) -> bool {
        self.objects.get(o.index, o.gn).is_some_and(|ob| ob.exotic == EXOTIC_PROXY)
    }

    /// `None`: not a proxy. `Some(None)`: a revoked proxy. `Some(Some((target, handler)))`: a live proxy.
    pub fn proxy_parts(&self, o: ObjId) -> R<Option<Option<(ObjId, ObjId)>>> {
        if self.obj(o)?.exotic != EXOTIC_PROXY {
            return Ok(None);
        }
        Ok(match self.exotics.get(&o.index) {
            Some(Exotic::Proxy(p)) => Some(*p),
            _ => None,
        })
    }

    /// `revoke`: the proxy drops its target and handler for good. Revoking twice, or a non-proxy, does nothing.
    pub fn revoke_proxy(&mut self, o: ObjId) -> R<()> {
        self.obj(o)?;
        if let Some(Exotic::Proxy(p)) = self.exotics.get_mut(&o.index) {
            *p = None;
        }
        Ok(())
    }

    /// `IsCompatiblePropertyDescriptor(extensible, d, current)`: could `d` be applied to `current` without a TypeError at
    /// the next `[[DefineOwnProperty]]`? It is `ValidateAndApplyPropertyDescriptor` with no object, so nothing is stored.
    pub fn is_compatible_descriptor(&self, extensible: bool, d: &PropertyDescriptor, current: Option<Property>) -> bool {
        validate_apply(extensible, current, d, |a, b| self.str_eq(a, b)).is_some()
    }
}

impl PropertyDescriptor {
    /// `CompletePropertyDescriptor`: fill the absent fields with their defaults. A descriptor with `get` or `set` is an
    /// accessor; any other is a data property (a generic descriptor too).
    pub fn complete(&self) -> Property {
        let (enumerable, configurable) = (self.enumerable.unwrap_or(false), self.configurable.unwrap_or(false));
        if self.is_accessor() {
            Property::Accessor { get: self.get.flatten(), set: self.set.flatten(), enumerable, configurable }
        } else {
            Property::Data { value: self.value.unwrap_or(Value::Undefined), writable: self.writable.unwrap_or(false), enumerable, configurable }
        }
    }

    /// Does the descriptor have `get` or `set` (`IsAccessorDescriptor`)?
    pub fn has_accessor_field(&self) -> bool {
        self.is_accessor()
    }

    /// Does the descriptor have `value` or `writable` (`IsDataDescriptor`)?
    pub fn has_data_field(&self) -> bool {
        self.is_data()
    }
}

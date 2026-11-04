// Embedder hooks. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// One realm has at most one [`Host`]. The host owns Rust-side state (a DOM, for example). JavaScript reaches it
// through function objects made by [`Interp::host_function`]. Each one carries a number, the method id. When
// JavaScript calls it, the evaluator checks the depth and step guards (like any call), then calls
// `Host::call(it, id, this, args, is_new)`. Getters and setters are ordinary accessor properties whose functions
// are host functions, so `el.id` runs through the same path.
//
// Reentrancy: the evaluator clones the `Rc<dyn Host>` before the call. The host gets `&self` and the interpreter
// `&mut Interp`, so it may call back into JavaScript (`it.call`) or ask for properties (`it.get_value`) while
// it runs. A host that keeps state in `RefCell`s must release every borrow before it does that.
//
// GC contract: the heap cannot see inside the host. `Interp::collect` asks the host three questions:
// - `roots`: values that live as long as the realm (prototypes, constructors, the attached tree).
// - `trace(owner, out)`: for every object the collector reaches, the values that object keeps alive on the Rust
//   side (a wrapper keeps its listeners' callbacks, for example). It is called once per reached object.
// - `prune(live)`: after marking, the host drops metadata of owners that were not reached. A handle is
//   generation checked, so a dropped owner can never alias a new object.
// Nothing is collected during a call, so a host may hold handles in Rust locals while it runs.
// With a host installed, use Interp::collect. Heap::collect(Interp::roots()) does not follow opaque host edges.

/// An open iteration over a JavaScript iterable (see [`Interp::host_iter_open`]). Opaque. It lives in Rust locals for
/// the length of one host call: nothing is collected during a call, so it needs no rooting. Do not keep it past the call.
pub struct HostIter(It);

/// What an embedder implements. See the header of this file.
pub trait Host {
    /// Method `id` (from [`Interp::host_function`]) was called with `this` and `args`. `is_new` is true for
    /// `new f(...)` (only when the function was made constructible). The result of a constructor call
    /// is the new object.
    fn call(&self, it: &mut Interp<'_>, id: u32, this: Value, args: &[Value], is_new: bool) -> Result<Value, Error>;
    /// Values the host keeps for the whole life of the realm.
    fn roots(&self, _out: &mut Vec<Value>) {}
    /// Values that `owner` keeps alive through host state. Called for every object the collector reaches.
    fn trace(&self, _owner: ObjId, _out: &mut Vec<Value>) {}
    /// Drop metadata of every owner for which `live` is false. Called once per collection, before the sweep.
    fn prune(&self, _live: &dyn Fn(ObjId) -> bool) {}
    /// A `queueMicrotask` callback threw `thrown`. Record it without running JavaScript that can fail
    /// (read data properties only). The queue goes on. The default drops the value.
    fn report_exception(&self, _it: &mut Interp<'_>, _thrown: Value) {}
}

impl<'p> Interp<'p> {
    /// Install the host of this realm. Only one host per realm: a second call is `Unsupported`.
    pub fn set_host(&mut self, host: Rc<dyn Host>) -> Result<(), Error> {
        if self.host.is_some() {
            return Err(unsupported("second host in one realm", Span::default()));
        }
        self.host = Some(host);
        Ok(())
    }

    /// A function object that calls host method `id`. `ctor` allows `new`. The function has no `prototype`:
    /// a constructor's owner defines it. Counts toward `max_functions`.
    pub fn host_function(&mut self, id: u32, name: &str, len: u32, ctor: bool) -> Result<ObjId, Error> {
        let o = self.new_func_object(Callee::Host { id, ctor })?;
        let s = h!(self, self.heap.intern_str(name));
        self.def(o, self.k_length, Value::Number(len as f64), false, false, true)?;
        self.def(o, self.k_name, Value::String(s), false, false, true)?;
        Ok(o)
    }

    fn call_host(&mut self, id: u32, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let Some(host) = self.host.clone() else { return Err(unsupported("host function without a host", Span::default())) };
        host.call(self, id, this, args, is_new)
    }

    /// Charge `n` steps of work (a traversal the host does for JavaScript). `Limit(Steps)` when the budget ends.
    /// Outside a call it uses the budget of the next `run`, so call it only while a host method runs.
    pub fn charge(&mut self, n: u64) -> Result<(), Error> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > self.step_limit { Err(Error::Limit(LimitKind::Steps)) } else { Ok(()) }
    }

    /// Run `f` as one host operation (a driver dispatching an event, for example). It starts the step budget if no
    /// call is active, counts as one call level, and refuses `collect`, `run_script` and `drain_microtasks`
    /// until it ends. Every JavaScript call inside shares the one budget.
    pub fn host_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        if self.depth == 0 {
            self.begin();
        }
        if self.depth >= self.limits.max_call_depth {
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// A microtask checkpoint from inside a top-level [`Interp::host_scope`] (a driver dispatching an event: the JS stack
    /// is empty between listeners). It uses the budget of the scope. Inside nested JS (`depth` above one) it does nothing.
    pub fn host_checkpoint(&mut self) -> Result<usize, Error> {
        if self.depth != 1 {
            return Ok(0);
        }
        self.depth = 0;
        let r = self.drain_jobs();
        self.depth = 1;
        r
    }

    /// A new pending promise for the host to settle later (a network request, for example). It has no resolving
    /// functions: the host calls [`Interp::host_resolve_promise`] or [`Interp::host_reject_promise`] exactly once.
    /// Root it (in `Host::roots`) while it waits. Counts toward `max_promises`.
    pub fn host_new_promise(&mut self) -> Result<Value, Error> {
        Ok(Value::Object(self.new_promise()?))
    }

    /// Resolve a promise made by [`Interp::host_new_promise`], with the promise resolve function rules (a thenable is
    /// assimilated in a job). The host calls this or `host_reject_promise` once. Not a promise: `Unsupported`.
    /// Call it while a host scope is active. Reactions run at the next checkpoint, never here.
    pub fn host_resolve_promise(&mut self, p: Value, v: Value) -> Result<(), Error> {
        let o = self.host_promise(p)?;
        self.resolve_promise(o, v)
    }

    /// A function that calls function `target` with `this` fixed, as `bind` does but not through the script-visible
    /// `Function.prototype.bind`. The host uses it to carry one value into a promise reaction. The collector traces
    /// `target` and `this`. Counts toward `max_functions`. `target` must be a function object.
    pub fn host_bind(&mut self, target: ObjId, this: Value) -> Result<ObjId, Error> {
        if !self.funcs.contains_key(&target) {
            return Err(self.type_err("host_bind target is not a function"));
        }
        self.new_func_object(Callee::Bound { target, this, args: Vec::new() })
    }

    /// `PromiseResolve(%Promise%, x)` then `PerformPromiseThen(promise, on_ok, on_err)` with a fresh derived promise. A
    /// native promise is used as it is (no extra job); any other value is wrapped, and a thenable is assimilated in a
    /// job. The reactions are called with `this` undefined and one argument, at the next checkpoint. Does not read the
    /// script-visible `then`. Reading `x.constructor` (as the standard does) may run a getter.
    pub fn host_promise_react(&mut self, x: Value, on_ok: Value, on_err: Value) -> Result<(), Error> {
        let p = self.promise_resolve(x)?;
        self.host_promise_then(p, on_ok, on_err)
    }

    /// Attach intrinsic reactions to a known native Promise without reading its constructor or then properties.
    pub fn host_promise_then(&mut self, p: Value, on_ok: Value, on_err: Value) -> Result<(), Error> {
        let o = self.host_promise(p)?;
        let derived = self.new_promise()?;
        self.perform_then(o, on_ok, on_err, derived)
    }

    /// Reject a promise made by [`Interp::host_new_promise`]. See `host_resolve_promise`.
    pub fn host_reject_promise(&mut self, p: Value, v: Value) -> Result<(), Error> {
        let o = self.host_promise(p)?;
        self.reject_promise(o, v)
    }

    fn host_promise(&self, p: Value) -> Result<ObjId, Error> {
        match p {
            Value::Object(o) if self.promise.map.contains_key(&o) => Ok(o),
            _ => Err(unsupported("host settle of a value that is not a promise", Span::default())),
        }
    }

    /// A `TypeError` to throw.
    pub fn type_error(&mut self, msg: &str) -> Error {
        self.type_err(msg)
    }

    /// A `RangeError` to throw.
    pub fn range_error(&mut self, msg: &str) -> Error {
        self.mk_err(RANGE_ERROR, msg)
    }

    /// An error object whose prototype is `proto`, with own `message`. The host adds other own properties.
    /// Use `Error.prototype` ([`Interp::error_prototype`]) or an object that inherits from it.
    pub fn new_error(&mut self, proto: ObjId, msg: &str) -> Result<ObjId, Error> {
        let o = h!(self, self.heap.new_object(Some(proto)));
        let m = h!(self, self.heap.intern_str(msg));
        self.def(o, self.k_message, Value::String(m), true, false, true)?;
        Ok(o)
    }

    pub fn error_prototype(&self) -> ObjId {
        self.error_protos[0]
    }
    pub fn object_prototype(&self) -> ObjId {
        self.object_proto
    }
    pub fn array_prototype(&self) -> ObjId {
        self.array_proto
    }

    /// Can `v` be called?
    pub fn is_callable_value(&self, v: Value) -> bool {
        self.is_callable(v)
    }

    /// `ToString(v)`. It may call JavaScript (`toString` of an object), so hold no host borrow.
    pub fn to_js_string(&mut self, v: Value) -> Result<StrId, Error> {
        self.coerce_string(v)
    }

    /// `ToNumber(v)`. It may call JavaScript.
    pub fn to_js_number(&mut self, v: Value) -> Result<f64, Error> {
        self.coerce_number(v)
    }

    /// `ToBoolean(v)`.
    pub fn to_js_bool(&self, v: Value) -> Result<bool, Error> {
        self.truthy(v)
    }

    /// `base[key]`. It runs getters, so it may call JavaScript.
    pub fn get_value(&mut self, base: Value, key: PropertyKey) -> Result<Value, Error> {
        self.get_prop(base, key)
    }

    /// `base[key] = v`. It runs setters.
    pub fn put_value(&mut self, base: Value, key: PropertyKey, v: Value) -> Result<(), Error> {
        self.put_prop(base, key, v)
    }

    /// A new array with these elements. The elements are charged as steps.
    pub fn array_from(&mut self, items: &[Value]) -> Result<Value, Error> {
        self.charge(items.len() as u64)?;
        self.new_array_from(items)
    }

    /// Define a data property with all the flags given.
    pub fn define_data(&mut self, o: ObjId, key: PropertyKey, v: Value, writable: bool, enumerable: bool, configurable: bool) -> Result<(), Error> {
        self.def(o, key, v, writable, enumerable, configurable)
    }

    /// The realm's own `Symbol.iterator` as a property key. It does not depend on the global `Symbol`, which a
    /// script can delete or replace. The realm roots it.
    pub fn symbol_iterator(&self) -> PropertyKey {
        self.sc.k_iterator
    }

    /// `%IteratorPrototype%`: the prototype of a host iterator object, so that it is iterable (`it[Symbol.iterator]() === it`).
    pub fn iterator_prototype(&self) -> ObjId {
        self.sc.iter_proto
    }

    /// `GetIterator(v)` with the engine's own iteration (the one `for-of`, spread and patterns use): `Symbol.iterator` is
    /// read once. `None` when it is `undefined` or `null`, so the caller can take another path (a record). A method that
    /// is not callable, or a result that is not an object, is a `TypeError`. The method may run JavaScript.
    pub fn host_iter_open(&mut self, v: Value) -> Result<Option<HostIter>, Error> {
        Ok(self.iter_open(v)?.map(HostIter))
    }

    /// The next value, or `None` at the end. After `None` or an error, the iteration is done for good.
    /// Charges one step. The value is not rooted: the host reads it before it runs more JavaScript.
    pub fn host_iter_next(&mut self, it: &mut HostIter) -> Result<Option<Value>, Error> {
        self.iter_next(&mut it.0)
    }

    /// Stop early without an error (`IteratorClose`): calls `return`. Does nothing for a finished iteration.
    pub fn host_iter_close(&mut self, it: &mut HostIter) -> Result<(), Error> {
        self.iter_close(&mut it.0)
    }

    /// Stop because of `err`: calls `return` and keeps `err`, unless `err` is a limit, `Unsupported` or a heap
    /// fault. Then it runs no JavaScript. Returns the error to pass on.
    pub fn host_iter_fail(&mut self, it: &mut HostIter, err: Error) -> Error {
        self.iter_close_throw(&mut it.0, err)
    }

    /// Map a heap error to the error JavaScript sees (a limit stays a limit).
    pub fn heap_error(&mut self, e: HeapError) -> Error {
        self.herr(e)
    }
}

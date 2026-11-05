// `JSON.parse(text, reviver)` for the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// What is bound: InternalizeJSONProperty of ECMA-262 up to ES2022, with `reviver(name, value)` called as
// `reviver.call(holder, name, value)`. The synthetic root holder has the one key `""`. Own enumerable string keys
// (`EnumerableOwnProperties(value, key)`) and array lengths are read ONCE, when a container is first reached;
// every value is read with an ordinary `Get` just before it is visited, so getters and mutations by earlier
// callbacks are seen. A result of `undefined` runs `[[Delete]]`, any other result `CreateDataProperty`; a `false`
// answer of either is ignored, as in the spec.
//
// Compatibility limit: the current standard adds a third `context` argument with the `source` text of primitives (JSON
// parse source text access). This engine targets ES2022, keeps no parse records and passes two arguments. It
// never invents a `context`.
//
// How: no Rust recursion. A stack of `RvFrame`s (one per open object or array) lives in the call. Reviver calls
// go through `call_fn`, so they may re-enter `JSON.parse`; all state is local to one call. The heap never collects
// during a call, so the handles in the frames stay valid. A JS throw propagates as `Error::Throw`; every bound
// below is an `Error::Limit`, which no `catch` or `finally` can see, and no further JS runs after it.
//
// Bounds (the existing JSON caps, checked BEFORE the growth): open containers (`json.max_depth`) and values visited
// by one call (`json.max_nodes`). Callbacks can add keys, elements and cycles, so the walk is bound again here: an
// array longer than the remaining visit budget, or an object with more keys than that, ends with
// `Json(Nodes)`; a stack deeper than `max_depth` ends with `Json(Depth)`. Work is charged to the step budget:
// one step per visit and one per own key listed.

/// One open object or array.
struct RvFrame {
    obj: ObjId,
    /// The holder that `obj` was read from, and the name; they are the `this` and the first argument of the call.
    holder: ObjId,
    name: PropertyKey,
    /// Key snapshot of an object. `None` for an array (then `len` is its length at open time).
    keys: Option<Vec<PropertyKey>>,
    len: usize,
    next: usize,
}

/// The state of one `JSON.parse` revive pass.
struct Rv {
    stack: Vec<RvFrame>,
    reviver: Value,
    nodes: usize,
    lim: json::JsonLimits,
}

impl<'p> Interp<'p> {
    fn json_revive(&mut self, root: Value, reviver: Value) -> Res<Value> {
        let lim = self.ext.bl.json;
        let root_holder = h!(self, self.heap.new_object(Some(self.object_proto)));
        let k_empty = h!(self, self.heap.key_from_str(""));
        self.def(root_holder, k_empty, root, true, true, true)?;
        let mut st = Rv { stack: Vec::new(), reviver, nodes: 0, lim };
        let mut done = self.rv_enter(&mut st, root_holder, k_empty)?.map(|v| (v, k_empty));
        loop {
            if let Some((v, key)) = done.take() {
                let Some(fr) = st.stack.last() else { return Ok(v) };
                self.rv_store(fr.obj, key, v)?;
            }
            let Some(fr) = st.stack.last_mut() else { unreachable!("an empty stack returns with the root result") };
            if fr.next < fr.len {
                let key = match &fr.keys {
                    Some(k) => k[fr.next],
                    None => PropertyKey::Index(fr.next as u32),
                };
                fr.next += 1;
                let obj = fr.obj;
                done = self.rv_enter(&mut st, obj, key)?.map(|v| (v, key));
            } else {
                let fr = st.stack.pop().expect("the top frame exists");
                let kv = h!(self, self.heap.key_to_value(fr.name));
                let v = self.call_fn(st.reviver, Value::Object(fr.holder), &[kv, Value::Object(fr.obj)])?;
                done = Some((v, fr.name));
            }
        }
    }

    /// `Get(holder, key)`, then the reviver call for a primitive, or a frame for an object (`None`).
    fn rv_enter(&mut self, st: &mut Rv, holder: ObjId, key: PropertyKey) -> Res<Option<Value>> {
        self.tick()?;
        if st.nodes >= st.lim.max_nodes {
            return Err(Error::Limit(LimitKind::Json(json::LimitKind::Nodes)));
        }
        st.nodes += 1;
        let hv = Value::Object(holder);
        let val = self.get_prop(hv, key)?;
        let Value::Object(o) = val else {
            let kv = h!(self, self.heap.key_to_value(key));
            return self.call_fn(st.reviver, hv, &[kv, val]).map(Some);
        };
        if st.stack.len() >= st.lim.max_depth {
            return Err(Error::Limit(LimitKind::Json(json::LimitKind::Depth)));
        }
        let nodes_limit = || Error::Limit(LimitKind::Json(json::LimitKind::Nodes));
        let remaining = st.lim.max_nodes - st.nodes;
        let (keys, len) = if h!(self, self.heap.is_array(o)) {
            let len = match self.get_prop(val, self.k_length)? {
                Value::Number(n) if n >= 1.0 => n as usize,
                _ => 0,
            };
            if len > remaining {
                return Err(nodes_limit());
            }
            (None, len)
        } else {
            let slots = h!(self, self.heap.own_key_slots(o));
            if slots > remaining {
                return Err(nodes_limit());
            }
            self.spend(slots as u64)?;
            let mut list = h!(self, self.heap.own_keys(o));
            let mut n = 0;
            for i in 0..list.len() {
                let k = list[i];
                if !matches!(k, PropertyKey::Symbol(_)) && h!(self, self.heap.get_own_property(o, k)).is_some_and(|p| p.enumerable()) {
                    list[n] = k;
                    n += 1;
                }
            }
            list.truncate(n);
            (Some(list), n)
        };
        st.stack.push(RvFrame { obj: o, holder, name: key, keys, len, next: 0 });
        Ok(None)
    }

    /// `undefined` deletes, anything else is `CreateDataProperty`. A refusal (`false`) is ignored.
    fn rv_store(&mut self, o: ObjId, key: PropertyKey, v: Value) -> Res<()> {
        if let Value::Undefined = v {
            h!(self, self.heap.delete(o, key));
        } else {
            let d = PropertyDescriptor { value: Some(v), writable: Some(true), enumerable: Some(true), configurable: Some(true), ..Default::default() };
            h!(self, self.heap.define_own_property(o, key, &d));
        }
        Ok(())
    }
}

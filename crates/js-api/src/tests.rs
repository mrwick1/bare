// Tests of the scope rules, with a toy engine whose handle stack is a `Vec`. It has no collector; the tests check which slots stay.
use super::*;

#[derive(Clone, Debug, PartialEq)]
enum V {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<u16>),
    Obj(usize),
}

#[derive(Default)]
struct Toy {
    stack: Vec<V>,
    persistent: Vec<V>,
    slots: Rc<Slots>,
    objects: Vec<Vec<(V, V)>>,
    owners: Vec<(usize, u32)>,
}

impl Toy {
    fn push<'s>(&mut self, v: V) -> Local<'s> {
        self.stack.push(v);
        Local::from_slot(self.stack.len() as u32 - 1)
    }

    fn at(&self, v: Local<'_>) -> &V {
        &self.stack[v.slot() as usize]
    }
}

impl Engine for Toy {
    type Raw = V;
    fn stack_len(&self) -> u32 {
        self.stack.len() as u32
    }
    fn stack_truncate(&mut self, len: u32) {
        self.stack.truncate(len as usize);
    }
    fn stack_set(&mut self, to: u32, from: u32) {
        self.stack[to as usize] = self.stack[from as usize].clone();
    }
    fn persist(&mut self, v: Local<'_>) -> u32 {
        let (id, v) = (self.slots.take(), self.at(v).clone());
        match self.persistent.get_mut(id as usize) {
            Some(slot) => *slot = v,
            None => self.persistent.push(v),
        }
        id
    }
    fn persistent<'s>(&mut self, id: u32) -> Local<'s> {
        self.push(self.persistent[id as usize].clone())
    }
    fn slots(&self) -> &Rc<Slots> {
        &self.slots
    }
    fn undefined<'s>(&mut self) -> Local<'s> {
        self.push(V::Undefined)
    }
    fn null<'s>(&mut self) -> Local<'s> {
        self.push(V::Null)
    }
    fn boolean<'s>(&mut self, b: bool) -> Local<'s> {
        self.push(V::Bool(b))
    }
    fn number<'s>(&mut self, n: f64) -> Local<'s> {
        self.push(V::Num(n))
    }
    fn string<'s>(&mut self, text: &str) -> Result<Local<'s>, Error<'s>> {
        Ok(self.push(V::Str(text.encode_utf16().collect())))
    }
    fn type_of(&self, v: Local<'_>) -> Type {
        match self.at(v) {
            V::Undefined => Type::Undefined,
            V::Null => Type::Null,
            V::Bool(_) => Type::Boolean,
            V::Num(_) => Type::Number,
            V::Str(_) => Type::String,
            V::Obj(_) => Type::Object,
        }
    }
    fn as_bool(&self, v: Local<'_>) -> Option<bool> {
        if let V::Bool(b) = self.at(v) { Some(*b) } else { None }
    }
    fn as_number(&self, v: Local<'_>) -> Option<f64> {
        if let V::Num(n) = self.at(v) { Some(*n) } else { None }
    }
    fn as_str(&self, v: Local<'_>) -> Option<Str<'_>> {
        if let V::Str(u) = self.at(v) { Some(Str::Utf16(u)) } else { None }
    }
    fn global<'s>(&mut self) -> Local<'s> {
        self.push(V::Obj(0))
    }
    fn new_object<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        self.objects.push(Vec::new());
        Ok(self.push(V::Obj(self.objects.len() - 1)))
    }
    fn get<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let V::Obj(o) = *self.at(obj) else { return Err(Error::Throw(Local::from_slot(obj.slot()))) };
        let k = self.at(key).clone();
        let v = self.objects[o].iter().find(|p| p.0 == k).map_or(V::Undefined, |p| p.1.clone());
        Ok(self.push(v))
    }
    fn set<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>) -> Result<(), Error<'s>> {
        self.define(obj, key, value, Attrs::ALL)
    }
    fn define<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>, _attrs: Attrs) -> Result<(), Error<'s>> {
        let V::Obj(o) = *self.at(obj) else { return Err(Error::Throw(Local::from_slot(obj.slot()))) };
        let (k, v) = (self.at(key).clone(), self.at(value).clone());
        self.objects[o].retain(|p| p.0 != k);
        self.objects[o].push((k, v));
        Ok(())
    }
    fn call<'s>(&mut self, _f: Local<'_>, _this: Local<'_>, _args: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("calls")))
    }
    fn host_function<'s>(&mut self, _name: &str, _id: u32, _length: u32, _kind: HostFn) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("host functions")))
    }
    fn install_table<'s>(&mut self, _targets: &[Local<'_>], _id_base: u32, _defs: &'static [crate::Def]) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("host functions")))
    }
    fn set_host(&mut self, _host: Rc<dyn Host<Self>>) {}
    fn raw(&self, v: Local<'_>) -> V {
        self.at(v).clone()
    }
    fn raw_local<'s>(&mut self, r: &V) -> Local<'s> {
        self.push(r.clone())
    }
    fn set_host_owner<'s>(&mut self, obj: Local<'_>, key: u32) -> Result<(), Error<'s>> {
        let V::Obj(o) = *self.at(obj) else { return Err(Error::Throw(Local::from_slot(obj.slot()))) };
        self.owners.retain(|&(x, _)| x != o);
        self.owners.push((o, key));
        Ok(())
    }
    fn new_promise<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("promises")))
    }
    fn resolve_promise<'s>(&mut self, _p: Local<'_>, _v: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("promises")))
    }
    fn reject_promise<'s>(&mut self, _p: Local<'_>, _reason: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("promises")))
    }
    fn promise_then<'s>(&mut self, _p: Local<'_>, _ok: Local<'_>, _err: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("promises")))
    }
    fn promise_react<'s>(&mut self, _x: Local<'_>, _ok: Local<'_>, _err: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("promises")))
    }
    fn promise_status<'s>(&mut self, _v: Local<'_>) -> Option<PromiseState<'s>> {
        None
    }
    fn queue_microtask<'s>(&mut self, _cb: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("microtasks")))
    }
    fn checkpoint<'s>(&mut self) -> Result<usize, Error<'s>> {
        Ok(0)
    }
    fn drain_microtasks<'s>(&mut self) -> Result<usize, Error<'s>> {
        Ok(0)
    }
    fn pending_jobs(&self) -> usize {
        0
    }
    fn clear_failed_job(&mut self) {}
    fn iter_open<'s>(&mut self, _v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>> {
        Ok(None)
    }
    fn iter_next<'s>(&mut self, _it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>> {
        Ok(None)
    }
    fn iter_fail<'s>(&mut self, _it: &mut Iter<'s>, err: Error<'s>) -> Error<'s> {
        err
    }
    fn module_parse<'s>(&mut self, _source: &str, _url: &str) -> Result<u32, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn module_requests(&self, _m: u32) -> Vec<String> {
        Vec::new()
    }
    fn module_set_requested(&mut self, _m: u32, _deps: &[u32]) {}
    fn module_url(&self, _m: u32) -> &str {
        ""
    }
    fn module_link<'s>(&mut self, _root: u32) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn module_evaluate<'s>(&mut self, _m: u32) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn take_dynamic_imports(&mut self) -> Vec<ImportRequest> {
        Vec::new()
    }
    fn finish_dynamic_import<'s>(&mut self, _p: &Persistent, _m: u32) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn reject_dynamic_import<'s>(&mut self, _p: &Persistent, _reason: Local<'_>) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn fail_dynamic_import<'s>(&mut self, _p: &Persistent, _message: &str) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("modules")))
    }
    fn safepoint(&mut self) {}

    // The base set of handler methods: the toy engine has none of them. The adapter tests of each real engine run them.
    fn type_error<'s>(&mut self, _msg: &str) -> Error<'s> {
        Error::Abort(Abort::Unsupported("toy"))
    }
    fn range_error<'s>(&mut self, _msg: &str) -> Error<'s> {
        Error::Abort(Abort::Unsupported("toy"))
    }
    fn charge<'s>(&mut self, _n: u64) -> Result<(), Error<'s>> {
        Ok(())
    }
    fn to_number<'s>(&mut self, _v: Local<'_>) -> Result<f64, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("toy")))
    }
    fn to_string<'s>(&mut self, _v: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("toy")))
    }
    fn to_boolean(&self, _v: Local<'_>) -> bool {
        false
    }
    fn new_array<'s>(&mut self, _items: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("toy")))
    }
    fn is_array<'s>(&mut self, _v: Local<'_>) -> Result<bool, Error<'s>> {
        Ok(false)
    }
    fn intrinsic<'s>(&mut self, _which: Intrinsic) -> Local<'s> {
        self.push(V::Undefined)
    }
    fn get_prototype_of<'s>(&mut self, _obj: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        Ok(None)
    }
    fn set_prototype_of<'s>(&mut self, _obj: Local<'_>, _proto: Option<Local<'_>>) -> Result<bool, Error<'s>> {
        Ok(false)
    }
    fn define_accessor<'s>(&mut self, _obj: Local<'_>, _key: Local<'_>, _get: Option<Local<'_>>, _set: Option<Local<'_>>, _attrs: Attrs) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("toy")))
    }
    fn proto_from_new_target<'s>(&mut self, _default: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("toy")))
    }
    fn string_utf16<'s>(&mut self, units: &[u16]) -> Result<Local<'s>, Error<'s>> {
        Ok(self.push(V::Str(units.to_vec())))
    }
    fn wall_now_ms(&self) -> f64 {
        0.0
    }
    fn has_own<'s>(&mut self, _obj: Local<'_>, _key: Local<'_>) -> Result<bool, Error<'s>> {
        Ok(false)
    }
    fn own_keys<'s>(&mut self, _obj: Local<'_>) -> Result<Vec<Local<'s>>, Error<'s>> {
        Ok(Vec::new())
    }
    fn own_enumerable_data<'s>(&mut self, _obj: Local<'_>, _key: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        Ok(None)
    }
    fn is_callable(&self, _v: Local<'_>) -> bool {
        false
    }
    fn bind<'s>(&mut self, _target: Local<'_>, _this: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("bind")))
    }
}

fn toy() -> Toy {
    Toy { objects: vec![Vec::new()], ..Toy::default() }
}

#[test]
fn a_scope_drops_the_slots_it_pushed_and_keeps_the_outer_ones() {
    let mut e = toy();
    enter(&mut e, |s| {
        let a = s.number(1.0);
        s.scope(|inner| {
            inner.number(2.0);
            inner.number(3.0);
            assert_eq!(inner.e.stack_len(), 3);
            assert_eq!(inner.as_number(a), Some(1.0), "an outer local is usable inside");
        });
        assert_eq!(s.e.stack_len(), 1);
        assert_eq!(s.as_number(a), Some(1.0));
    });
    assert_eq!(e.stack_len(), 0);
}

#[test]
fn escape_scope_keeps_only_the_result_or_the_exception() {
    let mut e = toy();
    enter(&mut e, |s| {
        let o = s.new_object().unwrap();
        let k = s.string("name").unwrap();
        let v = s.string("box").unwrap();
        s.set(o, k, v).unwrap();
        let base = s.e.stack_len();
        let r = s.escape_scope(|inner| {
            let name = inner.get_str(o, "name")?;
            let t = inner.as_str(name).unwrap().to_string_lossy();
            inner.string(&format!("<{t}>"))
        });
        let r = r.unwrap();
        assert_eq!(s.e.stack_len(), base + 1, "one slot: the result");
        assert_eq!(s.as_str(r).unwrap().to_string_lossy(), "<box>");
        let thrown = s.escape_scope(|inner| {
            let n = inner.number(7.0);
            inner.number(8.0);
            Err(Error::Throw(n))
        });
        let Err(Error::Throw(t)) = thrown else { panic!("{thrown:?}") };
        assert_eq!(s.as_number(t), Some(7.0));
        assert_eq!(s.e.stack_len(), base + 2);
        let abort = s.escape_scope(|_| Err(Error::Abort(Abort::Limit("time"))));
        assert_eq!(abort, Err(Error::Abort(Abort::Limit("time"))));
    });
}

#[test]
fn a_persistent_handle_outlives_its_scope_until_it_is_dropped() {
    let mut e = toy();
    let p = enter(&mut e, |s| {
        let v = s.string("listener").unwrap();
        s.persist(v)
    });
    assert_eq!(e.stack_len(), 0);
    enter(&mut e, |s| {
        let v = s.local(&p);
        assert_eq!(s.type_of(v), Type::String);
        s.release(p);
    });
    let live = |e: &Toy| {
        let mut v = Vec::new();
        e.slots.for_each(|id| v.push(id));
        v
    };
    assert_eq!(live(&e), Vec::<u32>::new(), "a dropped slot is free at once");
    let (q, r) = enter(&mut e, |s| {
        let v = s.number(2.0);
        (s.persist(v), s.persist(v))
    });
    assert_eq!((q.id(), r.id()), (0, 1), "the lowest free slot first");
    drop(q);
    assert_eq!(live(&e), vec![1], "a plain drop needs no scope");
    let t = enter(&mut e, |s| {
        let v = s.number(3.0);
        s.persist(v)
    });
    assert_eq!(t.id(), 0, "a freed slot is used again");
    assert_eq!(live(&e), vec![0, 1]);
}

#[test]
fn args_are_consecutive_slots() {
    let mut e = toy();
    enter(&mut e, |s| {
        let a = s.number(1.0);
        s.boolean(true);
        let args = Args::from_slots(a.slot(), 2);
        assert_eq!(args.len(), 2);
        assert_eq!(args.get(1).and_then(|v| s.as_bool(v)), Some(true));
        assert_eq!(args.get(2), None);
        assert_eq!(args.iter().count(), 2);
    });
}

#[test]
fn a_traced_value_is_read_stored_and_visited_through_its_owner() {
    let mut e = toy();
    let mut t = enter(&mut e, |s| {
        let v = s.number(1.0);
        s.trace_ref(v)
    });
    assert_eq!(e.stack_len(), 0, "a traced value holds no slot");
    let mut work = Vec::new();
    enter(&mut e, |s| {
        assert_eq!(s.traced(&t).slot(), 0);
        let v = s.traced(&t);
        assert_eq!(s.as_number(v), Some(1.0));
        let w = s.string("cb").unwrap();
        s.store(&mut t, w);
        let o = s.new_object().unwrap();
        s.set_host_owner(o, 7).unwrap();
        s.set_host_owner(o, 8).unwrap();
        let n = s.null();
        assert!(matches!(s.set_host_owner(n, 9), Err(Error::Throw(_))), "only an object owns");
    });
    assert_eq!(e.owners, vec![(1, 8)], "a new key replaces the old one");
    Tracer::new(&mut work).visit(&t);
    assert_eq!(work, vec![V::Str("cb".encode_utf16().collect())]);
}

/// A host that gives no hook: the three microtask hooks have default bodies that do nothing and fail nothing.
struct NoHooks;

impl Host<Toy> for NoHooks {
    fn call<'s>(&self, s: &mut Scope<'s, Toy>, _id: u32, _this: Local<'s>, _args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(s.undefined())
    }
}

#[test]
fn the_microtask_hooks_of_a_host_default_to_doing_nothing() {
    let mut e = toy();
    enter(&mut e, |s| {
        let (h, v) = (NoHooks, s.undefined());
        h.report_exception(s, v);
        assert_eq!(h.between_jobs(s), Ok(()));
        assert_eq!(h.promise_rejections(s, &[v], &[], 3), Ok(()));
        assert_eq!(s.promise_status(v), None);
        assert_eq!(MAX_PENDING_REJECTIONS, 1024);
    });
}

struct NoClone;

impl CloneHost<Toy> for NoClone {
    fn is_host_object(&self, _: &V) -> bool {
        false
    }
    fn can_transfer(&self, _: &V) -> bool {
        false
    }
    fn transfer_out(&mut self, _: &V) -> Box<dyn std::any::Any> {
        Box::new(())
    }
    fn transfer_in<'s>(&mut self, _: &mut Scope<'s, Toy>, _: Vec<Box<dyn std::any::Any>>) -> Result<Vec<Local<'s>>, Error<'s>> {
        Ok(Vec::new())
    }
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, Toy>, _: Refused) -> Error<'s> {
        Error::Throw(s.undefined())
    }
}

#[test]
fn an_engine_with_no_clone_says_unsupported() {
    let mut e = toy();
    let v = Local::from_slot(0);
    let limits = CloneLimits { bytes: 1, depth: 1, nodes: 1, names: ["b", "d", "n"] };
    let opts = CloneOpts { limits, transfer: &[] };
    assert_eq!(e.clone_out(v, &opts, &mut NoClone).err(), Some(Error::Abort(Abort::Unsupported("clone_out"))));
    let mut c = Cloned::new(Box::from([]), Vec::new(), 0);
    assert_eq!(e.clone_in(&mut c, &mut NoClone).err(), Some(Error::Abort(Abort::Unsupported("clone_in"))));
    assert_eq!(e.clone_copy(v, &limits, &mut NoClone).err(), Some(Error::Abort(Abort::Unsupported("clone_copy"))));
}

#[test]
fn an_engine_with_no_handler_methods_says_unsupported() {
    let mut e = toy();
    let v = Local::from_slot(0);
    assert!(enter(&mut e, |s| matches!(s.run_script("1"), Err(Error::Abort(Abort::Unsupported("run_script"))))));
    assert!(enter(&mut e, |s| matches!(s.new_error(v, "m"), Err(Error::Abort(Abort::Unsupported("new_error"))))));
    assert!(enter(&mut e, |s| matches!(s.array_len(v), Err(Error::Abort(Abort::Unsupported("array_len"))))));
    assert_eq!(enter(&mut e, |s| (s.host_key(v), s.is_detached(v), s.bytes(v).is_none(), s.display(v))), (None, false, true, String::new()));
    assert_eq!(task(&mut e, |_| 1), Err(Abort::Unsupported("task")));
}

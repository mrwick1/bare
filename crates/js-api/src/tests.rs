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
    fn host_function<'s>(&mut self, _name: &str, _id: u32, _length: u32) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("host functions")))
    }
    fn set_host(&mut self, _host: Rc<dyn Host<Self>>) {}
    fn safepoint(&mut self) {}
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

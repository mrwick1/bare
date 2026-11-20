//! The runtime side of the ops that S2.5 (the compiler) added: accessors and methods, classes, private names, `super`, spread and
//! object rest, tagged templates. Plain safe code over [`Realm`]; the loop in `interp.rs` calls these and does the calls to JS itself
//! (a getter or a setter is a frame of the loop: a helper here gives [`Got::Get`] or [`Put::Set`] and the loop pushes it).
//!
//! **Private names.** `NewPrivateName` makes an atom whose text is `\0#x\0<n>` (the description, then a counter), which is unique for
//! each evaluation of a class. A private field, method or accessor is an own property of that key with no `A_E` and no `A_C`, so
//! `for-in` and the own-key lists skip it, and the lookup of a private name looks at the own shape only (the brand check). Known
//! gap: a script that builds the string `"\0#x\0<n>"` can reach the property. A symbol cell (S2.7) removes the gap.
use crate::cell::Layout;
use crate::interp_rt::{ErrKind, Got, Key, Put, R, Realm, Slot};
use crate::object::{F_CLASS, F_DERIVED, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W};
use crate::string::Str;
use crate::value::Value;

impl Realm {
    pub(crate) fn reconfigure(&mut self, o: Obj, k: Str, attrs: u8, vals: &[Value]) -> R<()> {
        if o.reconfigure(&mut self.heap, &mut self.shapes, k, attrs, vals) { Ok(()) } else { Err(self.error(ErrKind::Range, "out of memory")) }
    }

    fn key_atom(&mut self, key: Key) -> Str {
        match key {
            Key::Name(k) => k,
            Key::Idx(i) => self.idx_atom(i),
        }
    }

    fn add_attrs(&mut self, o: Obj, k: Str, attrs: u8, vals: &[Value]) -> R<()> {
        match o.add(&mut self.heap, &mut self.shapes, k, attrs, vals) {
            Some(_) => Ok(()),
            None => Err(self.error(ErrKind::Range, "out of memory")),
        }
    }

    /// Define the own data property `key` of `o` with the attributes `attrs` (a method: not enumerable).
    pub fn define_attrs(&mut self, o: Obj, key: Key, attrs: u8, v: Value) -> R<()> {
        self.fn_props(o);
        let k = self.key_atom(key);
        match self.shapes.find(o.shape(&self.shapes), k) {
            Some(e) if e.attrs() == attrs => {
                o.set_slot(&self.shapes, e.slot(), v);
                Ok(())
            }
            Some(_) => self.reconfigure(o, k, attrs, &[v]),
            None => self.add_attrs(o, k, attrs, &[v]),
        }
    }

    /// Define the getter (`setter` false) or the setter of the own accessor `key`. A getter and a setter of one name share a property.
    pub fn define_accessor(&mut self, o: Obj, key: Key, f: Value, setter: bool, enumerable: bool) -> R<()> {
        self.fn_props(o);
        let k = self.key_atom(key);
        let attrs = A_ACC | A_C | if enumerable { A_E } else { 0 };
        let mut vals = [Value::UNDEFINED; 2];
        vals[setter as usize] = f;
        match self.shapes.find(o.shape(&self.shapes), k) {
            Some(e) if e.attrs() & A_ACC != 0 => {
                o.set_slot(&self.shapes, e.slot() + setter as u32, f);
                Ok(())
            }
            Some(_) => self.reconfigure(o, k, attrs, &vals),
            None => self.add_attrs(o, k, attrs, &vals),
        }
    }

    /// `MakeClass` (`parent` `None`) and `MakeDerived`: give the constructor `ctor` its prototype object, the home object, and the class
    /// flags. With a parent, the prototype object inherits from `parent.prototype` and the constructor from `parent`.
    pub fn make_class(&mut self, ctor: Value, parent: Option<Value>) -> R<()> {
        let c = Realm::obj(ctor).expect("a constructor");
        let (proto_parent, ctor_parent) = match parent {
            None => (Some(self.object_proto.cell()), None),
            Some(p) if p.is_null() => (None, None),
            Some(p) => {
                let Some(po) = Realm::obj(p).filter(|o| o.is_callable()) else { return self.type_error("class extends value is not a constructor or null") };
                let pp = match self.lookup(po, self.k_prototype)? {
                    Some(v) if v.is_null() => None,
                    Some(v) => match Realm::obj(v) {
                        Some(x) => Some(x.cell()),
                        None => return self.type_error("class extends value does not have a valid prototype property"),
                    },
                    None => return self.type_error("class extends value does not have a valid prototype property"),
                };
                (pp, Some(po))
            }
        };
        let shape = self.shapes.root(Layout::Object, proto_parent, 4);
        let proto = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        self.define_attrs(proto, Key::Name(self.k_constructor), A_W | A_C, ctor)?;
        self.define_attrs(c, Key::Name(self.k_prototype), 0, proto.value())?;
        c.set_slot(&self.shapes, 2, proto.value());
        c.set_flag(F_CLASS | if parent.is_some() { F_DERIVED } else { 0 });
        if let Some(po) = ctor_parent {
            c.set_proto(&mut self.shapes, Some(po.cell()));
        }
        Ok(())
    }

    // ---- the global lexical environment ----

    /// `DeclLex`: the slot `slot` of the context `ctx` is the global lexical binding `name`. A SyntaxError when a script declared the name
    /// already, or the global object has a fixed property of that name (a `var`, `undefined`).
    pub fn declare_lex(&mut self, name: Str, ctx: Value, slot: u32, konst: bool) -> R<()> {
        self.check_lex(name)?;
        self.lex.insert(name.value().bits(), (name, ctx, slot, konst));
        // A binding hides a property of the global object of that name: the cache of the global sites must not give the property.
        self.shapes.changed(self.global.cell().as_ptr() as usize);
        Ok(())
    }

    /// `CheckLex`: the checks of `declare_lex`, without the declaration.
    pub fn check_lex(&mut self, name: Str) -> R<()> {
        let fixed = self.shapes.find(self.global.shape(&self.shapes), name).is_some_and(|e| e.attrs() & A_C == 0);
        if fixed || self.lex.contains_key(&name.value().bits()) {
            let n = self.text(name);
            return Err(self.error(ErrKind::Syntax, &format!("Identifier '{n}' has already been declared")));
        }
        Ok(())
    }

    /// Is `name` a global lexical binding? (The binding is in its dead zone or it has a value.)
    pub fn has_lex(&self, name: Str) -> bool {
        !self.lex.is_empty() && self.lex.contains_key(&name.value().bits())
    }

    /// The value of the global lexical binding `name`. `None`: there is none. A ReferenceError in its dead zone.
    pub fn lex_get(&mut self, name: Str) -> Option<R<Value>> {
        let &(_, ctx, slot, _) = self.lex.get(&name.value().bits())?;
        let v = Realm::ctx(ctx).expect("a context").get(1 + slot as usize);
        Some(if v.is_hole() {
            let n = self.text(name);
            Err(self.error(ErrKind::Reference, &format!("cannot access '{n}' before initialization")))
        } else {
            Ok(v)
        })
    }

    /// `name = v` for a global lexical binding. `None`: there is none. A TypeError for a `const`, a ReferenceError in the dead zone.
    pub fn lex_put(&mut self, name: Str, v: Value) -> Option<R<()>> {
        let &(_, ctx, slot, konst) = self.lex.get(&name.value().bits())?;
        let cx = Realm::ctx(ctx).expect("a context");
        let n = self.text(name);
        Some(if cx.get(1 + slot as usize).is_hole() {
            Err(self.error(ErrKind::Reference, &format!("cannot access '{n}' before initialization")))
        } else if konst {
            Err(self.error(ErrKind::Type, &format!("assignment to constant variable '{n}'")))
        } else {
            cx.set(1 + slot as usize, v);
            Ok(())
        })
    }

    // ---- private names ----

    /// `NewPrivateName`: a key that no other evaluation of the class has.
    pub fn new_private_name(&mut self, desc: Str) -> Value {
        self.private_n += 1;
        let t = format!("\0{}\0{}", self.text(desc), self.private_n);
        self.intern(&t).value()
    }

    fn private_desc(&self, k: Str) -> String {
        self.text(k).split('\0').nth(1).unwrap_or("#?").to_string()
    }

    fn private_obj(&mut self, o: Value, k: Str, what: &str) -> R<Obj> {
        match Realm::obj(o) {
            Some(ob) => Ok(ob),
            None => {
                let d = self.private_desc(k);
                self.type_error(&format!("cannot {what} private member {d} of a value that is not an object"))
            }
        }
    }

    /// `o.#name`.
    pub fn private_get(&mut self, o: Value, name: Value) -> R<Got> {
        let k = Realm::str_of(name).expect("a private name");
        let ob = self.private_obj(o, k, "read")?;
        match self.shapes.find(ob.shape(&self.shapes), k) {
            Some(e) if e.attrs() & A_ACC != 0 => {
                let g = ob.slot(&self.shapes, e.slot());
                if g.is_undefined() {
                    let d = self.private_desc(k);
                    return self.type_error(&format!("'{d}' was defined without a getter"));
                }
                Ok(Got::Get(g))
            }
            Some(e) => Ok(Got::Val(ob.slot(&self.shapes, e.slot()))),
            None => {
                let d = self.private_desc(k);
                self.type_error(&format!("cannot read private member {d} from an object whose class did not declare it"))
            }
        }
    }

    /// `o.#name = v`.
    pub fn private_set(&mut self, o: Value, name: Value, v: Value) -> R<Put> {
        let k = Realm::str_of(name).expect("a private name");
        let ob = self.private_obj(o, k, "write")?;
        let d = self.private_desc(k);
        match self.shapes.find(ob.shape(&self.shapes), k) {
            Some(e) if e.attrs() & A_ACC != 0 => match Self::setter(ob.slot(&self.shapes, e.slot() + 1)) {
                Put::Fail => self.type_error(&format!("'{d}' was defined without a setter")),
                p => Ok(p),
            },
            Some(e) if e.attrs() & A_W == 0 => self.type_error(&format!("private method {d} is not writable")),
            Some(e) => {
                ob.set_slot(&self.shapes, e.slot(), v);
                Ok(Put::Done)
            }
            None => self.type_error(&format!("cannot write private member {d} to an object whose class did not declare it")),
        }
    }

    /// `#name in o`.
    pub fn private_has(&mut self, o: Value, name: Value) -> R<bool> {
        let k = Realm::str_of(name).expect("a private name");
        let Some(ob) = Realm::obj(o) else { return self.type_error("cannot use 'in' to search for a private field in a value that is not an object") };
        Ok(self.shapes.find(ob.shape(&self.shapes), k).is_some())
    }

    /// Add the private field or method `name` (`attrs` `A_W`: a field; 0: a method) to `o`.
    pub fn private_def(&mut self, o: Value, name: Value, attrs: u8, v: Value) -> R<()> {
        let k = Realm::str_of(name).expect("a private name");
        let ob = self.private_obj(o, k, "initialize")?;
        if self.shapes.find(ob.shape(&self.shapes), k).is_some() {
            let d = self.private_desc(k);
            return self.type_error(&format!("cannot initialize {d} twice on the same object"));
        }
        self.add_attrs(ob, k, attrs, &[v])
    }

    /// Add the getter (`setter` false) or the setter of the private accessor `name` to `o`.
    pub fn private_def_accessor(&mut self, o: Value, name: Value, f: Value, setter: bool) -> R<()> {
        let k = Realm::str_of(name).expect("a private name");
        let ob = self.private_obj(o, k, "initialize")?;
        let mut vals = [Value::UNDEFINED; 2];
        vals[setter as usize] = f;
        match self.shapes.find(ob.shape(&self.shapes), k) {
            Some(e) if e.attrs() & A_ACC != 0 && ob.slot(&self.shapes, e.slot() + setter as u32).is_undefined() => {
                ob.set_slot(&self.shapes, e.slot() + setter as u32, f);
                Ok(())
            }
            Some(_) => {
                let d = self.private_desc(k);
                self.type_error(&format!("cannot initialize {d} twice on the same object"))
            }
            None => self.add_attrs(ob, k, A_ACC, &vals),
        }
    }

    // ---- super ----

    fn super_base(&mut self, home: Value) -> R<Option<Obj>> {
        match Realm::obj(home) {
            Some(h) => Ok(self.proto_of(h)),
            None => Err(self.error(ErrKind::Reference, "'super' keyword unexpected here")),
        }
    }

    /// `super[key]` read: the lookup starts at the prototype of the home object. A getter runs with the `this` of the caller.
    pub fn super_get(&mut self, home: Value, key: Key) -> R<Got> {
        let k = self.key_atom(key);
        Ok(match self.super_base(home)? {
            None => return self.type_error("cannot read properties of null (the prototype of the home object)"),
            Some(b) => Self::got(self.lookup_g(b, k)),
        })
    }

    /// `super[key] = v`: a setter that the prototype chain gives runs with `this`; else the value is set on `this`.
    pub fn super_set(&mut self, home: Value, this: Value, key: Key, v: Value) -> R<Put> {
        let k = self.key_atom(key);
        let Some(b) = self.super_base(home)? else { return self.type_error("cannot set properties of null (the prototype of the home object)") };
        if let Some(Slot::Acc(_, s)) = self.lookup_g(b, k) {
            return Ok(Self::setter(s));
        }
        self.set_prop_g(this, key, v)
    }

    // ---- spread and rest ----

    /// The values that the iterator of `v` gives. An array and a string are iterated (the protocol is S2.7).
    pub fn iter_values(&mut self, v: Value) -> R<Vec<Value>> {
        if let Some(o) = Realm::obj(v)
            && self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed
        {
            return Ok((0..o.len()).map(|i| o.get_element(i).unwrap_or(Value::UNDEFINED)).collect());
        }
        if let Some(s) = Realm::str_of(v) {
            let s = self.flat(s);
            let units: Vec<u16> = (0..s.len()).filter_map(|i| s.unit_at(i)).collect();
            let (mut out, mut i) = (Vec::new(), 0);
            while i < units.len() {
                let pair = (0xD800..0xDC00).contains(&units[i]) && units.get(i + 1).is_some_and(|u| (0xDC00..0xE000).contains(u));
                let n = if pair { 2 } else { 1 };
                out.push(crate::string::Str::new(&mut self.heap, crate::string::Chars::Two(&units[i..i + n])).expect("memory").value());
                i += n;
            }
            return Ok(out);
        }
        self.type_error("the value is not iterable (the iterator protocol is S2.7)")
    }

    /// The own enumerable properties of `src`, minus the keys `skip`. An accessor throws (it needs a call).
    fn own_enumerable(&mut self, src: Value, skip: &[Key]) -> R<Vec<(Key, Value)>> {
        let (mut names, mut idxs) = (Vec::new(), Vec::new());
        for k in skip {
            match *k {
                Key::Name(s) => names.push(s),
                Key::Idx(i) => {
                    idxs.push(i);
                    names.push(self.idx_atom(i));
                }
            }
        }
        let mut out = Vec::new();
        if let Some(o) = Realm::obj(src) {
            if self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed {
                for i in 0..o.len() {
                    if let Some(v) = o.get_element(i)
                        && !idxs.contains(&i)
                    {
                        out.push((Key::Idx(i), v));
                    }
                }
            }
            let entries: Vec<_> = self.shapes.entries(o.shape(&self.shapes)).filter(|e| e.attrs() & A_E != 0).map(|e| (e.key(), e.attrs(), e.slot())).collect();
            for (k, a, slot) in entries {
                if a & A_ACC != 0 {
                    return self.type_error("copying an accessor property is not supported yet (S2.7)");
                }
                if !names.contains(&k) {
                    out.push((Key::Name(k), o.slot(&self.shapes, slot)));
                }
            }
        } else if let Some(s) = Realm::str_of(src) {
            for i in 0..s.len() as u32 {
                if !idxs.contains(&i) {
                    let c = self.flat(s).unit_at(i as usize).map(|u| String::from_utf16_lossy(&[u])).unwrap_or_default();
                    out.push((Key::Idx(i), self.string(&c)));
                }
            }
        }
        Ok(out)
    }

    /// `{...src}` into the object `dst`.
    pub fn copy_props(&mut self, dst: Obj, src: Value) -> R<()> {
        for (k, v) in self.own_enumerable(src, &[])? {
            self.define(dst, k, v)?;
        }
        Ok(())
    }

    /// `{a, ...rest} = src`: a new object with the own enumerable properties of `src` except `keys`.
    pub fn obj_rest(&mut self, src: Value, keys: &[Value]) -> R<Value> {
        if src.is_undefined() || src.is_null() {
            return self.type_error("cannot destructure 'undefined' or 'null'");
        }
        let skip = keys.iter().map(|k| self.key_of(*k)).collect::<R<Vec<_>>>()?;
        let o = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        for (k, v) in self.own_enumerable(src, &skip)? {
            self.define(o, k, v)?;
        }
        Ok(o.value())
    }

    /// The template object of a tagged template: the array of the cooked strings with the array of the raw strings as `raw`.
    pub fn make_template(&mut self, cooked: &[Value], raw: &[Value]) -> Value {
        let r = Obj::array(&mut self.heap, &self.shapes, self.s_array, raw).expect("memory");
        let a = Obj::array(&mut self.heap, &self.shapes, self.s_array, cooked).expect("memory");
        a.add(&mut self.heap, &mut self.shapes, self.k_raw, 0, &[r.value()]).expect("memory");
        a.value()
    }
}

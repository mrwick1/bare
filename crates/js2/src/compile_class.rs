//! Classes of the compiler (`class C extends B { ... }`).
//!
//! The class makes its constructor, its prototype, and its elements, in the order of the text (the spec's ClassDefinitionEvaluation):
//!
//! 1. A scope holds the inner name of the class and the names that the class makes: one variable `#x` for each private name (a private
//!    name object that `NewPrivateName` makes, one for each evaluation of the class), `%k<i>` for the key of a computed field, and `%m#x`,
//!    `%g#x` and `%s#x` for the function of an instance private method, getter and setter. These names are no identifiers: the facts do not
//!    list them, so [`Fc::captured`] puts each one in a context slot and the methods find them by the chain.
//! 2. The constructor is a closure (or `DefaultCtor`). `MakeClass` or `MakeDerived` gives it its prototype.
//! 3. Each method and accessor is defined on the prototype (or on the constructor when it is `static`), not enumerable. A private static
//!    method goes on the constructor at once. A computed key is read here, once.
//! 4. The fields that an instance gets (the private methods and accessors first, then the fields in order) are one synthetic function:
//!    its code is made here by a second [`Fc`] over the same tree (the text of an initializer is in the tree of the function that holds
//!    the class, not in a function of its own), and it is a child with [`Child::ready`]. `SetFields` gives it to the constructor.
//! 5. The inner name of the class gets the constructor. A `static` field or block is a synthetic function too. The class calls it with
//!    `this` set to the constructor, in the order of the text.
use super::*;

impl<'a> Fc<'a> {
    /// A new compiler for a function that has no tree of its own: it sees the variables of the scopes that are open here.
    fn inline_fc(&self, id: FnId, flags: u16) -> Fc<'a> {
        let mut c = Fc::new(self.prog, self.facts, self.atoms, &Input { chain: self.cur_ctx.clone(), self_name: false, name: None, scoped: self.scoped });
        c.strict = true;
        c.noctor = true;
        c.fn_id = Some(id);
        c.fn_flags = flags;
        c
    }

    /// The entry of the facts that holds the text `start..end`: a field initializer (`flag` `FIELD_INIT`) or a static block.
    fn inline_entry(&self, start: u32, end: u32, flag: u16) -> Option<FnId> {
        let facts = self.facts;
        let at = facts.by_start.partition_point(|&i| facts.fns[i as usize].start <= start);
        facts.by_start[..at].iter().rev().take(64).copied().find(|&i| {
            let f = facts.get(i);
            f.flags & facts::INLINE != 0 && f.flags & flag != 0 && f.start <= start && f.end >= end
        })
    }

    /// Add a function that this compiler made to the children. Gives its index for `Closure`.
    fn child_ready(&mut self, id: FnId, code: Compiled) -> CR<u32> {
        for w in code.unsupported.iter().copied() {
            self.unsup(w);
        }
        self.inline.push(code);
        self.funcs.push(id);
        self.children.push(Child { id, chain: self.cur_ctx.clone(), self_name: false, name: None, ready: Some(self.inline.len() as u32 - 1) });
        if self.funcs.len() > u16::MAX as usize {
            return Err(CompileError::Limit("more than 65,535 child functions"));
        }
        Ok(self.funcs.len() as u32 - 1)
    }

    /// The `name` of a method or accessor: `get x`, `set x` or `x`.
    fn prefixed(n: &str, setter: u32, getter: bool) -> Box<str> {
        match (getter, setter) {
            (true, _) => format!("get {n}").into(),
            (_, 1) => format!("set {n}").into(),
            _ => n.into(),
        }
    }

    /// A class: its constructor goes to `dst`.
    pub(crate) fn class(&mut self, cid: ClassId, dst: u32) -> CR {
        let prog = self.prog;
        let c = *prog.class(cid);
        let cname: Option<Box<str>> = match c.name {
            Some(n) => Some(Box::from(&*prog.name(n))),
            None => self.hint_for_class(cid),
        };
        let elems: Vec<ClassElem> = prog.list(c.elems).map(|e| *prog.elem(e)).collect();
        let m = self.mark();
        let is_ctor = |e: &ClassElem| matches!(e.kind, ElemKind::Method(f) if matches!(prog.func(f).kind, FuncKind::Constructor | FuncKind::DerivedConstructor));

        // The variables of the class scope.
        let mut decls: Vec<Decl<'a>> = Vec::new();
        let add = |name: String, kind: VK, end: u32, decls: &mut Vec<Decl<'a>>| {
            if !decls.iter().any(|d| d.name == name.as_str()) {
                decls.push(Decl { name: Cow::Owned(name), kind, end, reg: None });
            }
        };
        // The inner name of the class: only when the text of the class uses it (the declaring name is one hit).
        let name = c.name.map(|n| prog.name(n)).filter(|n| self.occurrences(n, c.span.start, c.span.end) > 1);
        if let Some(n) = &name {
            decls.push(Decl { name: n.clone(), kind: VK::Const, end: c.span.end, reg: None });
        }
        let mut privates: Vec<String> = Vec::new();
        for (i, e) in elems.iter().enumerate() {
            match (e.key, e.kind) {
                (Key::Private(s), kind) => {
                    let n = self.private_name(s);
                    if !privates.contains(&n) {
                        privates.push(n.clone());
                        add(n.clone(), VK::Var, 0, &mut decls);
                    }
                    if !e.is_static {
                        match kind {
                            ElemKind::Method(_) => add(format!("%m{n}"), VK::Var, 0, &mut decls),
                            ElemKind::Getter(_) => add(format!("%g{n}"), VK::Var, 0, &mut decls),
                            ElemKind::Setter(_) => add(format!("%s{n}"), VK::Var, 0, &mut decls),
                            _ => {}
                        }
                    }
                }
                (Key::Computed(_), ElemKind::Field(_)) => add(format!("%k{i}"), VK::Var, 0, &mut decls),
                _ => {}
            }
        }
        let mark = self.enter(c.span, &decls, false)?;
        // The inner name is in its dead zone while the heritage and the computed keys run.
        if let Some(n) = &name
            && let Some(Home::Reg(r)) = self.find_var(n).map(|v| v.home)
        {
            self.op(Op::LoadHole, r, 0, 0);
            self.set_live(r);
        }
        for n in &privates {
            let t = self.alloc();
            let a = self.atom(n)?;
            self.op(Op::NewPrivateName, t, a, 0);
            self.set_live(t);
            self.store_var(n, 0, t, true)?;
            self.free(t);
        }

        // The constructor and the prototype.
        let sup = match c.superclass {
            Some(e) => {
                let s = self.alloc();
                self.to(e, s)?;
                self.set_live(s);
                Some(s)
            }
            None => None,
        };
        let ctor = self.alloc();
        match elems.iter().find(|e| is_ctor(e)).map(|e| e.kind) {
            Some(ElemKind::Method(f)) => {
                let i = self.child(f, false, cname.clone())?;
                self.op(Op::Closure, ctor, i, 0);
                self.set_live(ctor);
            }
            _ => {
                self.op(Op::DefaultCtor, ctor, sup.is_some() as u32, 0);
                self.set_live(ctor);
                if let Some(n) = &cname {
                    let t = self.alloc();
                    let a = self.atom(n)?;
                    self.op(Op::LoadAtom, t, a, 0);
                    self.set_live(t);
                    self.op(Op::SetFnName, ctor, t, 0);
                    self.free(t);
                }
            }
        }
        self.set_live(ctor);
        match sup {
            Some(s) => self.op(Op::MakeDerived, ctor, s, 0),
            None => self.op(Op::MakeClass, ctor, 0, 0),
        }
        let proto = self.alloc();
        let s = self.site_of("prototype")?;
        self.op(Op::GetField, proto, ctor, s);
        self.set_live(proto);

        // The methods and accessors.
        let mut instance: Vec<usize> = Vec::new();
        let mut statics: Vec<usize> = Vec::new();
        for (i, e) in elems.iter().enumerate() {
            if is_ctor(e) {
                continue;
            }
            let m2 = self.mark();
            let obj = if e.is_static { ctor } else { proto };
            match e.kind {
                ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) => {
                    let (setter, getter) = (matches!(e.kind, ElemKind::Setter(_)) as u32, matches!(e.kind, ElemKind::Getter(_)));
                    let accessor = setter == 1 || getter;
                    if let Key::Private(s) = e.key {
                        let n = self.private_name(s);
                        let fr = self.alloc();
                        self.hint = Some(Hint::Func(f, Self::prefixed(&n, setter, getter)));
                        self.function(f, fr, false)?;
                        self.set_live(fr);
                        if self.func_uses_super(f) {
                            self.op(Op::SetHome, fr, obj, 0);
                        }
                        if e.is_static {
                            let k = self.alloc();
                            self.read_var(&n, 0, k)?;
                            self.set_live(k);
                            if accessor {
                                self.opx(Op::DefPrivateAccessor, ctor, k, fr, setter);
                            } else {
                                self.op(Op::DefPrivateMethod, ctor, k, fr);
                            }
                        } else {
                            let hidden = format!("%{}{n}", if accessor { if getter { 'g' } else { 's' } } else { 'm' });
                            self.store_var(&hidden, 0, fr, true)?;
                            instance.push(i);
                        }
                    } else {
                        let key = self.prop_key(obj, e.key)?;
                        let fr = self.alloc();
                        if let Ok(n) = &key {
                            self.hint = Some(Hint::Func(f, Self::prefixed(n, setter, getter)));
                        }
                        self.function(f, fr, false)?;
                        self.set_live(fr);
                        if let Err(k) = &key {
                            self.op(Op::SetFnName, fr, *k, if getter { 1 } else { 2 * setter });
                        }
                        if self.func_uses_super(f) {
                            self.op(Op::SetHome, fr, obj, 0);
                        }
                        match (key, accessor) {
                            (Ok(n), false) => {
                                let s = self.site_of(&n)?;
                                self.op(Op::DefMethod, obj, fr, s);
                            }
                            (Err(k), false) => self.op(Op::DefMethodElem, obj, k, fr),
                            (Ok(n), true) => {
                                let s = self.site_of(&n)?;
                                self.opx(Op::DefAccessor, obj, fr, s, setter | 2);
                            }
                            (Err(k), true) => self.opx(Op::DefAccessorElem, obj, k, fr, setter | 2),
                        }
                    }
                }
                ElemKind::Field(_) => {
                    if let Key::Computed(k) = e.key {
                        let kv = self.any(k)?;
                        let kt = self.alloc();
                        self.op(Op::ToKey, kt, obj, kv);
                        self.set_live(kt);
                        self.store_var(&format!("%k{i}"), 0, kt, true)?;
                    }
                    if e.is_static {
                        statics.push(i);
                    } else {
                        instance.push(i);
                    }
                }
                ElemKind::StaticBlock(_) => statics.push(i),
            }
            self.free(m2);
        }

        // The inner name is set before the first `static` initializer runs.
        if let Some(n) = &name {
            self.store_var(n, 0, ctor, true)?;
        }

        // The fields of an instance.
        if !instance.is_empty() {
            let (i, home) = self.fields_function(&elems, &instance, false)?;
            let f = self.alloc();
            self.op(Op::Closure, f, i, 0);
            self.set_live(f);
            if home {
                self.op(Op::SetHome, f, proto, 0);
            }
            self.op(Op::SetFields, ctor, f, 0);
            self.free(f);
        }

        // The static fields and blocks, in the order of the text.
        for i in statics {
            let m2 = self.mark();
            let base = self.window(2);
            let (ci, home) = match elems[i].kind {
                ElemKind::StaticBlock(f) => self.static_block(elems[i], f)?,
                _ => self.fields_function(&elems, &[i], true)?,
            };
            self.op(Op::Closure, base, ci, 0);
            self.set_live(base);
            if home {
                self.op(Op::SetHome, base, ctor, 0);
            }
            self.op(Op::Mov, base + 1, ctor, 0);
            self.set_live(base + 1);
            self.op(Op::Call, base, base, 0);
            self.free(m2);
        }

        self.op(Op::Mov, dst, ctor, 0);
        self.leave(mark);
        self.free(m);
        Ok(())
    }

    /// The text `start..end` of the function that holds the class, as a set of children: the functions inside the initializers.
    fn inline_kids(&self, id: FnId, out: &mut Vec<FnId>) {
        out.extend(self.facts.children(Some(id)));
    }

    /// `static { ... }`: its body is a function with no parameters, compiled by a second compiler. Gives the child and whether it
    /// uses `super`.
    fn static_block(&mut self, e: ClassElem, f: FuncId) -> CR<(u32, bool)> {
        let id = self.inline_entry(e.span.start, e.span.end, facts::STATIC_BLOCK).unwrap_or(0);
        let flags = self.facts.get(id).flags;
        let mut c = self.inline_fc(id, flags);
        let mut kids = Vec::new();
        self.inline_kids(id, &mut kids);
        c.note_children(&kids);
        c.function_body(f)?;
        let code = c.finish(0)?;
        Ok((self.child_ready(id, code)?, flags & facts::SUPER != 0))
    }

    /// One function for the elements `which` of `elems`: the private methods and accessors (instance only), then the fields, in order.
    /// With `this` the instance, or (`is_static`) the constructor. Gives the child and whether the text uses `super`.
    fn fields_function(&mut self, elems: &[ClassElem], which: &[usize], is_static: bool) -> CR<(u32, bool)> {
        let prog = self.prog;
        // The entry of each initializer, and the one that names the function.
        let mut entries: Vec<(usize, Option<FnId>)> = Vec::new();
        for &i in which {
            let e = elems[i];
            let id = match e.kind {
                ElemKind::Field(Some(x)) => {
                    let sp = prog.expr(x).span;
                    self.inline_entry(sp.start, sp.end, facts::FIELD_INIT)
                }
                _ => None,
            };
            entries.push((i, id));
        }
        let id = entries.iter().find_map(|(_, id)| *id).unwrap_or(0);
        let flags = entries.iter().filter_map(|(_, id)| *id).fold(0, |a, id| a | self.facts.get(id).flags);
        let mut c = self.inline_fc(id, flags);
        let mut kids = Vec::new();
        for (_, id) in &entries {
            if let Some(id) = id {
                self.inline_kids(*id, &mut kids);
            }
        }
        c.note_children(&kids);
        let mut decls: Vec<Decl<'a>> = Vec::new();
        if c.cap.contains_key("this") {
            decls.push(Decl { name: Cow::Borrowed("this"), kind: VK::Var, end: 0, reg: None });
        }
        let mark = c.enter(Span { start: 0, end: u32::MAX }, &decls, false)?;
        if let Some(Var { home: Home::Ctx { level, slot }, .. }) = c.find_var("this") {
            let (level, slot) = (*level, *slot);
            let t = c.alloc();
            c.op(Op::LoadThis, t, 0, 0);
            c.op(Op::InitCtx, t, c.depth - level, slot);
            c.free(t);
        }
        let th = c.alloc();
        c.op(Op::LoadThis, th, 0, 0);
        c.set_live(th);
        // Private methods and accessors first.
        if !is_static {
            for (i, _) in &entries {
                let e = elems[*i];
                let Key::Private(s) = e.key else { continue };
                let n = c.private_name(s);
                let (tag, x) = match e.kind {
                    ElemKind::Method(_) => ('m', 0),
                    ElemKind::Getter(_) => ('g', 0),
                    ElemKind::Setter(_) => ('s', 1),
                    _ => continue,
                };
                let m2 = c.mark();
                let k = c.alloc();
                c.read_var(&n, 0, k)?;
                c.set_live(k);
                let f = c.alloc();
                c.read_var(&format!("%{tag}{n}"), 0, f)?;
                c.set_live(f);
                match e.kind {
                    ElemKind::Method(_) => c.op(Op::DefPrivateMethod, th, k, f),
                    _ => c.opx(Op::DefPrivateAccessor, th, k, f, x),
                }
                c.free(m2);
            }
        }
        for (i, id) in &entries {
            let e = elems[*i];
            let ElemKind::Field(init) = e.kind else { continue };
            let m2 = c.mark();
            let sp = init.map(|x| prog.expr(x).span.start).unwrap_or(e.span.start);
            c.line(sp);
            match e.key {
                Key::Private(s) => {
                    let n = c.private_name(s);
                    let k = c.alloc();
                    c.read_var(&n, 0, k)?;
                    c.set_live(k);
                    if let (Key::Private(s), Some(x)) = (e.key, init) {
                        let t = c.private_name(s);
                        c.name_hint(x, &t);
                    }
                    let v = c.field_value(init)?;
                    c.op(Op::DefPrivate, th, k, v);
                }
                Key::Computed(_) => {
                    let k = c.alloc();
                    c.read_var(&format!("%k{i}"), 0, k)?;
                    c.set_live(k);
                    let v = c.field_value(init)?;
                    if init.is_some_and(|x| c.is_anon_def(x)) {
                        c.op(Op::SetFnName, v, k, 0);
                    }
                    c.op(Op::DefElem, th, k, v);
                }
                key => match c.prop_key(th, key)? {
                    Ok(n) => {
                        if let Some(x) = init {
                            c.name_hint(x, &n);
                        }
                        let v = c.field_value(init)?;
                        let s = c.site_of(&n)?;
                        c.op(Op::DefField, th, v, s);
                    }
                    Err(k) => {
                        let v = c.field_value(init)?;
                        if init.is_some_and(|x| c.is_anon_def(x)) {
                            c.op(Op::SetFnName, v, k, 0);
                        }
                        c.op(Op::DefElem, th, k, v);
                    }
                },
            }
            c.free(m2);
            let _ = id;
        }
        c.leave(mark);
        let code = c.finish(0)?;
        Ok((self.child_ready(id, code)?, flags & facts::SUPER != 0))
    }

    /// The value of a field initializer in a new register (`undefined` without one).
    fn field_value(&mut self, init: Option<ExprId>) -> CR<u32> {
        let v = self.alloc();
        match init {
            Some(x) => self.to(x, v)?,
            None => self.op(Op::LoadUndef, v, 0, 0),
        }
        self.set_live(v);
        Ok(v)
    }
}

//! Destructuring patterns of the compiler: declarations, parameters, assignment targets, `for-in` and `for-of` heads.
use super::*;

/// How a pattern writes its names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Bind {
    /// The first write of a declaration or a parameter: no dead-zone or constant check.
    Init,
    /// An assignment.
    Assign,
}

/// Where the value that a pattern takes apart comes from.
#[derive(Clone, Copy)]
enum Src<'s> {
    Reg(u32),
    Field(u32, &'s str),
    Elem(u32, u32),
}

impl<'a> Fc<'a> {
    /// The names that a pattern declares.
    pub(crate) fn pat_names(&self, p: PatId, out: &mut Vec<(Cow<'a, str>, u32)>) {
        let prog = self.prog;
        let pt = *prog.pat(p);
        match pt.kind {
            PatKind::Ident => out.push((prog.name(pt.span), pt.span.end)),
            PatKind::Assign { target, .. } => self.pat_names(target, out),
            PatKind::Rest(t) => self.pat_names(t, out),
            PatKind::Array(l) => {
                for e in prog.list(l) {
                    self.pat_names(e, out);
                }
            }
            PatKind::Object(l) => {
                for pp in prog.list(l) {
                    self.pat_names(prog.pat_prop(pp).value, out);
                }
            }
            PatKind::Member(_) | PatKind::Hole => {}
        }
    }

    /// Take the value in register `v` apart into the names of the pattern.
    pub(crate) fn bind_pattern(&mut self, p: PatId, v: u32, mode: Bind) -> CR {
        self.bind_src(p, Src::Reg(v), mode)
    }

    fn emit_src(&mut self, src: Src, dst: u32) -> CR {
        match src {
            Src::Reg(r) => {
                if r != dst {
                    self.op(Op::Mov, dst, r, 0);
                }
            }
            Src::Field(o, n) => {
                let s = self.site_of(n)?;
                self.op(Op::GetField, dst, o, s);
            }
            Src::Elem(o, k) => self.op(Op::GetElem, dst, o, k),
        }
        self.set_live(dst);
        Ok(())
    }

    /// The register to build the value of an identifier target in: the variable's own register when a plain write can go there.
    fn target_reg(&self, p: PatId, mode: Bind) -> Option<u32> {
        let pt = *self.prog.pat(p);
        if !matches!(pt.kind, PatKind::Ident) {
            return None;
        }
        let name = self.name_of(pt.span);
        match self.resolve(&name, pt.span.start) {
            Res::Reg { reg, konst, tdz } if mode == Bind::Init || (!konst && !tdz) => Some(reg),
            _ => None,
        }
    }

    fn bind_src(&mut self, p: PatId, src: Src, mode: Bind) -> CR {
        let prog = self.prog;
        let pt = *prog.pat(p);
        let m = self.mark();
        match pt.kind {
            PatKind::Ident => {
                let name = self.name_of(pt.span);
                if let Some(r) = self.target_reg(p, mode) {
                    self.emit_src(src, r)?;
                } else {
                    let r = match src {
                        Src::Reg(r) => r,
                        _ => {
                            let t = self.alloc();
                            self.emit_src(src, t)?;
                            t
                        }
                    };
                    self.store_var(&name, pt.span.start, r, mode == Bind::Init)?;
                }
            }
            PatKind::Assign { target, default } => {
                let t = self.target_reg(target, mode).unwrap_or_else(|| self.alloc());
                self.emit_src(src, t)?;
                let skip = self.label();
                self.jmp_r(Op::JSNeUndef, t, skip);
                let tp = *prog.pat(target);
                if matches!(tp.kind, PatKind::Ident) {
                    let name = prog.name(tp.span);
                    self.name_hint(default, &name);
                }
                self.to(default, t)?;
                self.bind(skip);
                if matches!(tp.kind, PatKind::Ident) && self.target_reg(target, mode).is_some() {
                    // already in the variable
                } else {
                    self.bind_src(target, Src::Reg(t), mode)?;
                }
            }
            PatKind::Member(me) => {
                let v = match src {
                    Src::Reg(r) => r,
                    _ => {
                        let t = self.alloc();
                        self.emit_src(src, t)?;
                        t
                    }
                };
                self.store_member_target(me, v)?;
            }
            PatKind::Array(list) => {
                let v = self.src_reg(src)?;
                self.array_pattern(list, v, mode)?;
            }
            PatKind::Object(list) => {
                let v = self.src_reg(src)?;
                self.object_pattern(list, v, mode)?;
            }
            PatKind::Rest(_) | PatKind::Hole => {}
        }
        self.free(m);
        Ok(())
    }

    fn src_reg(&mut self, src: Src) -> CR<u32> {
        Ok(match src {
            Src::Reg(r) => r,
            _ => {
                let t = self.alloc();
                self.emit_src(src, t)?;
                t
            }
        })
    }

    /// `a.b = v` or `a[k] = v` where the target is a member expression.
    fn store_member_target(&mut self, me: ExprId, v: u32) -> CR {
        let x = *self.prog.expr(me);
        match x.kind {
            ExprKind::Member { obj, name, .. } => {
                let nm = self.name_of(name);
                let (o, mode) = self.member_base(obj, None)?;
                self.set_member(mode, o, Some(&nm), None, v)
            }
            ExprKind::Index { obj, index, .. } => {
                let (o, mode) = self.member_base(obj, Some(index))?;
                let k = self.any(index)?;
                self.set_member(mode, o, None, Some(k), v)
            }
            ExprKind::PrivateMember { obj, name, .. } => {
                let o = self.any(obj)?;
                let k = self.private_key(name)?;
                self.set_member(Mode::Private, o, None, Some(k), v)
            }
            _ => {
                self.unsup("this assignment target");
                Ok(())
            }
        }
    }

    fn object_pattern(&mut self, list: List<PatProp>, v: u32, mode: Bind) -> CR {
        let prog = self.prog;
        let props: Vec<PatProp> = prog.list(list).map(|p| *prog.pat_prop(p)).collect();
        // With a rest property, the keys of the other properties go to a window of registers that `ObjRest` reads.
        let nkeys = props.len() as u32 - props.iter().any(|p| matches!(p.key, Key::None)) as u32;
        let base = props.iter().any(|p| matches!(p.key, Key::None)).then(|| self.window(nkeys));
        for (i, pp) in props.into_iter().enumerate() {
            let m = self.mark();
            let slot = base.map(|b| b + i as u32);
            match pp.key {
                Key::None => {
                    let PatKind::Rest(t) = prog.pat(pp.value).kind else { continue };
                    let r = self.alloc();
                    self.opx(Op::ObjRest, r, v, base.unwrap_or(0), nkeys);
                    self.set_live(r);
                    self.bind_src(t, Src::Reg(r), mode)?;
                }
                Key::Computed(k) => {
                    let kr = self.any(k)?;
                    let kt = slot.unwrap_or_else(|| self.alloc());
                    self.op(Op::ToKey, kt, v, kr);
                    self.set_live(kt);
                    self.bind_src(pp.value, Src::Elem(v, kt), mode)?;
                }
                Key::Name(s) | Key::Str(s) => {
                    let name: Cow<str> = if matches!(pp.key, Key::Name(_)) {
                        prog.name(s)
                    } else {
                        let mut b = Vec::new();
                        let _ = prog.string(s, &mut b);
                        Cow::Owned(String::from_utf16_lossy(&b))
                    };
                    if super::expr::index_like(&name) {
                        let k = slot.unwrap_or_else(|| self.alloc());
                        self.load_number(k, name.parse::<f64>().unwrap_or(0.0));
                        self.set_live(k);
                        self.bind_src(pp.value, Src::Elem(v, k), mode)?;
                    } else {
                        if let Some(sl) = slot {
                            let a = self.atom(&name)?;
                            self.op(Op::LoadAtom, sl, a, 0);
                            self.set_live(sl);
                        }
                        self.bind_src(pp.value, Src::Field(v, &name), mode)?;
                    }
                }
                Key::Num(s) => {
                    let k = slot.unwrap_or_else(|| self.alloc());
                    let n = prog.number(s, crate::lex::NumberBase::Decimal).unwrap_or(0.0);
                    self.load_number(k, n);
                    self.set_live(k);
                    self.bind_src(pp.value, Src::Elem(v, k), mode)?;
                }
                Key::BigInt(_) | Key::Private(_) => self.unsup("a BigInt or private key in a pattern"),
            }
            self.free(m);
        }
        Ok(())
    }

    /// Can writing to the names of `p` throw? A write to a register or a context slot does not, unless it is an assignment to a `const`
    /// or to a `let` that may be in its dead zone. A default, a nested pattern and a member target run code.
    fn pat_may_throw(&self, p: PatId, mode: Bind) -> bool {
        let pt = *self.prog.pat(p);
        match pt.kind {
            PatKind::Hole => false,
            PatKind::Rest(t) => self.pat_may_throw(t, mode),
            PatKind::Assign { target, default } => {
                let pure = matches!(self.prog.expr(default).kind, ExprKind::Number(_) | ExprKind::String | ExprKind::Null | ExprKind::Bool(_) | ExprKind::This | ExprKind::Function(_));
                !pure || self.pat_may_throw(target, mode)
            }
            PatKind::Ident => match self.resolve(&self.name_of(pt.span), pt.span.start) {
                Res::Reg { tdz, konst, .. } | Res::Ctx { tdz, konst, .. } => mode == Bind::Assign && (tdz || konst),
                Res::Global => self.strict,
            },
            _ => true,
        }
    }

    /// `[a, , b = 1, ...c] = iterable`. A binding that throws closes the iterator (the spec's IteratorClose on a throw), unless the iterator
    /// is done. A throw from `next()` itself does not: the ranges of the handler leave out each `IterNext`.
    fn array_pattern(&mut self, list: List<Pat>, v: u32, mode: Bind) -> CR {
        let prog = self.prog;
        let elems: Vec<PatId> = prog.list(list).collect();
        let m = self.mark();
        let rec = self.window(3);
        self.line(prog.pat(elems.first().copied().unwrap_or_else(|| Id::from_index(0))).span.start);
        self.op(Op::IterOpen, rec, v, 0);
        (0..3).for_each(|i| self.set_live(rec + i));
        let done = self.alloc();
        self.op(Op::LoadFalse, done, 0, 0);
        self.set_live(done);
        let ex = elems.iter().any(|&e| self.pat_may_throw(e, mode)).then(|| self.alloc());
        let snapshot = self.live.clone();
        let mut pieces: Vec<(usize, usize)> = Vec::new();
        let mut open = self.asm.len();
        for el in elems {
            let m2 = self.mark();
            let ep = *prog.pat(el);
            match ep.kind {
                PatKind::Rest(target) => {
                    let arr = self.alloc();
                    self.op(Op::NewArray, arr, 0, 0);
                    self.set_live(arr);
                    let (head, exh, end) = (self.label(), self.label(), self.label());
                    self.bind(head);
                    self.mark_used(head);
                    self.jmp_r(Op::JmpIf, done, end);
                    let t = self.alloc();
                    pieces.push((open, self.asm.len()));
                    self.jmp_rx(Op::IterNext, t, rec, exh);
                    open = self.asm.len();
                    self.set_live(t);
                    self.op(Op::ArrayPush, arr, t, 0);
                    self.jmp_back(head);
                    self.bind(exh);
                    self.op(Op::LoadTrue, done, 0, 0);
                    self.bind(end);
                    self.bind_src(target, Src::Reg(arr), mode)?;
                }
                _ => {
                    let t = self.alloc();
                    let (undef, exhausted, got) = (self.label(), self.label(), self.label());
                    self.jmp_r(Op::JmpIf, done, undef);
                    pieces.push((open, self.asm.len()));
                    self.jmp_rx(Op::IterNext, t, rec, exhausted);
                    open = self.asm.len();
                    self.jmp(got);
                    self.bind(exhausted);
                    self.op(Op::LoadTrue, done, 0, 0);
                    self.bind(undef);
                    self.op(Op::LoadUndef, t, 0, 0);
                    self.bind(got);
                    self.set_live(t);
                    if !matches!(ep.kind, PatKind::Hole) {
                        self.bind_src(el, Src::Reg(t), mode)?;
                    }
                }
            }
            self.free(m2);
        }
        pieces.push((open, self.asm.len()));
        let end = self.label();
        self.jmp_r(Op::JmpIf, done, end);
        self.op(Op::IterClose, rec, 0, 0);
        self.bind(end);
        if let Some(ex) = ex {
            let label = self.label();
            self.mark_used(label);
            let depth = self.depth;
            for (start, end) in pieces {
                self.handlers.push(HandlerRec { start, end, target: label, reg: ex, depth });
            }
            self.cold.push(Cold { label, live: snapshot, rec, done, ex });
        }
        self.free(m);
        Ok(())
    }
}

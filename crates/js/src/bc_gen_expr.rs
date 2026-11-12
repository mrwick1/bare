//! Expressions of the code generator (`bc_gen.rs`). Each one writes its value into the register `dst` that the caller gives and
//! keeps the order of evaluation of the tree walker (`eval_expr_inner`, `link_inner`, `eval_assign`).
use super::*;

/// An assignment target with its base and key evaluated.
enum Place {
    Name(Site, u32),
    /// The object register and the name of `o.name`.
    Field(u32, Span),
    /// The object and key registers of `o[k]`. The key is a property key already (`ToKey`).
    Elem(u32, u32),
}

fn binary(op: BinaryOp) -> Op {
    match op {
        BinaryOp::Add => Op::Add,
        BinaryOp::Sub => Op::Sub,
        BinaryOp::Mul => Op::Mul,
        BinaryOp::Div => Op::Div,
        BinaryOp::Mod => Op::Mod,
        BinaryOp::Exp => Op::Exp,
        BinaryOp::Eq => Op::Eq,
        BinaryOp::Ne => Op::Ne,
        BinaryOp::StrictEq => Op::StrictEq,
        BinaryOp::StrictNe => Op::StrictNe,
        BinaryOp::Lt => Op::Lt,
        BinaryOp::Gt => Op::Gt,
        BinaryOp::Le => Op::Le,
        BinaryOp::Ge => Op::Ge,
        BinaryOp::Shl => Op::Shl,
        BinaryOp::Shr => Op::Shr,
        BinaryOp::UShr => Op::UShr,
        BinaryOp::BitAnd => Op::BitAnd,
        BinaryOp::BitOr => Op::BitOr,
        BinaryOp::BitXor => Op::BitXor,
        BinaryOp::In => Op::In,
        BinaryOp::Instanceof => Op::InstanceOf,
    }
}

/// The jump that skips the right side of `&&`, `||` or `??` (the left value is the result).
fn short(op: LogicalOp) -> Op {
    match op {
        LogicalOp::And => Op::JmpIfNot,
        LogicalOp::Or => Op::JmpIf,
        LogicalOp::Nullish => Op::JmpNotNullish,
    }
}

impl Gen<'_, '_> {
    pub(super) fn expr(&mut self, e: ExprId, dst: u32) -> G<()> {
        let ex = *self.prog.expr(e);
        let saved = std::mem::replace(&mut self.pos, ex.span.start);
        let r = self.expr_inner(e, ex, dst);
        self.pos = saved;
        r
    }

    fn expr_inner(&mut self, e: ExprId, ex: Expr, dst: u32) -> G<()> {
        let prog = self.prog.clone();
        match ex.kind {
            ExprKind::Ident => {
                let n = self.it.expr_name[e.index()];
                let s = self.site(e.index() as u32, n)?;
                self.load(s, n, dst)
            }
            ExprKind::This => self.emit(Op::LoadThis, dst, 0, 0),
            ExprKind::Null => self.emit(Op::LoadNull, dst, 0, 0),
            ExprKind::Bool(b) => self.emit(if b { Op::LoadTrue } else { Op::LoadFalse }, dst, 0, 0),
            ExprKind::Number(base) => {
                let n = self.it.num_lit(ex.span, base);
                if n.fract() == 0.0 && (-32768.0..=32767.0).contains(&n) && !(n == 0.0 && n.is_sign_negative()) {
                    self.emit(Op::LoadI, dst, n as i32 as u32, 0)
                } else {
                    let k = self.konst(KeyOf::Number(n.to_bits()));
                    self.emit(Op::LoadK, dst, k, 0)
                }
            }
            ExprKind::String => {
                let mut u = Vec::new();
                prog.string(ex.span, &mut u).map_err(|_| "string")?;
                let k = self.konst(KeyOf::String(u.into()));
                self.emit(Op::LoadK, dst, k, 0)
            }
            ExprKind::Template(t) => self.template(t, dst),
            ExprKind::Array(l) => {
                self.emit(Op::NewArray, dst, l.len().min(255) as u32, 0)?;
                let t = self.reg()?;
                for x in prog.list(l) {
                    match prog.expr(x).kind {
                        ExprKind::Hole => return Err("hole in an array literal"),
                        ExprKind::Spread(_) => return Err("spread"),
                        _ => {}
                    }
                    self.expr(x, t)?;
                    self.emit(Op::ArrayPush, dst, t, 0)?;
                }
                self.free(t);
                Ok(())
            }
            ExprKind::Object(l) => self.object(l, dst),
            ExprKind::Function(f) => self.emit(Op::Closure, dst, f.index() as u32, 0),
            ExprKind::Unary { op, arg } => self.unary(op, arg, dst),
            ExprKind::Update { op, prefix, arg } => {
                let mark = self.top;
                let p = self.place_of(arg)?;
                let step = if op == UpdateOp::Inc { Op::Inc } else { Op::Dec };
                self.get(&p, dst)?;
                if prefix {
                    self.emit(step, dst, dst, 0)?;
                    self.put(&p, dst)?;
                } else {
                    self.emit(Op::ToNumeric, dst, dst, 0)?;
                    let t = self.reg()?;
                    self.emit(step, t, dst, 0)?;
                    self.put(&p, t)?;
                }
                self.free(mark);
                Ok(())
            }
            ExprKind::Binary { op, left, right } => {
                self.expr(left, dst)?;
                let t = self.reg()?;
                self.expr(right, t)?;
                self.emit(binary(op), dst, dst, t)?;
                self.free(t);
                Ok(())
            }
            ExprKind::Logical { op, left, right } => {
                self.expr(left, dst)?;
                let j = self.jump(short(op), dst)?;
                self.expr(right, dst)?;
                self.patch(j)
            }
            ExprKind::Assign { op, target, value } => self.assign(op, target, value, dst),
            ExprKind::Cond { test, cons, alt } => {
                self.expr(test, dst)?;
                let j = self.jump(Op::JmpIfNot, dst)?;
                self.expr(cons, dst)?;
                let end = self.jump(Op::Jmp, 0)?;
                self.patch(j)?;
                self.expr(alt, dst)?;
                self.patch(end)
            }
            ExprKind::Call { callee, args, optional: false } => self.call(e, callee, args, dst),
            ExprKind::New { callee, args } => {
                let b = self.reg()?;
                self.expr(callee, b)?;
                let n = self.args(args)?;
                self.code.calls.push((self.here() as u32, e.index() as u32));
                self.emit(Op::New, dst, b, n)?;
                self.free(b);
                Ok(())
            }
            ExprKind::Member { obj, name, optional: false } => {
                self.expr(obj, dst)?;
                self.field(Op::GetField, dst, dst, name)
            }
            ExprKind::Index { obj, index, optional: false } => {
                self.expr(obj, dst)?;
                let t = self.reg()?;
                self.expr(index, t)?;
                self.emit(Op::GetElem, dst, dst, t)?;
                self.free(t);
                Ok(())
            }
            ExprKind::Seq(l) => prog.list(l).try_for_each(|x| self.expr(x, dst)),
            ExprKind::Paren(i) => self.expr(i, dst),
            ExprKind::Super => Err("super"),
            ExprKind::Class(_) => Err("class"),
            ExprKind::Tagged { .. } => Err("tagged template"),
            ExprKind::RegExp => Err("regexp literal"),
            ExprKind::BigInt => Err("bigint literal"),
            ExprKind::Chain(_) | ExprKind::Call { .. } | ExprKind::Member { .. } | ExprKind::Index { .. } => Err("optional chain"),
            ExprKind::PrivateMember { .. } | ExprKind::PrivateIn { .. } => Err("private name"),
            ExprKind::NewTarget => Err("new.target"),
            ExprKind::Await(_) | ExprKind::Yield { .. } => Err("await or yield"),
            ExprKind::ImportCall { .. } | ExprKind::ImportMeta => Err("module syntax"),
            ExprKind::Spread(_) | ExprKind::Hole => Err("spread"),
        }
    }

    /// `GetField` or `SetField`: `a` and `b` as the op says, with a new inline cache and the name as the constant.
    fn field(&mut self, op: Op, a: u32, b: u32, name: Span) -> G<()> {
        let n = self.it.name_at(name);
        let k = self.name(n);
        let ic = self.ic();
        self.emit_ins(Ins::new(op, a, b, ic).with_x(k))
    }

    /// An anonymous function takes the name `n` (`eval_named`). Any other expression is as `expr`.
    pub(super) fn named(&mut self, e: ExprId, dst: u32, n: Option<u32>) -> G<()> {
        match (self.prog.expr(e).kind, n) {
            (ExprKind::Function(f), Some(n)) if self.prog.func(f).name.is_none() => {
                let k = self.name(n);
                let saved = std::mem::replace(&mut self.pos, self.prog.expr(e).span.start);
                let r = self.emit_ins(Ins::new(Op::ClosureName, dst, k, 0).with_x(f.index() as u32));
                self.pos = saved;
                r
            }
            (ExprKind::Paren(i), Some(_)) => self.named(i, dst, n),
            (ExprKind::Class(_), Some(_)) => Err("class"),
            _ => self.expr(e, dst),
        }
    }

    fn template(&mut self, t: Id<Template>, dst: u32) -> G<()> {
        let prog = self.prog.clone();
        let t = *prog.template(t);
        let exprs: Vec<ExprId> = prog.template_exprs(&t).collect();
        let base = self.top;
        let mut n = 0;
        for (i, q) in prog.template_quasis(&t).enumerate() {
            let mut u = Vec::new();
            prog.quasi(prog.quasi_node(q), false, &mut u).map_err(|_| "template")?;
            if exprs.is_empty() || !u.is_empty() {
                let k = self.konst(KeyOf::String(u.into()));
                if exprs.is_empty() {
                    return self.emit(Op::LoadK, dst, k, 0);
                }
                let r = self.reg()?;
                self.emit(Op::LoadK, r, k, 0)?;
                n += 1;
            }
            if let Some(&x) = exprs.get(i) {
                let r = self.reg()?;
                self.expr(x, r)?;
                self.emit(Op::ToStr, r, r, 0)?;
                n += 1;
            }
        }
        self.emit(Op::Concat, dst, base, n)?;
        self.free(base);
        Ok(())
    }

    fn object(&mut self, l: List<Prop>, dst: u32) -> G<()> {
        let prog = self.prog.clone();
        self.emit(Op::NewObject, dst, 0, 0)?;
        let t = self.reg()?;
        for pid in prog.list(l) {
            let p = *prog.prop(pid);
            match p.kind {
                PropKind::Init | PropKind::Shorthand | PropKind::Method => {}
                PropKind::Get | PropKind::Set => return Err("getter or setter"),
                PropKind::Spread => return Err("spread"),
            }
            let s = match p.key {
                Key::Name(sp) => prog.name(sp).into_owned(),
                Key::Str(sp) => {
                    let mut u = Vec::new();
                    prog.string(sp, &mut u).map_err(|_| "string")?;
                    String::from_utf16(&u).map_err(|_| "a key with a lone surrogate")?
                }
                Key::Num(sp) => number_to_string(self.it.num_lit(sp, num_base(prog.text(sp)))),
                _ => return Err("computed key"),
            };
            if p.kind == PropKind::Init && s == "__proto__" {
                return Err("__proto__ in an object literal");
            }
            let n = self.it.intern_name(&s);
            let k = self.name(n);
            self.named(p.value, t, Some(n))?;
            self.emit(Op::DefField, dst, t, k)?;
        }
        self.free(t);
        Ok(())
    }

    fn unary(&mut self, op: UnaryOp, arg: ExprId, dst: u32) -> G<()> {
        let o = match op {
            UnaryOp::Neg => Op::Neg,
            UnaryOp::Plus => Op::Plus,
            UnaryOp::Not => Op::Not,
            UnaryOp::BitNot => Op::BitNot,
            UnaryOp::Typeof => Op::Typeof,
            UnaryOp::Void => Op::LoadUndef,
            UnaryOp::Delete => return Err("delete"),
        };
        let mut a = arg;
        while let ExprKind::Paren(i) = self.prog.expr(a).kind {
            a = i;
        }
        if op == UnaryOp::Typeof && matches!(self.prog.expr(a).kind, ExprKind::Ident) {
            // `typeof` of a missing name is "undefined" (a name in an environment always exists).
            let n = self.it.expr_name[a.index()];
            match self.site(a.index() as u32, n)? {
                Site::Global => {
                    let k = self.name(n);
                    return self.emit(Op::TypeofGlobal, dst, k, 0);
                }
                Site::Dynamic => {
                    let k = self.name(n);
                    return self.emit(Op::TypeofName, dst, k, 0);
                }
                Site::Env { .. } => {}
            }
        }
        self.expr(arg, dst)?;
        self.emit(o, dst, if o == Op::LoadUndef { 0 } else { dst }, 0)
    }

    /// Evaluate the base and the key of an update target (`expr_ref`). The registers stay in use until the caller frees them.
    fn place_of(&mut self, e: ExprId) -> G<Place> {
        let prog = self.prog.clone();
        match prog.expr(e).kind {
            ExprKind::Ident => {
                let n = self.it.expr_name[e.index()];
                Ok(Place::Name(self.site(e.index() as u32, n)?, n))
            }
            ExprKind::Paren(i) => self.place_of(i),
            ExprKind::Member { obj, name, optional: false } => {
                let o = self.reg()?;
                self.expr(obj, o)?;
                Ok(Place::Field(o, name))
            }
            ExprKind::Index { obj, index, optional: false } => {
                let o = self.reg()?;
                self.expr(obj, o)?;
                let k = self.reg()?;
                self.expr(index, k)?;
                self.emit(Op::ToKey, k, o, k)?;
                Ok(Place::Elem(o, k))
            }
            ExprKind::Super | ExprKind::PrivateMember { .. } => Err("private name or super"),
            _ => Err("assignment target"),
        }
    }

    fn get(&mut self, p: &Place, dst: u32) -> G<()> {
        match *p {
            Place::Name(s, n) => self.load(s, n, dst),
            Place::Field(o, name) => self.field(Op::GetField, dst, o, name),
            Place::Elem(o, k) => self.emit(Op::GetElem, dst, o, k),
        }
    }

    fn put(&mut self, p: &Place, src: u32) -> G<()> {
        match *p {
            Place::Name(s, n) => self.store(s, n, src),
            Place::Field(o, name) => self.field(Op::SetField, o, src, name),
            Place::Elem(o, k) => self.emit(Op::SetElem, o, k, src),
        }
    }

    fn assign(&mut self, op: AssignOp, target: PatId, value: ExprId, dst: u32) -> G<()> {
        let mark = self.top;
        let p = match self.prog.pat(target).kind {
            PatKind::Ident => {
                let n = self.it.pat_name[target.index()];
                Place::Name(self.site(self.it.pat_site(target.index()), n)?, n)
            }
            PatKind::Member(me) => {
                // A member target with a member inside a paren has the same place.
                self.place_of(me)?
            }
            _ => return Err("destructuring"),
        };
        let k = if let Place::Name(_, n) = p { Some(n) } else { None };
        match op {
            AssignOp::Assign => {
                self.named(value, dst, k)?;
                self.put(&p, dst)?;
            }
            AssignOp::Compound(b) => {
                self.get(&p, dst)?;
                let t = self.reg()?;
                self.expr(value, t)?;
                self.emit(binary(b), dst, dst, t)?;
                self.put(&p, dst)?;
            }
            AssignOp::Logical(l) => {
                self.get(&p, dst)?;
                let j = self.jump(short(l), dst)?;
                self.named(value, dst, k)?;
                self.put(&p, dst)?;
                self.patch(j)?;
            }
        }
        self.free(mark);
        Ok(())
    }

    /// `f(...)`: the callee in `b`, `this` in `b + 1` (the base of a member callee, in parentheses too), the arguments after.
    fn call(&mut self, e: ExprId, callee: ExprId, args: List<Expr>, dst: u32) -> G<()> {
        let b = self.reg()?;
        let this = self.reg()?;
        let mut c = callee;
        while let ExprKind::Paren(i) = self.prog.expr(c).kind {
            c = i;
        }
        match self.prog.expr(c).kind {
            ExprKind::Member { obj, name, optional: false } => {
                self.expr(obj, this)?;
                let saved = std::mem::replace(&mut self.pos, self.prog.expr(c).span.start);
                let r = self.field(Op::GetField, b, this, name);
                self.pos = saved;
                r?;
            }
            ExprKind::Index { obj, index, optional: false } => {
                self.expr(obj, this)?;
                let k = self.reg()?;
                self.expr(index, k)?;
                self.emit(Op::GetElem, b, this, k)?;
                self.free(k);
            }
            ExprKind::Super => return Err("super"),
            _ => {
                self.expr(callee, b)?;
                self.emit(Op::LoadUndef, this, 0, 0)?;
            }
        }
        let n = self.args(args)?;
        self.code.calls.push((self.here() as u32, e.index() as u32));
        self.emit(Op::Call, dst, b, n)?;
        self.free(b);
        Ok(())
    }

    /// Evaluate the arguments into the next registers. Gives their count.
    fn args(&mut self, l: List<Expr>) -> G<u32> {
        let prog = self.prog.clone();
        for a in prog.list(l) {
            if let ExprKind::Spread(_) = prog.expr(a).kind {
                return Err("spread");
            }
            let r = self.reg()?;
            self.expr(a, r)?;
        }
        Ok(l.len() as u32)
    }
}

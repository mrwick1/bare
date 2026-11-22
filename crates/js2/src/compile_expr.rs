//! Expressions of the compiler: value (`to`, `any`), effect (`effect`) and test (`cond`) contexts. See the module comment of
//! `compile.rs`.
use super::*;

/// A constant that a compare-jump or a store can name in the instruction.
#[derive(Clone, Copy)]
pub(crate) enum K {
    Int(i32),
    Str(u32),
    Null,
    Undef,
    Bool(bool),
}

/// The most elements or properties that a literal fills in one op. More go one by one (the window would be large).
const LIT_MAX: u32 = 64;

/// Does the text of an expression hold an assignment, `++` or `--`? A conservative scan: a `=` that is no part of a comparison, an arrow
/// or `==`. A string that holds one is a false hit, which only costs a copy.
fn text_assigns(t: &str) -> bool {
    let b = t.as_bytes();
    for i in 0..b.len() {
        match b[i] {
            b'=' => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                let prev2 = if i > 1 { b[i - 2] } else { 0 };
                let next = b.get(i + 1).copied().unwrap_or(0);
                if next == b'=' || next == b'>' {
                    continue;
                }
                match prev {
                    b'=' | b'!' => continue,
                    b'<' | b'>' if prev2 != prev => continue,
                    _ => return true,
                }
            }
            b'+' | b'-' if b.get(i + 1) == Some(&b[i]) => return true,
            _ => {}
        }
    }
    false
}

/// A property name that is an array index must be an element, not a named slot.
pub(crate) fn index_like(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0')) && s.parse::<u64>().is_ok_and(|v| v < u32::MAX as u64)
}

fn fits_i16(v: i32) -> bool {
    (-32768..=32767).contains(&v)
}

/// `(register op, immediate op, the operands commute when one is a constant)`.
fn arith(op: BinaryOp) -> Option<(Op, Option<Op>, bool)> {
    use BinaryOp::*;
    Some(match op {
        Add => (Op::Add, Some(Op::AddI), false),
        Sub => (Op::Sub, Some(Op::SubI), false),
        Mul => (Op::Mul, Some(Op::MulI), true),
        Div => (Op::Div, Some(Op::DivI), false),
        Mod => (Op::Mod, Some(Op::ModI), false),
        Exp => (Op::Exp, None, false),
        BitAnd => (Op::BitAnd, Some(Op::BitAndI), true),
        BitOr => (Op::BitOr, Some(Op::BitOrI), true),
        BitXor => (Op::BitXor, None, true),
        Shl => (Op::Shl, Some(Op::ShlI), false),
        Shr => (Op::Shr, Some(Op::ShrI), false),
        UShr => (Op::UShr, Some(Op::UShrI), false),
        _ => return None,
    })
}

/// The op with a 32-bit constant in the word `x`, for a constant that does not fit the immediate.
fn xarith(op: BinaryOp) -> Option<Op> {
    use BinaryOp::*;
    Some(match op {
        Add => Op::AddX,
        Sub => Op::SubX,
        Mul => Op::MulX,
        Mod => Op::ModX,
        _ => return None,
    })
}

fn compare_op(op: BinaryOp) -> Option<Op> {
    use BinaryOp::*;
    Some(match op {
        Eq => Op::Eq,
        Ne => Op::Ne,
        StrictEq => Op::StrictEq,
        StrictNe => Op::StrictNe,
        Lt => Op::Lt,
        Le => Op::Le,
        Gt => Op::Gt,
        Ge => Op::Ge,
        In => Op::In,
        Instanceof => Op::InstanceOf,
        _ => return None,
    })
}

impl<'a> Fc<'a> {
    pub(crate) fn expr_kind(&self, e: ExprId) -> ExprKind {
        self.prog.expr(e).kind
    }

    pub(crate) fn name_of(&self, span: Span) -> Cow<'a, str> {
        let prog = self.prog;
        prog.name(span)
    }

    /// May the text of `e` assign a variable?
    pub(crate) fn may_assign(&self, e: ExprId) -> bool {
        let s = self.prog.expr(e).span;
        text_assigns(self.src.get(s.start as usize..s.end as usize).unwrap_or(""))
    }

    /// The integer value of a literal, `-literal` included (not `-0`).
    pub(crate) fn int_const(&self, e: ExprId) -> Option<i32> {
        let x = self.prog.expr(e);
        let v = match x.kind {
            ExprKind::Number(base) => self.prog.number(x.span, base)?,
            ExprKind::Unary { op: UnaryOp::Neg, arg } => match self.expr_kind(arg) {
                ExprKind::Number(base) => -self.prog.number(self.prog.expr(arg).span, base)?,
                _ => return None,
            },
            _ => return None,
        };
        (v.fract() == 0.0 && v.abs() < 2147483648.0 && !(v == 0.0 && v.is_sign_negative())).then_some(v as i32)
    }

    /// Is `e` the global `undefined` (not shadowed)?
    fn is_undefined(&self, e: ExprId) -> bool {
        let x = self.prog.expr(e);
        matches!(x.kind, ExprKind::Ident) && self.name_of(x.span) == "undefined" && matches!(self.resolve("undefined", x.span.start), Res::Global)
            && self.dynamic_hops("undefined").is_empty()
    }

    pub(crate) fn konst(&mut self, e: ExprId) -> CR<Option<K>> {
        Ok(match self.expr_kind(e) {
            ExprKind::Null => Some(K::Null),
            ExprKind::Bool(b) => Some(K::Bool(b)),
            ExprKind::String => Some(K::Str(self.string_atom(self.prog.expr(e).span)?)),
            ExprKind::Ident if self.is_undefined(e) => Some(K::Undef),
            _ => self.int_const(e).map(K::Int),
        })
    }

    // ------------------------------------------------------------ variables

    /// Keep the resolved object in a register. Later work may change the names the scope object contains.
    fn dynamic_ref(&mut self, name: &str, this: Option<u32>) -> CR<Option<u32>> {
        let hops = self.dynamic_hops(name);
        if hops.is_empty() {
            return Ok(None);
        }
        let obj = self.alloc();
        let found = self.label();
        for (hop, kind) in hops {
            let s = self.site_of(name)?;
            self.opx(Op::FindScope, obj, hop, kind, s);
            self.set_live(obj);
            if let Some(this) = this {
                self.op(if kind == 0 { Op::Mov } else { Op::LoadUndef }, this, if kind == 0 { obj } else { 0 }, 0);
                self.set_live(this);
            }
            self.jmp_r(Op::JSNeUndef, obj, found);
        }
        self.bind(found);
        Ok(Some(obj))
    }

    fn use_dynamic(
        &mut self,
        object: Option<u32>,
        when_found: impl FnOnce(&mut Self, u32) -> CR,
        fallback: impl FnOnce(&mut Self) -> CR,
    ) -> CR {
        if let Some(object) = object {
            let missing = self.label();
            let done = self.label();
            self.jmp_r(Op::JSEqUndef, object, missing);
            when_found(self, object)?;
            self.jmp(done);
            self.bind(missing);
            fallback(self)?;
            self.bind(done);
            Ok(())
        } else {
            fallback(self)
        }
    }

    /// Write the value of variable `name` (used at offset `at`) into `dst`.
    pub(crate) fn read_var(&mut self, name: &str, at: u32, dst: u32) -> CR {
        let m = self.mark();
        let object = self.dynamic_ref(name, None)?;
        self.read_ref(name, at, dst, object)?;
        self.free(m);
        Ok(())
    }

    fn read_ref(&mut self, name: &str, at: u32, dst: u32, object: Option<u32>) -> CR {
        self.use_dynamic(object, |c, o| {
            let s = c.site_of(name)?;
            c.op(Op::ScopeGet, dst, o, s);
            Ok(())
        }, |c| c.read_static(name, at, dst))
    }

    fn read_static(&mut self, name: &str, at: u32, dst: u32) -> CR {
        match self.resolve(name, at) {
            Res::Reg { reg, tdz, .. } => {
                if tdz {
                    let a = self.atom(name)?;
                    self.op(Op::CheckInit, reg, a, 0);
                }
                if reg != dst {
                    self.op(Op::Mov, dst, reg, 0);
                }
            }
            Res::Ctx { hops, slot, tdz, env: true, .. } => self.read_env(name, hops, slot, tdz, dst)?,
            Res::Ctx { hops, slot, tdz, .. } => {
                if tdz {
                    let a = self.atom(name)?;
                    self.opx(Op::GetCtxTdz, dst, hops, slot, a);
                } else {
                    self.op(Op::GetCtx, dst, hops, slot);
                }
            }
            Res::Global => match name {
                "undefined" => self.op(Op::LoadUndef, dst, 0, 0),
                "NaN" => self.load_number(dst, f64::NAN),
                "Infinity" => self.load_number(dst, f64::INFINITY),
                "arguments" if !self.is_script && !self.is_arrow => {
                    self.unsup("the arguments object");
                    self.op(Op::LoadUndef, dst, 0, 0);
                }
                _ => {
                    if self.pre
                        && let Some(slot) = crate::prelude_pre::slot(name)
                    {
                        self.op(Op::GetPre, dst, slot, 0);
                        return Ok(());
                    }
                    let s = self.site_of(name)?;
                    self.op(if self.scoped { Op::GetName } else { Op::GetGlobal }, dst, s, 0);
                }
            },
        }
        Ok(())
    }

    pub(crate) fn load_number(&mut self, dst: u32, v: f64) {
        if v.fract() == 0.0 && v.abs() < 2147483648.0 && !(v == 0.0 && v.is_sign_negative()) {
            self.op(Op::LoadI, dst, v as i32 as u32, 0);
        } else {
            let n = self.number(v);
            self.op(Op::LoadN, dst, n, 0);
        }
    }

    /// Store register `src` into variable `name`. `init`: the first write of a declaration (no dead-zone or constant check).
    pub(crate) fn store_var(&mut self, name: &str, at: u32, src: u32, init: bool) -> CR {
        let m = self.mark();
        if init {
            self.store_static(name, at, src, true)?;
        } else {
            let object = self.dynamic_ref(name, None)?;
            self.store_ref(name, at, src, object)?;
        }
        self.free(m);
        Ok(())
    }

    fn store_ref(&mut self, name: &str, at: u32, src: u32, object: Option<u32>) -> CR {
        self.use_dynamic(object, |c, o| {
            let s = c.site_of(name)?;
            c.op(Op::ScopeSet, o, src, s);
            Ok(())
        }, |c| c.store_static(name, at, src, false))
    }

    fn store_static(&mut self, name: &str, at: u32, src: u32, init: bool) -> CR {
        let res = self.resolve(name, at);
        // Sloppy code assigns the name of its own function expression with no effect.
        if !init && !self.strict && matches!(res, Res::Reg { konst: true, .. } | Res::Ctx { konst: true, .. }) && self.is_callee(name) {
            return Ok(());
        }
        match res {
            Res::Reg { reg, tdz, konst } => {
                if konst && !init {
                    return self.throw_const(name, tdz, reg);
                }
                if tdz && !init {
                    let a = self.atom(name)?;
                    self.op(Op::CheckInit, reg, a, 0);
                }
                if reg != src {
                    self.op(Op::Mov, reg, src, 0);
                }
                self.set_live(reg);
            }
            Res::Ctx { hops, slot, tdz, konst, env: true } => self.store_env(name, hops, slot, tdz, konst, src, init)?,
            Res::Ctx { hops, slot, tdz, konst, .. } => {
                if konst && !init {
                    let a = self.atom(name)?;
                    if tdz {
                        self.opx(Op::GetCtxTdz, src, hops, slot, a);
                    }
                    self.op(Op::ThrowConst, 0, a, 0);
                } else if init {
                    self.op(Op::InitCtx, src, hops, slot);
                } else if tdz {
                    let a = self.atom(name)?;
                    self.opx(Op::SetCtxTdz, src, hops, slot, a);
                } else {
                    self.op(Op::SetCtx, src, hops, slot);
                }
            }
            Res::Global => {
                if self.pre && crate::prelude_pre::slot(name).is_some() {
                    self.unsup("an assignment to a slot of the prelude");
                }
                let s = self.site_of(name)?;
                self.op(if self.scoped { Op::SetName } else if self.strict && !init { Op::SetGlobalS } else { Op::SetGlobal }, src, s, 0);
            }
        }
        Ok(())
    }

    fn throw_const(&mut self, name: &str, tdz: bool, reg: u32) -> CR {
        let a = self.atom(name)?;
        if tdz {
            self.op(Op::CheckInit, reg, a, 0);
        }
        self.op(Op::ThrowConst, 0, a, 0);
        Ok(())
    }

    // ------------------------------------------------------------ value context

    /// Evaluate `e` into register `dst`. The register is live when this returns.
    pub(crate) fn to(&mut self, e: ExprId, dst: u32) -> CR {
        self.emit_value(e, dst)?;
        self.set_live(dst);
        Ok(())
    }

    /// Evaluate `e` and give a register that holds the value: the register of a register variable, in place, or a new temporary. The
    /// caller frees temporaries with the mark it took before.
    pub(crate) fn any(&mut self, e: ExprId) -> CR<u32> {
        let x = *self.prog.expr(e);
        if matches!(x.kind, ExprKind::Ident) {
            let name = self.name_of(x.span);
            if let Res::Reg { reg, tdz, .. } = self.resolve(&name, x.span.start)
                && self.dynamic_hops(&name).is_empty()
            {
                if tdz {
                    let a = self.atom(&name)?;
                    self.op(Op::CheckInit, reg, a, 0);
                }
                return Ok(reg);
            }
        }
        let t = self.alloc();
        self.to(e, t)?;
        Ok(t)
    }

    /// As `any`, for the left operand of an operation whose right operand `later` runs after it: a register variable that `later` may
    /// assign is copied first.
    pub(crate) fn any_before(&mut self, e: ExprId, later: ExprId) -> CR<u32> {
        let r = self.any(e)?;
        if self.is_var_reg(r) && self.may_clobber(r, later) {
            let t = self.alloc();
            self.op(Op::Mov, t, r, 0);
            self.set_live(t);
            return Ok(t);
        }
        Ok(r)
    }

    /// May `e` assign the register variable that lives in `reg`?
    pub(crate) fn may_clobber(&self, reg: u32, e: ExprId) -> bool {
        self.may_assign(e) && self.reads_reg(reg, e)
    }

    /// Does the text of `e` mention the register variable that lives in `reg`? A register that no variable owns counts as read.
    fn reads_reg(&self, reg: u32, e: ExprId) -> bool {
        let s = self.prog.expr(e).span;
        let var = self.scopes.iter().rev().flat_map(|sc| sc.vars.iter()).find(|v| matches!(v.home, Home::Reg(r) if r == reg));
        var.is_none_or(|v| self.may_occur(&v.name, s.start, s.end))
    }

    /// The register that a multi-step expression may write early, when `es` are the parts that it evaluates after the first write. A
    /// register variable is not one when a part could read it.
    fn scratch(&mut self, dst: u32, es: &[ExprId]) -> u32 {
        if self.is_var_reg(dst) && es.iter().any(|&e| self.reads_reg(dst, e)) { self.alloc() } else { dst }
    }

    fn emit_value(&mut self, e: ExprId, dst: u32) -> CR {
        let x = *self.prog.expr(e);
        match x.kind {
            ExprKind::Ident => {
                let name = self.name_of(x.span);
                self.read_var(&name, x.span.start, dst)?;
            }
            ExprKind::This => self.this(dst)?,
            ExprKind::Null => self.op(Op::LoadNull, dst, 0, 0),
            ExprKind::Bool(b) => self.op(if b { Op::LoadTrue } else { Op::LoadFalse }, dst, 0, 0),
            ExprKind::Number(base) => {
                let v = self.prog.number(x.span, base).unwrap_or(f64::NAN);
                self.load_number(dst, v);
            }
            ExprKind::String => {
                let a = self.string_atom(x.span)?;
                self.op(Op::LoadAtom, dst, a, 0);
            }
            ExprKind::Template(t) => self.template(t, dst)?,
            ExprKind::Array(_) | ExprKind::Object(_)
                if x.span.end - x.span.start >= crate::data_lit::MIN_BYTES && self.try_data_literal(e, dst)? => {}
            ExprKind::Array(list) => self.array(list, dst)?,
            ExprKind::Object(list) => self.object(list, dst)?,
            ExprKind::Function(f) => self.function(f, dst, true)?,
            ExprKind::Unary { op, arg } => self.unary(op, arg, dst)?,
            ExprKind::Update { op, prefix, arg } => self.update(op, prefix, arg, Some(dst))?,
            ExprKind::Binary { op, left, right } => self.binary(op, left, right, dst)?,
            ExprKind::Logical { op, left, right } => {
                let m = self.mark();
                let d = self.scratch(dst, &[right]);
                self.to(left, d)?;
                let end = self.label();
                let jop = match op {
                    LogicalOp::And => Op::JmpIfNot,
                    LogicalOp::Or => Op::JmpIf,
                    LogicalOp::Nullish => Op::JmpNotNullish,
                };
                self.jmp_r(jop, d, end);
                self.to(right, d)?;
                self.bind(end);
                if d != dst {
                    self.op(Op::Mov, dst, d, 0);
                }
                self.free(m);
            }
            ExprKind::Assign { op, target, value } => self.assign(op, target, value, Some(dst), x.span)?,
            ExprKind::Cond { test, cons, alt } => {
                let (els, end) = (self.label(), self.label());
                self.cond(test, false, els)?;
                self.to(cons, dst)?;
                self.jmp(end);
                self.bind(els);
                self.to(alt, dst)?;
                self.bind(end);
            }
            ExprKind::Call { callee, args, optional } => self.call(e, callee, args, optional, dst)?,
            ExprKind::New { callee, args } => self.new_expr(e, callee, args, dst)?,
            ExprKind::Member { obj, name, optional } => {
                let nm = self.name_of(name);
                let m = self.mark();
                if matches!(self.expr_kind(obj), ExprKind::Super) {
                    let t = self.this_reg()?;
                    let s = self.site_of(&nm)?;
                    self.line(x.span.start);
                    self.op(Op::GetSuper, dst, t, s);
                } else {
                    let o = self.any(obj)?;
                    if optional {
                        self.short(o);
                    }
                    let s = self.site_of(&nm)?;
                    self.line(x.span.start);
                    self.op(Op::GetField, dst, o, s);
                }
                self.free(m);
            }
            ExprKind::Index { obj, index, optional } => {
                let m = self.mark();
                if matches!(self.expr_kind(obj), ExprKind::Super) {
                    let t = self.this_reg()?;
                    let k = self.any(index)?;
                    self.line(x.span.start);
                    self.op(Op::GetSuperElem, dst, t, k);
                } else {
                    let o = self.any_before(obj, index)?;
                    if optional {
                        self.short(o);
                    }
                    self.line(x.span.start);
                    self.get_index(dst, o, index)?;
                }
                self.free(m);
            }
            ExprKind::PrivateMember { obj, name, optional } => {
                let m = self.mark();
                let o = self.any(obj)?;
                if optional {
                    self.short(o);
                }
                let k = self.private_key(name)?;
                self.line(x.span.start);
                self.op(Op::GetPrivate, dst, o, k);
                self.free(m);
            }
            ExprKind::PrivateIn { name, right } => {
                let m = self.mark();
                let o = self.any(right)?;
                let k = self.private_key(name)?;
                self.line(x.span.start);
                self.op(Op::HasPrivate, dst, o, k);
                self.free(m);
            }
            ExprKind::Seq(list) => {
                let items: Vec<ExprId> = self.prog.list(list).collect();
                for (i, it) in items.iter().enumerate() {
                    if i + 1 == items.len() {
                        self.to(*it, dst)?;
                    } else {
                        self.effect(*it)?;
                    }
                }
            }
            ExprKind::Chain(inner) => {
                let m = self.mark();
                let d = self.scratch(dst, &[inner]);
                let (short, done) = (self.label(), self.label());
                let saved = self.chain_short.replace(short);
                self.to(inner, d)?;
                self.chain_short = saved;
                self.jmp(done);
                self.bind(short);
                self.op(Op::LoadUndef, d, 0, 0);
                self.bind(done);
                if d != dst {
                    self.op(Op::Mov, dst, d, 0);
                }
                self.free(m);
            }
            ExprKind::BigInt => {
                let t = self.src.get(x.span.start as usize..x.span.end as usize).unwrap_or("0n");
                let digits: String = t.trim_end_matches('n').chars().filter(|c| *c != '_').collect();
                let a = self.atom(&digits)?;
                self.op(Op::LoadBigInt, dst, a, 0);
            }
            ExprKind::RegExp => {
                // `/body/flags`: the flags follow the last `/`.
                let t = self.src.get(x.span.start as usize..x.span.end as usize).unwrap_or("//");
                let cut = t.rfind('/').unwrap_or(t.len() - 1).max(1);
                let (body, flags) = (self.atom(&t[1..cut])?, self.atom(&t[cut + 1..])?);
                self.op(Op::NewRegExp, dst, body, flags);
            }
            ExprKind::Tagged { tag, quasi } => {
                let subs: Vec<ExprId> = self.prog.template_exprs(self.prog.template(quasi)).collect();
                self.call_parts(e, tag, Some(quasi), subs, false, dst)?
            }
            ExprKind::Class(c) => self.class(c, dst)?,
            ExprKind::Super => self.not_built(dst, "super outside a call or a member access"),
            ExprKind::NewTarget => {
                if (self.is_arrow || self.eval) && !matches!(self.resolve("new.target", 0), Res::Global) {
                    self.read_var("new.target", 0, dst)?;
                } else {
                    self.load_new_target(dst);
                }
            }
            ExprKind::Await(a) => self.await_expr(a, dst)?,
            ExprKind::Yield { arg, delegate } => self.yield_expr(arg, delegate, dst)?,
            ExprKind::Spread(_) => self.not_built(dst, "spread"),
            ExprKind::Hole => self.op(Op::LoadUndef, dst, 0, 0),
            ExprKind::ImportCall { arg, opts } => self.import_call(arg, opts, dst)?,
            ExprKind::ImportMeta => self.read_var("%meta", 0, dst)?,
        }
        Ok(())
    }

    fn not_built(&mut self, dst: u32, what: &'static str) {
        self.unsup(what);
        self.op(Op::LoadUndef, dst, 0, 0);
    }

    /// `?.`: leave the chain when the value in `r` is nullish.
    fn short(&mut self, r: u32) {
        if let Some(l) = self.chain_short {
            self.jmp_r(Op::JmpNullish, r, l);
        }
    }

    /// `this`. In an arrow, or in a derived constructor, it is a variable (in the derived constructor it is the hole until `super()`
    /// returns, so a read checks it); elsewhere it is the `this` of the frame.
    pub(crate) fn this(&mut self, dst: u32) -> CR {
        if (self.is_arrow || self.derived || self.eval) && !matches!(self.resolve("this", 0), Res::Global) {
            return self.read_var("this", 0, dst);
        }
        self.op(self.load_this_op(), dst, 0, 0);
        Ok(())
    }

    /// `LoadThis` in strict code and in the top level of a script (its `this` is the global object already).
    pub(crate) fn load_this_op(&self) -> Op {
        if self.strict || self.is_script { Op::LoadThis } else { Op::LoadThisS }
    }

    /// `this` in a new temporary register (live).
    fn this_reg(&mut self) -> CR<u32> {
        let t = self.alloc();
        self.this(t)?;
        self.set_live(t);
        Ok(t)
    }

    /// The name of a private name, with the `#`: its variable in the scope of the class.
    pub(crate) fn private_name(&self, span: Span) -> String {
        let n = self.name_of(span);
        if n.starts_with('#') { n.into_owned() } else { format!("#{n}") }
    }

    /// The private name `#name` (a span with the `#`) in a new temporary register: a variable of the class scope.
    pub(crate) fn private_key(&mut self, name: Span) -> CR<u32> {
        let n = self.private_name(name);
        let t = self.alloc();
        self.read_var(&n, 0, t)?;
        self.set_live(t);
        Ok(t)
    }

    /// `dst = o[index]`, with the forms for a constant index.
    fn get_index(&mut self, dst: u32, o: u32, index: ExprId) -> CR {
        if let ExprKind::String = self.expr_kind(index) {
            let mut buf = Vec::new();
            let span = self.prog.expr(index).span;
            if self.prog.string(span, &mut buf).is_ok()
                && let Ok(s) = String::from_utf16(&buf)
                && !index_like(&s)
            {
                let site = self.site_of(&s)?;
                self.op(Op::GetField, dst, o, site);
                return Ok(());
            }
        }
        if let Some(i) = self.int_const(index)
            && (0..=255).contains(&i)
        {
            self.op(Op::GetElemI, dst, o, i as u32);
            return Ok(());
        }
        let k = match self.key_add(index)? {
            Some(k) => k,
            None => self.any(index)?,
        };
        self.op(Op::GetElem, dst, o, k);
        Ok(())
    }

    /// Evaluate an addition whose result is only used as a computed member key.
    fn key_add(&mut self, e: ExprId) -> CR<Option<u32>> {
        let ExprKind::Binary { op: BinaryOp::Add, left, right } = self.expr_kind(e) else { return Ok(None) };
        let dst = self.alloc();
        let m = self.mark();
        let l = self.any_before(left, right)?;
        let r = self.any(right)?;
        self.op(Op::AddKey, dst, l, r);
        self.free(m);
        self.set_live(dst);
        Ok(Some(dst))
    }

    fn template(&mut self, t: Id<Template>, dst: u32) -> CR {
        let prog = self.prog;
        let tm = *prog.template(t);
        let quasis: Vec<Id<Quasi>> = prog.template_quasis(&tm).collect();
        let exprs: Vec<ExprId> = prog.template_exprs(&tm).collect();
        let mut parts: Vec<Result<u32, ExprId>> = Vec::new();
        let mut buf = Vec::new();
        for (i, q) in quasis.iter().enumerate() {
            buf.clear();
            if prog.quasi(prog.quasi_node(*q), false, &mut buf).is_ok() && !buf.is_empty() {
                parts.push(Ok(self.atom_units(&buf)?));
            }
            if let Some(&ex) = exprs.get(i) {
                parts.push(Err(ex));
            }
        }
        if parts.is_empty() {
            let a = self.atom("")?;
            self.op(Op::LoadAtom, dst, a, 0);
            return Ok(());
        }
        let m = self.mark();
        let base = self.window(parts.len() as u32);
        for (i, p) in parts.iter().enumerate() {
            let r = base + i as u32;
            match p {
                Ok(a) => self.op(Op::LoadAtom, r, *a, 0),
                Err(ex) => {
                    self.to(*ex, r)?;
                    self.op(Op::ToStr, r, r, 0);
                }
            }
            self.set_live(r);
        }
        if parts.len() == 1 {
            self.op(Op::Mov, dst, base, 0);
        } else {
            self.op(Op::Concat, dst, base, parts.len() as u32);
        }
        self.free(m);
        Ok(())
    }

    fn array(&mut self, list: List<Expr>, dst: u32) -> CR {
        let items: Vec<ExprId> = self.prog.list(list).collect();
        let irregular = items.iter().any(|&i| matches!(self.expr_kind(i), ExprKind::Spread(_) | ExprKind::Hole));
        let m = self.mark();
        let d = self.scratch(dst, &items);
        if irregular {
            self.op(Op::NewArray, d, 0, 0);
            self.set_live(d);
            self.fill_array(d, &items)?;
        } else if items.is_empty() {
            self.op(Op::NewArray, d, 0, 0);
        } else if items.len() as u32 <= LIT_MAX {
            let base = self.window(items.len() as u32);
            for (i, it) in items.iter().enumerate() {
                self.to(*it, base + i as u32)?;
            }
            self.op(Op::NewArrLit, d, base, items.len() as u32);
        } else {
            self.op(Op::NewArray, d, items.len().min(u16::MAX as usize) as u32, 0);
            self.set_live(d);
            for it in items {
                let m2 = self.mark();
                let v = self.any(it)?;
                self.op(Op::ArrayPush, d, v, 0);
                self.free(m2);
            }
        }
        if d != dst {
            self.op(Op::Mov, dst, d, 0);
        }
        self.free(m);
        Ok(())
    }

    /// The static name of a property key that a slot can hold, or `None` for a computed key or an index.
    /// The value of the number literal that is a property key (`0x10`, `0b1`, `017`, `1_0`).
    pub(crate) fn num_key(&self, s: Span) -> f64 {
        use crate::lex::NumberBase as B;
        let t = self.prog.text(s).as_bytes();
        let base = match t {
            [b'0', b'x' | b'X', ..] => B::Hex,
            [b'0', b'o' | b'O', ..] => B::Octal,
            [b'0', b'b' | b'B', ..] => B::Binary,
            [b'0', b'0'..=b'7', ..] if t.iter().all(|c| (b'0'..=b'7').contains(c)) => B::LegacyOctal,
            [b'0', b'0'..=b'9', ..] => B::NonOctalDecimal,
            _ => B::Decimal,
        };
        self.prog.number(s, base).unwrap_or(0.0)
    }

    pub(crate) fn static_key(&self, key: Key) -> Option<Cow<'a, str>> {
        let prog = self.prog;
        match key {
            Key::Name(s) => Some(prog.name(s)),
            Key::BigInt(s) => {
                let text = self.src.get(s.start as usize..s.end as usize)?;
                let n = crate::bigint::parse_literal(text)?;
                Some(Cow::Owned(crate::bigint::to_string_radix(n.sv(), 10)))
            }
            Key::Str(s) => {
                let mut b = Vec::new();
                prog.string(s, &mut b).ok()?;
                Some(Cow::Owned(String::from_utf16(&b).ok()?))
            }
            _ => None,
        }
    }

    /// Is `e` a method or accessor function that uses `super.x` (its own or in an arrow inside it)? It needs a home object.
    pub(crate) fn uses_super(&self, e: ExprId) -> bool {
        match self.prog.expr(e).kind {
            ExprKind::Function(f) => self.func_uses_super(f),
            _ => false,
        }
    }

    pub(crate) fn func_uses_super(&self, f: FuncId) -> bool {
        matches!(self.prog.func(f).body, Body::Skipped(id) if self.facts.get(id).flags & facts::SUPER != 0)
    }

    /// `SetHome fn, home` for a method that uses `super`.
    pub(crate) fn set_home(&mut self, f: u32, home: u32, e: ExprId) {
        if self.uses_super(e) {
            self.op(Op::SetHome, f, home, 0);
        }
    }

    /// The key of a property for a define op: `Ok(name)` for a name that a site holds, `Err(reg)` for a register that holds the key
    /// (a computed key after `ToKey`, an index). `d` is the object (a `ToKey` of a computed key reads it for the error).
    pub(crate) fn prop_key(&mut self, d: u32, key: Key) -> CR<Result<Cow<'a, str>, u32>> {
        if let Some(n) = self.static_key(key)
            && !index_like(&n)
        {
            return Ok(Ok(n));
        }
        let kt = self.alloc();
        match (self.static_key(key), key) {
            (_, Key::Computed(k)) => {
                let kv = self.any(k)?;
                self.op(Op::ToKey, kt, d, kv);
            }
            (Some(n), _) => self.load_number(kt, n.parse::<f64>().unwrap_or(0.0)),
            (_, Key::Num(s)) => {
                let v = self.num_key(s);
                self.load_number(kt, v);
            }
            _ => {
                self.unsup("a private key in an object literal");
                self.op(Op::LoadUndef, kt, 0, 0);
            }
        }
        self.set_live(kt);
        Ok(Err(kt))
    }

    fn object(&mut self, list: List<Prop>, dst: u32) -> CR {
        let prog = self.prog;
        let props: Vec<Prop> = prog.list(list).map(|p| *prog.prop(p)).collect();
        let mut names: Vec<Cow<'a, str>> = Vec::new();
        // Only the compact opcode needs unique keys. Its register window limits the scan to LIT_MAX entries.
        if props.len() as u32 <= LIT_MAX {
            for p in &props {
                let ok_kind = matches!(p.kind, PropKind::Init | PropKind::Shorthand | PropKind::Method) && !self.uses_super(p.value);
                match self.static_key(p.key) {
                    Some(n) if ok_kind && !(index_like(&n) || (n == "__proto__" && p.kind == PropKind::Init) || names.contains(&n)) => names.push(n),
                    _ => break,
                }
            }
        }
        let m = self.mark();
        let values: Vec<ExprId> = props.iter().map(|p| p.value).collect();
        let d = self.scratch(dst, &values);
        if props.is_empty() {
            self.op(Op::NewObject, d, 0, 0);
        } else if names.len() == props.len() {
            let base = self.window(props.len() as u32);
            let mut keys = Vec::with_capacity(props.len());
            for (i, p) in props.iter().enumerate() {
                self.name_hint(p.value, &names[i]);
                self.to(p.value, base + i as u32)?;
                keys.push(self.atom(&names[i])?);
            }
            let r = self.recipes.push(&keys) as u32;
            self.op(Op::NewObjLit, d, base, r);
        } else {
            self.op(Op::NewObject, d, props.len().min(255) as u32, 0);
            self.set_live(d);
            for p in &props {
                let m2 = self.mark();
                match p.kind {
                    PropKind::Spread => {
                        let v = self.any(p.value)?;
                        self.op(Op::CopyProps, d, v, 0);
                    }
                    PropKind::Get | PropKind::Set => {
                        let key = self.prop_key(d, p.key)?;
                        let setter = (p.kind == PropKind::Set) as u32;
                        self.name_for_key(p.value, &key, 1 + setter);
                        let v = self.any(p.value)?;
                        self.name_at_run(p.value, v, &key, 1 + setter);
                        self.set_home(v, d, p.value);
                        match key {
                            Ok(n) => {
                                let s = self.site_of(&n)?;
                                self.opx(Op::DefAccessor, d, v, s, setter);
                            }
                            Err(k) => self.opx(Op::DefAccessorElem, d, k, v, setter),
                        }
                    }
                    _ if p.kind == PropKind::Init && self.static_key(p.key).is_some_and(|n| n == "__proto__") => {
                        let v = self.any(p.value)?;
                        self.op(Op::SetProto, d, v, 0);
                    }
                    _ => {
                        let key = self.prop_key(d, p.key)?;
                        self.name_for_key(p.value, &key, 0);
                        let v = self.any(p.value)?;
                        self.name_at_run(p.value, v, &key, 0);
                        if p.kind == PropKind::Method {
                            self.set_home(v, d, p.value);
                        }
                        match key {
                            Ok(n) => {
                                let s = self.site_of(&n)?;
                                self.op(Op::DefField, d, v, s);
                            }
                            Err(k) => self.op(Op::DefElem, d, k, v),
                        }
                    }
                }
                self.free(m2);
            }
        }
        if d != dst {
            self.op(Op::Mov, dst, d, 0);
        }
        self.free(m);
        Ok(())
    }

    /// The anonymous function or class `e` gets the name `name` when it is made (the spec's `NamedEvaluation`).
    pub(crate) fn name_hint(&mut self, e: ExprId, name: &str) {
        match self.prog.expr(e).kind {
            ExprKind::Function(f) if self.prog.func(f).name.is_none() => self.hint = Some(Hint::Func(f, name.into())),
            ExprKind::Class(c) if self.prog.class(c).name.is_none() => self.hint = Some(Hint::Class(c, name.into())),
            _ => {}
        }
    }

    /// Is `e` an anonymous function or class definition?
    pub(crate) fn is_anon_def(&self, e: ExprId) -> bool {
        match self.prog.expr(e).kind {
            ExprKind::Function(f) => self.prog.func(f).name.is_none(),
            ExprKind::Class(c) => self.prog.class(c).name.is_none(),
            _ => false,
        }
    }

    /// `name_hint` for a property key that the text gives (`prefix` 1: `get `, 2: `set `). A computed key names the function at run time
    /// ([`Fc::name_at_run`]).
    pub(crate) fn name_for_key(&mut self, e: ExprId, key: &Result<Cow<'a, str>, u32>, prefix: u32) {
        if let Ok(n) = key {
            match prefix {
                0 => self.name_hint(e, n),
                p => self.name_hint(e, &format!("{}{n}", if p == 1 { "get " } else { "set " })),
            }
        }
    }

    /// After the function in `v` was made for the computed key in a register: `SetFnName` when it is an anonymous definition.
    pub(crate) fn name_at_run(&mut self, e: ExprId, v: u32, key: &Result<Cow<'a, str>, u32>, prefix: u32) {
        if let Err(k) = key
            && self.is_anon_def(e)
        {
            self.set_live(v);
            self.op(Op::SetFnName, v, *k, prefix);
        }
    }

    pub(crate) fn hint_for_func(&mut self, f: FuncId) -> Option<Box<str>> {
        match self.hint.take() {
            Some(Hint::Func(g, n)) if g == f => Some(n),
            h => {
                self.hint = h;
                None
            }
        }
    }

    pub(crate) fn hint_for_class(&mut self, c: ClassId) -> Option<Box<str>> {
        match self.hint.take() {
            Some(Hint::Class(d, n)) if d == c => Some(n),
            h => {
                self.hint = h;
                None
            }
        }
    }

    /// A function value: a closure over the current context. `expr`: a function expression (a name that it uses is its own binding).
    pub(crate) fn function(&mut self, f: FuncId, dst: u32, expr: bool) -> CR {
        let func = *self.prog.func(f);
        let name = match func.name {
            Some(n) => Some(Box::from(&*self.prog.name(n))),
            None => self.hint_for_func(f),
        };
        let self_name = expr && func.name.is_some() && func.kind == FuncKind::Normal;
        let i = self.child(f, self_name, name)?;
        self.op(Op::Closure, dst, i, 0);
        Ok(())
    }

    fn unary(&mut self, op: UnaryOp, arg: ExprId, dst: u32) -> CR {
        let m = self.mark();
        match op {
            UnaryOp::Void => {
                self.effect(arg)?;
                self.op(Op::LoadUndef, dst, 0, 0);
            }
            UnaryOp::Typeof => {
                let a = *self.prog.expr(arg);
                let name = (matches!(a.kind, ExprKind::Ident)).then(|| self.name_of(a.span));
                match name {
                    Some(n) if matches!(self.resolve(&n, a.span.start), Res::Global) && (n != "undefined" || !self.dynamic_hops(&n).is_empty()) => {
                        let object = self.dynamic_ref(&n, None)?;
                        self.use_dynamic(object, |c, o| {
                            let t = c.alloc();
                            let s = c.site_of(&n)?;
                            c.op(Op::ScopeGet, t, o, s);
                            c.op(Op::Typeof, dst, t, 0);
                            c.free(t);
                            Ok(())
                        }, |c| {
                            let s = c.site_of(&n)?;
                            c.op(if c.scoped { Op::TypeofName } else { Op::TypeofGlobal }, dst, s, 0);
                            Ok(())
                        })?;
                    }
                    _ => {
                        let r = self.any(arg)?;
                        self.op(Op::Typeof, dst, r, 0);
                    }
                }
            }
            UnaryOp::Delete => self.delete(arg, dst)?,
            UnaryOp::Neg => {
                // `-5` is a constant, and `-x` is `Neg`.
                if let Some(v) = self.int_const(arg).filter(|v| *v != i32::MIN) {
                    self.load_number(dst, -(v as f64));
                } else {
                    let r = self.any(arg)?;
                    self.op(Op::Neg, dst, r, 0);
                }
            }
            UnaryOp::Plus => {
                let r = self.any(arg)?;
                self.op(Op::Plus, dst, r, 0);
            }
            UnaryOp::Not => {
                let r = self.any(arg)?;
                self.op(Op::Not, dst, r, 0);
            }
            UnaryOp::BitNot => {
                let r = self.any(arg)?;
                self.op(Op::BitNot, dst, r, 0);
            }
        }
        self.free(m);
        Ok(())
    }

    fn delete(&mut self, arg: ExprId, dst: u32) -> CR {
        let a = *self.prog.expr(arg);
        match a.kind {
            ExprKind::Member { obj, name, .. } => {
                let nm = self.name_of(name);
                let o = self.any(obj)?;
                let k = self.alloc();
                let at = self.atom(&nm)?;
                self.op(Op::LoadAtom, k, at, 0);
                self.set_live(k);
                self.op(Op::Delete, dst, o, k);
            }
            ExprKind::Index { obj, index, .. } => {
                let o = self.any_before(obj, index)?;
                let k = self.any(index)?;
                self.op(Op::Delete, dst, o, k);
            }
            ExprKind::Ident => {
                let n = self.name_of(a.span);
                let object = self.dynamic_ref(&n, None)?;
                self.use_dynamic(object, |c, o| {
                    let k = c.alloc();
                    let at = c.atom(&n)?;
                    c.op(Op::LoadAtom, k, at, 0);
                    c.set_live(k);
                    c.op(Op::Delete, dst, o, k);
                    c.free(k);
                    Ok(())
                }, |c| {
                    if matches!(c.resolve(&n, a.span.start), Res::Global) {
                        let s = c.site_of(&n)?;
                        c.op(Op::DeleteGlobal, dst, s, 0);
                    } else {
                        c.op(Op::LoadFalse, dst, 0, 0);
                    }
                    Ok(())
                })?;
            }
            ExprKind::Chain(_) => {
                self.unsup("delete of an optional chain");
                self.op(Op::LoadTrue, dst, 0, 0);
            }
            _ => {
                self.effect(arg)?;
                self.op(Op::LoadTrue, dst, 0, 0);
            }
        }
        Ok(())
    }

    fn binary(&mut self, op: BinaryOp, left: ExprId, right: ExprId, dst: u32) -> CR {
        let m = self.mark();
        if let Some((rop, iop, commute)) = arith(op) {
            let imm = |c: &Self, e: ExprId| c.int_const(e).filter(|v| fits_i16(*v));
            let shift = matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr);
            let ok = |v: i32| !shift || (0..32).contains(&v);
            if let (Some(iop), Some(v)) = (iop, imm(self, right).filter(|v| ok(*v))) {
                let l = self.any(left)?;
                self.op(iop, dst, l, v as u32);
            } else if let (Some(iop), true, Some(v)) = (iop, commute, imm(self, left)) {
                let r = self.any(right)?;
                self.op(iop, dst, r, v as u32);
            } else if let (Some(xop), Some(v)) = (xarith(op), self.int_const(right)) {
                let l = self.any(left)?;
                self.opx(xop, dst, l, 0, v as u32);
            } else if let (Some(xop), true, Some(v)) = (xarith(op), commute, self.int_const(left)) {
                let r = self.any(right)?;
                self.opx(xop, dst, r, 0, v as u32);
            } else {
                let l = self.any_before(left, right)?;
                let r = self.any(right)?;
                self.op(rop, dst, l, r);
            }
        } else if let Some(cop) = compare_op(op) {
            let l = self.any_before(left, right)?;
            let r = self.any(right)?;
            self.op(cop, dst, l, r);
        }
        self.free(m);
        Ok(())
    }

    // ------------------------------------------------------------ calls

    fn call(&mut self, e: ExprId, callee: ExprId, args: List<Expr>, optional: bool, dst: u32) -> CR {
        let args: Vec<ExprId> = self.prog.list(args).collect();
        self.call_parts(e, callee, None, args, optional, dst)
    }

    /// Fill the array in `arr` with the arguments: `ArrayPush` for a value, `ArraySpread` for `...x`.
    fn fill_array(&mut self, arr: u32, items: &[ExprId]) -> CR {
        for &a in items {
            let m = self.mark();
            if let ExprKind::Spread(inner) = self.expr_kind(a) {
                let v = self.any(inner)?;
                self.op(Op::ArraySpread, arr, v, 0);
            } else if matches!(self.expr_kind(a), ExprKind::Hole) {
                self.op(Op::ArrayHole, arr, 0, 0);
            } else {
                let v = self.any(a)?;
                self.op(Op::ArrayPush, arr, v, 0);
            }
            self.free(m);
        }
        Ok(())
    }

    /// A call. `tpl`: a tagged template, whose template object is the first argument. A call with `...x` among its arguments builds the
    /// arguments as an array (`CallSpread`).
    fn call_parts(&mut self, e: ExprId, callee: ExprId, tpl: Option<Id<Template>>, args: Vec<ExprId>, optional: bool, dst: u32) -> CR {
        let prog = self.prog;
        let mut c = *prog.expr(callee);
        // `(a?.b)()` keeps `a` as `this`: the member is the callee, and the chain ends at the parenthesis.
        let mut paren_chain = None;
        if let ExprKind::Chain(inner) = c.kind
            && matches!(self.expr_kind(inner), ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::PrivateMember { .. })
        {
            c = *prog.expr(inner);
            paren_chain = Some(self.label());
        }
        let direct = !optional && tpl.is_none() && matches!(c.kind, ExprKind::Ident) && self.name_of(c.span) == "eval";
        let spread = direct || args.iter().any(|&a| matches!(self.expr_kind(a), ExprKind::Spread(_)));
        if matches!(c.kind, ExprKind::Super) {
            return self.super_call(e, &args, spread, dst);
        }
        let m = self.mark();
        let tn = tpl.is_some() as u32;
        let n = args.len() as u32 + tn;
        let name = matches!(c.kind, ExprKind::Ident).then(|| self.name_of(c.span));
        let dynamic = name.as_deref().is_some_and(|n| !self.dynamic_hops(n).is_empty());
        let method = dynamic || matches!(c.kind, ExprKind::Member { .. } | ExprKind::Index { .. } | ExprKind::PrivateMember { .. });
        let base = self.window(if spread { 3 } else if method { 2 + n } else { 1 + n });
        let outer_short = paren_chain.and_then(|l| self.chain_short.replace(l));
        match c.kind {
            ExprKind::Member { obj, name, optional: o2 } => {
                let nm = self.name_of(name);
                if matches!(self.expr_kind(obj), ExprKind::Super) {
                    self.this(base + 1)?;
                    self.set_live(base + 1);
                    let s = self.site_of(&nm)?;
                    self.line(c.span.start);
                    self.op(Op::GetSuper, base, base + 1, s);
                } else {
                    self.to(obj, base + 1)?;
                    if o2 {
                        self.short(base + 1);
                    }
                    let s = self.site_of(&nm)?;
                    self.line(c.span.start);
                    self.op(Op::GetField, base, base + 1, s);
                }
                self.set_live(base);
            }
            ExprKind::Index { obj, index, optional: o2 } => {
                if matches!(self.expr_kind(obj), ExprKind::Super) {
                    self.this(base + 1)?;
                    self.set_live(base + 1);
                    let k = self.any(index)?;
                    self.line(c.span.start);
                    self.op(Op::GetSuperElem, base, base + 1, k);
                } else {
                    self.to(obj, base + 1)?;
                    if o2 {
                        self.short(base + 1);
                    }
                    self.line(c.span.start);
                    self.get_index(base, base + 1, index)?;
                }
                self.set_live(base);
            }
            ExprKind::PrivateMember { obj, name, optional: o2 } => {
                self.to(obj, base + 1)?;
                if o2 {
                    self.short(base + 1);
                }
                let k = self.private_key(name)?;
                self.line(c.span.start);
                self.op(Op::GetPrivate, base, base + 1, k);
                self.set_live(base);
                self.free(k);
            }
            ExprKind::Ident if dynamic => {
                let n = name.as_deref().unwrap();
                let t = self.mark();
                let object = self.dynamic_ref(n, Some(base + 1))?;
                self.read_ref(n, c.span.start, base, object)?;
                self.set_live(base);
                self.free(t);
            }
            _ => {
                self.to(callee, base)?;
                if spread {
                    self.op(Op::LoadUndef, base + 1, 0, 0);
                    self.set_live(base + 1);
                }
            }
        }
        if let Some(short) = paren_chain {
            self.chain_short = outer_short;
            let done = self.label();
            self.jmp(done);
            self.bind(short);
            self.op(Op::LoadUndef, base, 0, 0);
            self.op(Op::LoadUndef, base + 1, 0, 0);
            self.bind(done);
            self.set_live(base);
            self.set_live(base + 1);
        }
        if optional {
            self.short(base);
        }
        if spread {
            let arr = base + 2;
            self.op(Op::NewArray, arr, 0, 0);
            self.set_live(arr);
            self.fill_array(arr, &args)?;
            self.line(prog.expr(e).span.start);
            if direct {
                let scope = self.eval_scopes.len() as u32;
                self.eval_scopes.push(self.cur_ctx.clone());
                let new_target = !self.is_script && !self.is_arrow || !matches!(self.resolve("new.target", 0), Res::Global);
                self.opx(Op::EvalCall, dst, base, new_target as u32, scope);
            } else {
                self.op(Op::CallSpread, dst, base, 0);
            }
        } else {
            let first = if method { base + 2 } else { base + 1 };
            if let Some(t) = tpl {
                let (h, site) = self.template_object(t)?;
                self.op(Op::GetTemplate, first, h, site);
                self.set_live(first);
            }
            for (i, a) in args.iter().enumerate() {
                self.to(*a, first + tn + i as u32)?;
            }
            self.line(prog.expr(e).span.start);
            self.op(if method { Op::Call } else { Op::CallU }, dst, base, n);
        }
        self.free(m);
        Ok(())
    }

    /// The recipe of a template (cooked and raw string of each piece) and its site, for `GetTemplate`.
    fn template_object(&mut self, t: Id<Template>) -> CR<(u32, u32)> {
        let prog = self.prog;
        let tm = *prog.template(t);
        let mut keys = Vec::new();
        let mut buf = Vec::new();
        for q in prog.template_quasis(&tm).collect::<Vec<_>>() {
            let qn = *prog.quasi_node(q);
            buf.clear();
            keys.push(if prog.quasi(&qn, false, &mut buf).is_ok() { self.atom_units(&buf)? } else { u32::MAX });
            buf.clear();
            let _ = prog.quasi(&qn, true, &mut buf);
            keys.push(self.atom_units(&buf)?);
        }
        let site = self.site(keys[1]);
        Ok((self.recipes.push(&keys) as u32, site))
    }

    /// `super(...)` in a derived constructor (or an arrow inside it): construct, then the result is `this`.
    fn super_call(&mut self, e: ExprId, args: &[ExprId], spread: bool, dst: u32) -> CR {
        let m = self.mark();
        if spread {
            let arr = self.alloc();
            self.op(Op::NewArray, arr, 0, 0);
            self.set_live(arr);
            self.fill_array(arr, args)?;
            self.line(self.prog.expr(e).span.start);
            self.op(Op::CallSuperSpread, dst, arr, 0);
        } else {
            let n = args.len() as u32;
            let base = self.window(n);
            for (i, a) in args.iter().enumerate() {
                self.to(*a, base + i as u32)?;
            }
            self.line(self.prog.expr(e).span.start);
            self.op(Op::CallSuper, dst, base, n);
        }
        self.set_live(dst);
        // The first `super()` binds `this`. A second call throws a ReferenceError, after the construct.
        let slot = match self.resolve("this", 0) {
            Res::Reg { reg, .. } => Some(reg),
            Res::Ctx { hops, slot, .. } => {
                let t = self.alloc();
                self.op(Op::GetCtx, t, hops, slot);
                self.set_live(t);
                Some(t)
            }
            Res::Global => None,
        };
        if let Some(t) = slot {
            self.op(Op::CheckUnbound, t, 0, 0);
        }
        self.store_var("this", 0, dst, true)?;
        self.free(m);
        Ok(())
    }

    fn new_expr(&mut self, e: ExprId, callee: ExprId, args: List<Expr>, dst: u32) -> CR {
        let prog = self.prog;
        let args: Vec<ExprId> = prog.list(args).collect();
        let m = self.mark();
        let n = args.len() as u32;
        if args.iter().any(|&a| matches!(self.expr_kind(a), ExprKind::Spread(_))) {
            let base = self.window(2);
            self.to(callee, base)?;
            self.op(Op::NewArray, base + 1, 0, 0);
            self.set_live(base + 1);
            self.fill_array(base + 1, &args)?;
            self.line(prog.expr(e).span.start);
            self.op(Op::NewSpread, dst, base, 0);
        } else {
            let base = self.window(1 + n);
            self.to(callee, base)?;
            for (i, a) in args.iter().enumerate() {
                self.to(*a, base + 1 + i as u32)?;
            }
            self.line(prog.expr(e).span.start);
            self.op(Op::New, dst, base, n);
        }
        self.free(m);
        Ok(())
    }

    // ------------------------------------------------------------ assignment

    /// `a = b`, `a += b`, `a &&= b`. `dst`: where the value goes, `None` in effect context.
    pub(crate) fn assign(&mut self, op: AssignOp, target: PatId, value: ExprId, dst: Option<u32>, span: Span) -> CR {
        let prog = self.prog;
        let t = *prog.pat(target);
        let m = self.mark();
        match t.kind {
            PatKind::Ident => {
                let name = self.name_of(t.span);
                self.assign_ident(op, &name, t.span.start, value, dst)?;
            }
            PatKind::Member(me) => self.assign_member(op, me, value, dst, span)?,
            PatKind::Array(_) | PatKind::Object(_) => {
                // Destructuring assignment: the value is in a register that stays.
                let v = match dst {
                    Some(d) if !self.is_var_reg(d) => {
                        self.to(value, d)?;
                        d
                    }
                    _ => {
                        let t = self.alloc();
                        self.to(value, t)?;
                        t
                    }
                };
                self.bind_pattern(target, v, super::pat::Bind::Assign)?;
                if let Some(d) = dst
                    && d != v
                {
                    self.op(Op::Mov, d, v, 0);
                }
            }
            _ => self.unsup("this assignment target"),
        }
        self.free(m);
        Ok(())
    }

    pub(crate) fn assign_ident(&mut self, op: AssignOp, name: &str, at: u32, value: ExprId, dst: Option<u32>) -> CR {
        let object = self.dynamic_ref(name, None)?;
        let res = self.resolve(name, at);
        if matches!(op, AssignOp::Assign | AssignOp::Logical(_)) {
            self.name_hint(value, name);
        }
        match op {
            AssignOp::Assign => {
                if let Res::Reg { reg, tdz: false, konst: false } = res && object.is_none() {
                    // The value goes straight into the variable's register.
                    self.to(value, reg)?;
                    if let Some(d) = dst
                        && d != reg
                    {
                        self.op(Op::Mov, d, reg, 0);
                    }
                } else {
                    let v = match dst {
                        Some(d) => {
                            self.to(value, d)?;
                            d
                        }
                        None => self.any(value)?,
                    };
                    self.set_live(v);
                    self.store_ref(name, at, v, object)?;
                }
            }
            AssignOp::Compound(bop) => {
                // Read the variable first, as the language does: a copy if the right side can assign it.
                let m = self.mark();
                let in_place = object.is_none() && matches!(res, Res::Reg { reg, tdz: false, konst: false } if !self.may_clobber(reg, value));
                let cur = match res {
                    Res::Reg { reg, .. } if in_place => reg,
                    _ => {
                        let cur = self.alloc();
                        self.read_ref(name, at, cur, object)?;
                        self.set_live(cur);
                        cur
                    }
                };
                let target = match res {
                    Res::Reg { reg, tdz: false, konst: false } if object.is_none() => reg,
                    _ => dst.filter(|d| !self.is_var_reg(*d)).unwrap_or(cur),
                };
                self.binary_regs(bop, cur, value, target)?;
                if object.is_some() || !matches!(res, Res::Reg { tdz: false, konst: false, .. }) {
                    // The store is a safepoint and reads `target`, so its root map must name it.
                    self.set_live(target);
                    self.store_ref(name, at, target, object)?;
                }
                if let Some(d) = dst
                    && d != target
                {
                    self.op(Op::Mov, d, target, 0);
                }
                self.free(m);
            }
            AssignOp::Logical(lop) => {
                let m = self.mark();
                let cur = match dst {
                    Some(d) if !self.is_var_reg(d) => d,
                    _ => self.alloc(),
                };
                self.read_ref(name, at, cur, object)?;
                self.set_live(cur);
                let end = self.label();
                self.jump_on_logic(lop, cur, end);
                self.to(value, cur)?;
                self.store_ref(name, at, cur, object)?;
                self.bind(end);
                if let Some(d) = dst
                    && d != cur
                {
                    self.op(Op::Mov, d, cur, 0);
                }
                self.free(m);
            }
        }
        Ok(())
    }

    /// Jump to `end` when the logical operator keeps the left value (`&&`: falsy, `||`: truthy, `??`: not nullish).
    fn jump_on_logic(&mut self, lop: LogicalOp, r: u32, end: Lb) {
        let jop = match lop {
            LogicalOp::And => Op::JmpIfNot,
            LogicalOp::Or => Op::JmpIf,
            LogicalOp::Nullish => Op::JmpNotNullish,
        };
        self.jmp_r(jop, r, end);
    }

    /// `dst = left op right` where `left` is already in a register.
    fn binary_regs(&mut self, op: BinaryOp, left: u32, right: ExprId, dst: u32) -> CR {
        if let Some((rop, Some(iop), _)) = arith(op) {
            let shift = matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr);
            if let Some(v) = self.int_const(right).filter(|v| fits_i16(*v) && (!shift || (0..32).contains(v))) {
                self.op(iop, dst, left, v as u32);
                return Ok(());
            }
            if let (Some(xop), Some(v)) = (xarith(op), self.int_const(right)) {
                self.opx(xop, dst, left, 0, v as u32);
                return Ok(());
            }
            let r = self.any(right)?;
            self.op(rop, dst, left, r);
            return Ok(());
        }
        let rop = arith(op).map(|a| a.0).or_else(|| compare_op(op)).unwrap_or(Op::Add);
        let r = self.any(right)?;
        self.op(rop, dst, left, r);
        Ok(())
    }

    fn assign_member(&mut self, op: AssignOp, me: ExprId, value: ExprId, dst: Option<u32>, span: Span) -> CR {
        let prog = self.prog;
        let x = *prog.expr(me);
        let (obj, name, index, private) = match x.kind {
            ExprKind::Member { obj, name, .. } => (obj, Some(self.name_of(name)), None, None),
            ExprKind::Index { obj, index, .. } => (obj, None, Some(index), None),
            ExprKind::PrivateMember { obj, name, .. } => (obj, None, None, Some(name)),
            _ => {
                self.unsup("this assignment target");
                return Ok(());
            }
        };
        let mode = if matches!(self.expr_kind(obj), ExprKind::Super) { Mode::Super } else if private.is_some() { Mode::Private } else { Mode::Plain };
        // `a["k"]` with a name that is no index is `a.k`.
        let mut name = name;
        let mut index = index;
        if let Some(ix) = index
            && let ExprKind::String = self.expr_kind(ix)
        {
            let mut b = Vec::new();
            if prog.string(prog.expr(ix).span, &mut b).is_ok()
                && let Ok(s) = String::from_utf16(&b)
                && !index_like(&s)
            {
                name = Some(Cow::Owned(s));
                index = None;
            }
        }
        let m = self.mark();
        let o = match (mode, index) {
            (Mode::Super, _) => self.this_reg()?,
            (_, Some(ix)) => self.any_before(obj, ix)?,
            _ => self.any_before(obj, value)?,
        };
        let k = match (private, index) {
            (Some(n), _) => Some(self.private_key(n)?),
            (_, Some(ix)) => Some(match self.key_add(ix)? {
                Some(k) => k,
                None if op == AssignOp::Assign => self.any_before(ix, value)?,
                None => {
                    let k = self.alloc();
                    self.to(ix, k)?;
                    k
                }
            }),
            _ => None,
        };
        self.line(span.start);
        match op {
            AssignOp::Assign => {
                // Effect context with a constant: no register for the value.
                if dst.is_none()
                    && mode == Mode::Plain
                    && let Some(name) = &name
                    && let Some(kc) = self.konst(value)?
                {
                    let s = self.site_of(name)?;
                    match kc {
                        K::Undef => self.op(Op::SetFieldK, o, 0, s),
                        K::Null => self.op(Op::SetFieldK, o, 1, s),
                        K::Bool(b) => self.op(Op::SetFieldK, o, if b { 3 } else { 2 }, s),
                        K::Int(v) if (-32768..=32767).contains(&v) => self.op(Op::SetFieldI, o, v as u32, s),
                        K::Str(a) => self.op(Op::SetFieldA, o, a, s),
                        K::Int(_) => {
                            let t = self.alloc();
                            self.to(value, t)?;
                            self.op(Op::SetField, o, t, s);
                        }
                    }
                } else {
                    // The value must not go into a register that holds the object or the key; a variable is read in place.
                    let v = match dst {
                        Some(d) if !self.is_var_reg(d) => {
                            self.to(value, d)?;
                            d
                        }
                        _ => self.any(value)?,
                    };
                    self.set_member(mode, o, name.as_deref(), k, v)?;
                    if let Some(d) = dst
                        && d != v
                    {
                        self.op(Op::Mov, d, v, 0);
                    }
                }
            }
            AssignOp::Compound(bop) => {
                let cur = self.alloc();
                self.get_member(mode, cur, o, name.as_deref(), k)?;
                self.set_live(cur);
                let res = match dst {
                    Some(d) if !self.is_var_reg(d) => d,
                    _ => cur,
                };
                self.binary_regs(bop, cur, value, res)?;
                self.set_member(mode, o, name.as_deref(), k, res)?;
                if let Some(d) = dst
                    && d != res
                {
                    self.op(Op::Mov, d, res, 0);
                }
            }
            AssignOp::Logical(lop) => {
                let cur = match dst {
                    Some(d) if !self.is_var_reg(d) => d,
                    _ => self.alloc(),
                };
                self.get_member(mode, cur, o, name.as_deref(), k)?;
                self.set_live(cur);
                let end = self.label();
                self.jump_on_logic(lop, cur, end);
                self.to(value, cur)?;
                self.set_member(mode, o, name.as_deref(), k, cur)?;
                self.bind(end);
                if let Some(d) = dst
                    && d != cur
                {
                    self.op(Op::Mov, d, cur, 0);
                }
            }
        }
        self.free(m);
        Ok(())
    }

    pub(crate) fn get_member(&mut self, mode: Mode, dst: u32, o: u32, name: Option<&str>, k: Option<u32>) -> CR {
        match (mode, name, k) {
            (Mode::Plain, Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::GetField, dst, o, s);
            }
            (Mode::Plain, None, Some(k)) => self.op(Op::GetElemRef, dst, o, k),
            (Mode::Super, Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::GetSuper, dst, o, s);
            }
            (Mode::Super, None, Some(k)) => self.op(Op::GetSuperElemRef, dst, o, k),
            (Mode::Private, _, Some(k)) => self.op(Op::GetPrivate, dst, o, k),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn set_member(&mut self, mode: Mode, o: u32, name: Option<&str>, k: Option<u32>, v: u32) -> CR {
        match (mode, name, k) {
            (Mode::Plain, Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::SetField, o, v, s);
            }
            (Mode::Plain, None, Some(k)) => self.op(Op::SetElem, o, k, v),
            (Mode::Super, Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::SetSuper, o, v, s);
            }
            (Mode::Super, None, Some(k)) => self.op(Op::SetSuperElem, o, k, v),
            (Mode::Private, _, Some(k)) => self.op(Op::SetPrivate, o, k, v),
            _ => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------ update

    /// `++a`, `a--`. `dst`: the value of the expression, `None` in effect context.
    pub(crate) fn update(&mut self, op: UpdateOp, prefix: bool, arg: ExprId, dst: Option<u32>) -> CR {
        let prog = self.prog;
        let iop = if op == UpdateOp::Inc { Op::Inc } else { Op::Dec };
        let x = *prog.expr(arg);
        let m = self.mark();
        match x.kind {
            ExprKind::Ident => {
                let name = self.name_of(x.span);
                let object = self.dynamic_ref(&name, None)?;
                match self.resolve(&name, x.span.start) {
                    Res::Reg { reg, tdz: false, konst: false } if object.is_none() => match dst {
                        None => self.op(iop, reg, reg, 0),
                        Some(d) if prefix => {
                            self.op(iop, reg, reg, 0);
                            if d != reg {
                                self.op(Op::Mov, d, reg, 0);
                            }
                        }
                        Some(d) => {
                            // The value is the old one, after ToNumeric.
                            let old = if self.is_var_reg(d) { self.alloc() } else { d };
                            self.op(Op::ToNumeric, old, reg, 0);
                            self.op(iop, reg, old, 0);
                            if old != d {
                                self.op(Op::Mov, d, old, 0);
                            }
                        }
                    },
                    _ => {
                        let cur = self.alloc();
                        self.read_ref(&name, x.span.start, cur, object)?;
                        self.set_live(cur);
                        self.update_value(iop, prefix, cur, dst);
                        self.store_ref(&name, x.span.start, if prefix || dst.is_none() { cur } else { cur + 1 }, object)?;
                        if let Some(d) = dst {
                            self.op(Op::Mov, d, cur, 0);
                        }
                    }
                }
            }
            ExprKind::Member { obj, name, .. } => {
                let nm = self.name_of(name);
                let (o, mode) = self.member_base(obj, None)?;
                self.member_update(mode, iop, prefix, o, (Some(&nm), None), dst)?;
            }
            ExprKind::Index { obj, index, .. } => {
                let (o, mode) = self.member_base(obj, Some(index))?;
                let k = self.alloc();
                self.to(index, k)?;
                self.member_update(mode, iop, prefix, o, (None, Some(k)), dst)?;
            }
            ExprKind::PrivateMember { obj, name, .. } => {
                let o = self.any(obj)?;
                let k = self.private_key(name)?;
                self.member_update(Mode::Private, iop, prefix, o, (None, Some(k)), dst)?;
            }
            _ => self.unsup("this update target"),
        }
        self.free(m);
        Ok(())
    }

    /// With the current value in `cur`: the new value is `cur` (prefix or effect). Otherwise the new value is in `cur + 1` and the old
    /// one, converted, in `cur`.
    fn update_value(&mut self, iop: Op, prefix: bool, cur: u32, dst: Option<u32>) {
        if prefix || dst.is_none() {
            self.op(iop, cur, cur, 0);
        } else {
            let new = self.alloc();
            debug_assert_eq!(new, cur + 1);
            self.op(Op::ToNumeric, cur, cur, 0);
            self.op(iop, new, cur, 0);
            self.set_live(new);
        }
    }

    /// The register of the object of a member target, and how the member is read: `super.x` takes `this`.
    pub(crate) fn member_base(&mut self, obj: ExprId, before: Option<ExprId>) -> CR<(u32, Mode)> {
        if matches!(self.expr_kind(obj), ExprKind::Super) {
            return Ok((self.this_reg()?, Mode::Super));
        }
        Ok((if let Some(ix) = before { self.any_before(obj, ix)? } else { self.any(obj)? }, Mode::Plain))
    }

    fn member_update(&mut self, mode: Mode, iop: Op, prefix: bool, o: u32, (name, k): (Option<&str>, Option<u32>), dst: Option<u32>) -> CR {
        let cur = self.alloc();
        self.get_member(mode, cur, o, name, k)?;
        self.set_live(cur);
        self.update_value(iop, prefix, cur, dst);
        let new = if prefix || dst.is_none() { cur } else { cur + 1 };
        self.set_member(mode, o, name, k, new)?;
        if let Some(d) = dst {
            self.op(Op::Mov, d, if prefix { new } else { cur }, 0);
        }
        Ok(())
    }

    // ------------------------------------------------------------ effect context

    fn foreach_step(&mut self, args: List<Expr>, at: u32) -> CR {
        let mut args = self.prog.list(args);
        let (Some(callback), Some(recv), Some(index), None) = (args.next(), args.next(), args.next(), args.next()) else {
            unreachable!("forEachStep takes a callback, a receiver, and an index")
        };
        let base = self.window(4);
        self.to(callback, base)?;
        let recv = self.any_before(recv, index)?;
        let index = self.any(index)?;
        let skip = self.label();
        self.line(at);
        self.jump_ins(Ins::new(Op::ForEachStep, base + 1, recv, index), skip);
        for r in base + 1..base + 4 {
            self.set_live(r);
        }
        self.op(Op::CallU, base, base, 3);
        self.bind(skip);
        Ok(())
    }

    /// Evaluate `e` for its effects only.
    pub(crate) fn effect(&mut self, e: ExprId) -> CR {
        let x = *self.prog.expr(e);
        let m = self.mark();
        match x.kind {
            ExprKind::Call { callee, args, optional: false }
                if self.pre
                    && matches!(self.expr_kind(callee), ExprKind::Ident)
                    && self.name_of(self.prog.expr(callee).span) == crate::prelude_pre::FOR_EACH_STEP
                    && matches!(self.resolve(crate::prelude_pre::FOR_EACH_STEP, x.span.start), Res::Global) =>
            {
                self.foreach_step(args, x.span.start)?;
            }
            ExprKind::Assign { op, target, value } => self.assign(op, target, value, None, x.span)?,
            ExprKind::Update { op, prefix, arg } => self.update(op, prefix, arg, None)?,
            ExprKind::Seq(list) => {
                let items: Vec<ExprId> = self.prog.list(list).collect();
                for it in items {
                    self.effect(it)?;
                }
            }
            ExprKind::Logical { op, left, right } => {
                // `a && b()` runs `b()` only when `a` is truthy: a test and a jump, no value.
                let end = self.label();
                match op {
                    LogicalOp::And => self.cond(left, false, end)?,
                    LogicalOp::Or => self.cond(left, true, end)?,
                    LogicalOp::Nullish => {
                        let r = self.any(left)?;
                        self.jmp_r(Op::JmpNotNullish, r, end);
                    }
                }
                self.effect(right)?;
                self.bind(end);
            }
            ExprKind::Cond { test, cons, alt } => {
                let (els, end) = (self.label(), self.label());
                self.cond(test, false, els)?;
                self.effect(cons)?;
                self.jmp(end);
                self.bind(els);
                self.effect(alt)?;
                self.bind(end);
            }
            ExprKind::Unary { op: UnaryOp::Void, arg } => self.effect(arg)?,
            ExprKind::Number(_) | ExprKind::String | ExprKind::Null | ExprKind::Bool(_) | ExprKind::This | ExprKind::Function(_) => {}
            ExprKind::Ident => {
                // A read can throw (a global that does not exist, a dead zone), so it stays; a variable that cannot throw does not.
                let name = self.name_of(x.span);
                match self.resolve(&name, x.span.start) {
                    Res::Reg { tdz: false, .. } | Res::Ctx { tdz: false, .. } if self.dynamic_hops(&name).is_empty() => {}
                    _ => {
                        let t = self.alloc();
                        self.read_var(&name, x.span.start, t)?;
                    }
                }
            }
            _ => {
                let t = self.alloc();
                self.to(e, t)?;
            }
        }
        self.free(m);
        Ok(())
    }

    // ------------------------------------------------------------ test context

    /// Jump to `to` when the truth of `e` is `jump_if`; otherwise go on.
    pub(crate) fn cond(&mut self, e: ExprId, jump_if: bool, to: Lb) -> CR {
        let x = *self.prog.expr(e);
        match x.kind {
            ExprKind::Unary { op: UnaryOp::Not, arg } => return self.cond(arg, !jump_if, to),
            ExprKind::Bool(b) => {
                if b == jump_if {
                    self.jmp(to);
                }
                return Ok(());
            }
            ExprKind::Logical { op: LogicalOp::And, left, right } => {
                if jump_if {
                    let skip = self.label();
                    self.cond(left, false, skip)?;
                    self.cond(right, true, to)?;
                    self.bind(skip);
                } else {
                    self.cond(left, false, to)?;
                    self.cond(right, false, to)?;
                }
                return Ok(());
            }
            ExprKind::Logical { op: LogicalOp::Or, left, right } => {
                if jump_if {
                    self.cond(left, true, to)?;
                    self.cond(right, true, to)?;
                } else {
                    let skip = self.label();
                    self.cond(left, true, skip)?;
                    self.cond(right, false, to)?;
                    self.bind(skip);
                }
                return Ok(());
            }
            ExprKind::Seq(list) => {
                let items: Vec<ExprId> = self.prog.list(list).collect();
                for (i, it) in items.iter().enumerate() {
                    if i + 1 == items.len() {
                        return self.cond(*it, jump_if, to);
                    }
                    self.effect(*it)?;
                }
            }
            ExprKind::Binary { op, left, right } if self.compare_jump(op, left, right, jump_if, to)? => return Ok(()),
            _ => {}
        }
        let m = self.mark();
        let r = self.any(e)?;
        self.jmp_r(if jump_if { Op::JmpIf } else { Op::JmpIfNot }, r, to);
        self.free(m);
        Ok(())
    }

    /// A compare of two values with a jump. `false` when `op` is no comparison (nothing is emitted).
    fn compare_jump(&mut self, op: BinaryOp, left: ExprId, right: ExprId, jump_if: bool, to: Lb) -> CR<bool> {
        use BinaryOp::*;
        if !matches!(op, Eq | Ne | StrictEq | StrictNe | Lt | Le | Gt | Ge) {
            return Ok(false);
        }
        let m = self.mark();
        // Which side is a constant? A constant on the left is moved to the right and the operator is mirrored.
        let (mut op, mut l, mut r) = (op, left, right);
        let kr = self.konst(r)?;
        let kl = if kr.is_none() { self.konst(l)? } else { None };
        let konst = if kr.is_some() {
            kr
        } else if kl.is_some() {
            std::mem::swap(&mut l, &mut r);
            op = match op {
                Lt => Gt,
                Le => Ge,
                Gt => Lt,
                Ge => Le,
                o => o,
            };
            kl
        } else {
            None
        };
        // Polarity: the equalities have one op for each, the order tests need the `Jn` forms for "not".
        let eq = matches!(op, Eq | StrictEq | Ne | StrictNe);
        let strict = matches!(op, StrictEq | StrictNe);
        let positive = matches!(op, Eq | StrictEq | Lt | Le | Gt | Ge);
        let want_eq = positive == jump_if; // for the equalities: jump when equal
        let a = self.any_before(l, r)?;
        let done = |c: &mut Self| {
            c.free(m);
            Ok(true)
        };
        if let Some(k) = konst {
            match (k, eq) {
                (K::Null, true) if strict => {
                    self.jmp_r(if want_eq { Op::JSEqNull } else { Op::JSNeNull }, a, to);
                    return done(self);
                }
                (K::Undef, true) if strict => {
                    self.jmp_r(if want_eq { Op::JSEqUndef } else { Op::JSNeUndef }, a, to);
                    return done(self);
                }
                // `x == null` is true for `null` and `undefined`.
                (K::Null | K::Undef, true) => {
                    self.jmp_r(if want_eq { Op::JmpNullish } else { Op::JmpNotNullish }, a, to);
                    return done(self);
                }
                (K::Bool(b), true) if strict => {
                    self.jmp_rx(if want_eq { Op::JSEqB } else { Op::JSNeB }, a, b as u32, to);
                    return done(self);
                }
                (K::Str(at), true) if strict => {
                    self.jmp_rx(if want_eq { Op::JSEqA } else { Op::JSNeA }, a, at, to);
                    return done(self);
                }
                (K::Int(v), true) => {
                    let o = match (strict, want_eq) {
                        (true, true) => Op::JSEqI,
                        (true, false) => Op::JSNeI,
                        (false, true) => Op::JEqI,
                        (false, false) => Op::JNeI,
                    };
                    self.jmp_rx(o, a, v as u32, to);
                    return done(self);
                }
                (K::Int(v), false) => {
                    let o = match (op, jump_if) {
                        (Lt, true) => Op::JLtI,
                        (Le, true) => Op::JLeI,
                        (Gt, true) => Op::JGtI,
                        (Ge, true) => Op::JGeI,
                        (Lt, false) => Op::JnLtI,
                        (Le, false) => Op::JnLeI,
                        (Gt, false) => Op::JnGtI,
                        _ => Op::JnGeI,
                    };
                    self.jmp_rx(o, a, v as u32, to);
                    return done(self);
                }
                _ => {}
            }
        }
        // Two registers (a constant that has no form goes into a register).
        let b = self.any(r)?;
        let o = if eq {
            match (strict, want_eq) {
                (true, true) => Op::JSEq,
                (true, false) => Op::JSNe,
                (false, true) => Op::JEq,
                (false, false) => Op::JNe,
            }
        } else {
            match (op, jump_if) {
                (Lt, true) => Op::JLt,
                (Le, true) => Op::JLe,
                (Gt, true) => Op::JGt,
                (Ge, true) => Op::JGe,
                (Lt, false) => Op::JnLt,
                (Le, false) => Op::JnLe,
                (Gt, false) => Op::JnGt,
                _ => Op::JnGe,
            }
        };
        self.jmp_rr(o, a, b, to);
        done(self)
    }
}



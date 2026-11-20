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

    /// Write the value of variable `name` (used at offset `at`) into `dst`.
    pub(crate) fn read_var(&mut self, name: &str, at: u32, dst: u32) -> CR {
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
                "arguments" if !self.is_script => {
                    self.unsup("the arguments object");
                    self.op(Op::LoadUndef, dst, 0, 0);
                }
                _ => {
                    let s = self.site_of(name)?;
                    self.op(Op::GetGlobal, dst, s, 0);
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
        match self.resolve(name, at) {
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
            Res::Ctx { hops, slot, tdz, konst } => {
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
                let s = self.site_of(name)?;
                self.op(Op::SetGlobal, src, s, 0);
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
            if let Res::Reg { reg, tdz, .. } = self.resolve(&name, x.span.start) {
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
                let o = self.any(obj)?;
                if optional {
                    self.short(o);
                }
                let s = self.site_of(&nm)?;
                self.line(x.span.start);
                self.op(Op::GetField, dst, o, s);
                self.free(m);
            }
            ExprKind::Index { obj, index, optional } => {
                let m = self.mark();
                let o = self.any_before(obj, index)?;
                if optional {
                    self.short(o);
                }
                self.line(x.span.start);
                self.get_index(dst, o, index)?;
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
            ExprKind::BigInt => self.not_built(dst, "a BigInt literal"),
            ExprKind::RegExp => self.not_built(dst, "a regexp literal"),
            ExprKind::Tagged { .. } => self.not_built(dst, "a tagged template"),
            ExprKind::Class(_) => self.not_built(dst, "a class"),
            ExprKind::PrivateMember { .. } | ExprKind::PrivateIn { .. } => self.not_built(dst, "a private name"),
            ExprKind::Super => self.not_built(dst, "super"),
            ExprKind::NewTarget => self.not_built(dst, "new.target"),
            ExprKind::Await(_) => self.not_built(dst, "await"),
            ExprKind::Yield { .. } => self.not_built(dst, "yield"),
            ExprKind::Spread(_) => self.not_built(dst, "spread"),
            ExprKind::Hole => self.op(Op::LoadUndef, dst, 0, 0),
            ExprKind::ImportCall { .. } => self.not_built(dst, "import()"),
            ExprKind::ImportMeta => self.not_built(dst, "import.meta"),
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

    fn this(&mut self, dst: u32) -> CR {
        if self.is_arrow
            && let Res::Ctx { hops, slot, .. } = self.resolve("this", u32::MAX)
        {
            self.op(Op::GetCtx, dst, hops, slot);
        } else {
            self.op(Op::LoadThis, dst, 0, 0);
        }
        Ok(())
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
        let k = self.any(index)?;
        self.op(Op::GetElem, dst, o, k);
        Ok(())
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
        if items.iter().any(|&i| matches!(self.expr_kind(i), ExprKind::Spread(_) | ExprKind::Hole)) {
            self.unsup("a spread or a hole in an array literal");
        }
        let m = self.mark();
        let d = self.scratch(dst, &items);
        if items.is_empty() {
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
    fn static_key(&self, key: Key) -> Option<Cow<'a, str>> {
        let prog = self.prog;
        match key {
            Key::Name(s) => Some(prog.name(s)),
            Key::Str(s) => {
                let mut b = Vec::new();
                prog.string(s, &mut b).ok()?;
                Some(Cow::Owned(String::from_utf16(&b).ok()?))
            }
            _ => None,
        }
    }

    fn object(&mut self, list: List<Prop>, dst: u32) -> CR {
        let prog = self.prog;
        let props: Vec<Prop> = prog.list(list).map(|p| *prog.prop(p)).collect();
        let mut names: Vec<Cow<'a, str>> = Vec::new();
        let mut simple = props.len() as u32 <= LIT_MAX;
        for p in &props {
            let ok_kind = matches!(p.kind, PropKind::Init | PropKind::Shorthand | PropKind::Method);
            match self.static_key(p.key) {
                Some(n) if ok_kind && !(index_like(&n) || (n == "__proto__" && p.kind == PropKind::Init) || names.contains(&n)) => names.push(n),
                _ => simple = false,
            }
        }
        let m = self.mark();
        let values: Vec<ExprId> = props.iter().map(|p| p.value).collect();
        let d = self.scratch(dst, &values);
        if props.is_empty() {
            self.op(Op::NewObject, d, 0, 0);
        } else if simple {
            let base = self.window(props.len() as u32);
            let mut keys = Vec::with_capacity(props.len());
            for (i, p) in props.iter().enumerate() {
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
                    PropKind::Spread => self.unsup("spread in an object literal"),
                    PropKind::Get | PropKind::Set => self.unsup("a getter or a setter in an object literal"),
                    _ => {
                        if matches!(p.kind, PropKind::Init) && self.static_key(p.key).is_some_and(|n| n == "__proto__") {
                            self.unsup("__proto__ in an object literal");
                        }
                        match (self.static_key(p.key), p.key) {
                            (Some(n), _) if !index_like(&n) => {
                                let v = self.any(p.value)?;
                                let s = self.site_of(&n)?;
                                self.op(Op::DefField, d, v, s);
                            }
                            (_, Key::Computed(k)) => {
                                let kv = self.any(k)?;
                                let kt = self.alloc();
                                self.op(Op::ToKey, kt, d, kv);
                                self.set_live(kt);
                                let v = self.any(p.value)?;
                                self.op(Op::DefElem, d, kt, v);
                            }
                            (n, key) => {
                                // An index key: a number or a string that is an index.
                                let kt = self.alloc();
                                match (n, key) {
                                    (Some(n), _) => self.load_number(kt, n.parse::<f64>().unwrap_or(0.0)),
                                    (_, Key::Num(s)) => {
                                        let v = prog.number(s, crate::lex::NumberBase::Decimal).unwrap_or(0.0);
                                        self.load_number(kt, v);
                                    }
                                    _ => {
                                        self.unsup("a BigInt or private key in an object literal");
                                        self.op(Op::LoadUndef, kt, 0, 0);
                                    }
                                }
                                self.set_live(kt);
                                let v = self.any(p.value)?;
                                self.op(Op::DefElem, d, kt, v);
                            }
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

    /// A function value: a closure over the current context. `expr`: a function expression (a name that it uses is its own binding).
    pub(crate) fn function(&mut self, f: FuncId, dst: u32, expr: bool) -> CR {
        let func = *self.prog.func(f);
        if func.is_async() {
            self.unsup("an async function");
        }
        if func.is_generator() {
            self.unsup("a generator");
        }
        if expr
            && let (Some(name), Body::Skipped(id)) = (func.name, func.body)
            && func.kind == FuncKind::Normal
        {
            let ff = self.facts.get(id);
            let n = self.name_of(name);
            if self.may_occur(&n, ff.parse_at, ff.end) {
                self.unsup("a function expression that may use its own name");
            }
        }
        let i = self.child(f)?;
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
                    Some(n) if matches!(self.resolve(&n, a.span.start), Res::Global) && n != "undefined" => {
                        let s = self.site_of(&n)?;
                        self.op(Op::TypeofGlobal, dst, s, 0);
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
                if matches!(self.resolve(&n, a.span.start), Res::Global) {
                    self.unsup("delete of a global name");
                }
                self.op(Op::LoadFalse, dst, 0, 0);
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
        let prog = self.prog;
        let args: Vec<ExprId> = prog.list(args).collect();
        let c = *prog.expr(callee);
        if args.iter().any(|&a| matches!(self.expr_kind(a), ExprKind::Spread(_))) {
            self.unsup("spread in a call");
        }
        if matches!(c.kind, ExprKind::Super) {
            self.unsup("a super call");
        }
        let m = self.mark();
        let n = args.len() as u32;
        let (base, first) = match c.kind {
            ExprKind::Member { obj, name, optional: o2 } => {
                let nm = self.name_of(name);
                let base = self.window(2 + n);
                self.to(obj, base + 1)?;
                if o2 {
                    self.short(base + 1);
                }
                let s = self.site_of(&nm)?;
                self.line(c.span.start);
                self.op(Op::GetField, base, base + 1, s);
                self.set_live(base);
                (base, base + 2)
            }
            ExprKind::Index { obj, index, optional: o2 } => {
                let base = self.window(2 + n);
                self.to(obj, base + 1)?;
                if o2 {
                    self.short(base + 1);
                }
                self.line(c.span.start);
                self.get_index(base, base + 1, index)?;
                self.set_live(base);
                (base, base + 2)
            }
            _ => {
                let base = self.window(1 + n);
                self.to(callee, base)?;
                (base, base + 1)
            }
        };
        let method = first == base + 2;
        if optional {
            self.short(base);
        }
        for (i, a) in args.iter().enumerate() {
            self.to(*a, first + i as u32)?;
        }
        self.line(prog.expr(e).span.start);
        self.op(if method { Op::Call } else { Op::CallU }, dst, base, n);
        self.free(m);
        Ok(())
    }

    fn new_expr(&mut self, e: ExprId, callee: ExprId, args: List<Expr>, dst: u32) -> CR {
        let prog = self.prog;
        let args: Vec<ExprId> = prog.list(args).collect();
        if args.iter().any(|&a| matches!(self.expr_kind(a), ExprKind::Spread(_))) {
            self.unsup("spread in a call");
        }
        let m = self.mark();
        let n = args.len() as u32;
        let base = self.window(1 + n);
        self.to(callee, base)?;
        for (i, a) in args.iter().enumerate() {
            self.to(*a, base + 1 + i as u32)?;
        }
        self.line(prog.expr(e).span.start);
        self.op(Op::New, dst, base, n);
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

    fn assign_ident(&mut self, op: AssignOp, name: &str, at: u32, value: ExprId, dst: Option<u32>) -> CR {
        let res = self.resolve(name, at);
        match op {
            AssignOp::Assign => {
                if let Res::Reg { reg, tdz: false, konst: false } = res {
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
                    self.store_var(name, at, v, false)?;
                }
            }
            AssignOp::Compound(bop) => {
                // Read the variable first, as the language does: a copy if the right side can assign it.
                let m = self.mark();
                let in_place = matches!(res, Res::Reg { reg, tdz: false, konst: false } if !self.may_clobber(reg, value));
                let cur = match res {
                    Res::Reg { reg, .. } if in_place => reg,
                    _ => {
                        let cur = self.alloc();
                        self.read_var(name, at, cur)?;
                        self.set_live(cur);
                        cur
                    }
                };
                let target = match res {
                    Res::Reg { reg, tdz: false, konst: false } => reg,
                    _ => dst.filter(|d| !self.is_var_reg(*d)).unwrap_or(cur),
                };
                self.binary_regs(bop, cur, value, target)?;
                if !matches!(res, Res::Reg { tdz: false, konst: false, .. }) {
                    self.store_var(name, at, target, false)?;
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
                self.read_var(name, at, cur)?;
                self.set_live(cur);
                let end = self.label();
                self.jump_on_logic(lop, cur, end);
                self.to(value, cur)?;
                self.store_var(name, at, cur, false)?;
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
        let (obj, name, index) = match x.kind {
            ExprKind::Member { obj, name, .. } => (obj, Some(self.name_of(name)), None),
            ExprKind::Index { obj, index, .. } => (obj, None, Some(index)),
            _ => {
                self.unsup("this assignment target");
                return Ok(());
            }
        };
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
        let o = match index {
            Some(ix) => self.any_before(obj, ix)?,
            None => self.any_before(obj, value)?,
        };
        let k = match index {
            Some(ix) => Some(self.any_before(ix, value)?),
            None => None,
        };
        self.line(span.start);
        match op {
            AssignOp::Assign => {
                // Effect context with a constant: no register for the value.
                if dst.is_none()
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
                    self.set_member(o, name.as_deref(), k, v)?;
                    if let Some(d) = dst
                        && d != v
                    {
                        self.op(Op::Mov, d, v, 0);
                    }
                }
            }
            AssignOp::Compound(bop) => {
                let cur = self.alloc();
                self.get_member(cur, o, name.as_deref(), k)?;
                self.set_live(cur);
                let res = match dst {
                    Some(d) if !self.is_var_reg(d) => d,
                    _ => cur,
                };
                self.binary_regs(bop, cur, value, res)?;
                self.set_member(o, name.as_deref(), k, res)?;
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
                self.get_member(cur, o, name.as_deref(), k)?;
                self.set_live(cur);
                let end = self.label();
                self.jump_on_logic(lop, cur, end);
                self.to(value, cur)?;
                self.set_member(o, name.as_deref(), k, cur)?;
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

    pub(crate) fn get_member(&mut self, dst: u32, o: u32, name: Option<&str>, k: Option<u32>) -> CR {
        match (name, k) {
            (Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::GetField, dst, o, s);
            }
            (None, Some(k)) => self.op(Op::GetElem, dst, o, k),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn set_member(&mut self, o: u32, name: Option<&str>, k: Option<u32>, v: u32) -> CR {
        match (name, k) {
            (Some(n), _) => {
                let s = self.site_of(n)?;
                self.op(Op::SetField, o, v, s);
            }
            (None, Some(k)) => self.op(Op::SetElem, o, k, v),
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
                match self.resolve(&name, x.span.start) {
                    Res::Reg { reg, tdz: false, konst: false } => match dst {
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
                        self.read_var(&name, x.span.start, cur)?;
                        self.set_live(cur);
                        self.update_value(iop, prefix, cur, dst);
                        self.store_var(&name, x.span.start, if prefix || dst.is_none() { cur } else { cur + 1 }, false)?;
                        if let Some(d) = dst {
                            self.op(Op::Mov, d, cur, 0);
                        }
                    }
                }
            }
            ExprKind::Member { obj, name, .. } => {
                let nm = self.name_of(name);
                let o = self.any(obj)?;
                self.member_update(iop, prefix, o, Some(&nm), None, dst)?;
            }
            ExprKind::Index { obj, index, .. } => {
                let o = self.any_before(obj, index)?;
                let k = self.any(index)?;
                self.member_update(iop, prefix, o, None, Some(k), dst)?;
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

    fn member_update(&mut self, iop: Op, prefix: bool, o: u32, name: Option<&str>, k: Option<u32>, dst: Option<u32>) -> CR {
        let cur = self.alloc();
        self.get_member(cur, o, name, k)?;
        self.set_live(cur);
        self.update_value(iop, prefix, cur, dst);
        let new = if prefix || dst.is_none() { cur } else { cur + 1 };
        self.set_member(o, name, k, new)?;
        if let Some(d) = dst {
            self.op(Op::Mov, d, if prefix { new } else { cur }, 0);
        }
        Ok(())
    }

    // ------------------------------------------------------------ effect context

    /// Evaluate `e` for its effects only.
    pub(crate) fn effect(&mut self, e: ExprId) -> CR {
        let x = *self.prog.expr(e);
        let m = self.mark();
        match x.kind {
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
                    Res::Reg { tdz: false, .. } | Res::Ctx { tdz: false, .. } => {}
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



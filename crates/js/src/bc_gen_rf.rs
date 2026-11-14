//! Stage 2 of the code generator (`docs/design-js.md`, P3): a function with no inner function, class, `with` or `arguments` keeps
//! all its variables in registers, and its code has no environment ops. Its test and jump are one op (`JnLt` and the others),
//! `i++` is one op (`Inc`), a number literal is an operand (`AddK`, `JnLtK`), and a local variable is its own operand (no `Mov`).
//!
//! Nothing outside the function can read or write a register, so an operand that is a local is safe to read late, except when the
//! code that runs between the read and the use assigns the variable (`pure` says it does not).
use super::bc_scope::{BindKind, FnScope, BF_ARGS, BF_DERIVED, BF_INNER, BF_IN_CLASS, BF_NEW_TARGET, BF_SELF_NAME};
use super::*;

impl<'p> Interp<'p> {
    /// The register layout of function `fid` if it can be a register function: the registers of its parameters, `var`s and
    /// top level `let`s and `const`s, in the order of `bc_binds` (so the parameters are registers `0..nparams`).
    pub(super) fn rf_start(&mut self, fid: FuncId, f: &Func, fs: &FnScope) -> Option<(Rf, usize, usize)> {
        let ok = f.mode == FuncMode::Plain
            && matches!(f.kind, FuncKind::Normal | FuncKind::Arrow | FuncKind::Method)
            && f.simple_params
            && !f.is_async()
            && !f.is_generator()
            && self.cc.cold.fflags[fid.index()] == 0
            && fs.flags & (BF_ARGS | BF_NEW_TARGET | BF_DERIVED | BF_IN_CLASS | BF_SELF_NAME) == 0;
        if !ok {
            return None;
        }
        // Inner functions: the variables that they name stay in the environment (see `rfc_analyze`).
        let (captured, env_scopes) = if fs.flags & BF_INNER != 0 { self.rfc_analyze(fid, fs)? } else { Default::default() };
        let mut binds = Vec::new();
        self.bc_binds(fs.scope, &mut binds);
        let nparams = f.params.len();
        // A repeated parameter name has one binding: the registers would not match the arguments.
        if binds.len() > 200 || binds.iter().filter(|b| b.kind == BindKind::Param).count() != nparams || !binds[..nparams].iter().all(|b| b.kind == BindKind::Param) {
            return None;
        }
        let locals = binds
            .iter()
            .enumerate()
            .filter(|(_, b)| !captured.contains(&(fs.scope, b.name)))
            .map(|(i, b)| ((fs.scope, b.name), Local { reg: i as u32, init: b.flags & INIT != 0, is_const: b.kind == BindKind::Const }))
            .collect();
        let env = fs.flags & BF_INNER != 0;
        Some((Rf { scope: fs.scope, outer: self.sc_all[fs.scope as usize].parent, locals, uses_this: false, env, env_scopes, captured }, nparams, binds.len()))
    }

    /// What the inner functions of function `fid` take from it. Gives the variables that an inner function names, as pairs of the
    /// declaring scope and the name (an inner function that names one reads or writes it by (hops, index) in an environment, in the
    /// bytecode lane and on the tree walker alike, so the variable must be a binding there), and the scopes of `fid` that have an
    /// inner function inside them (each of them keeps its environment, so the hop count of a site in the inner function stays that
    /// of `bc_resolve`). `None` when the walk of an inner function fails.
    fn rfc_analyze(&mut self, fid: FuncId, fs: &FnScope) -> Option<(std::collections::HashSet<(u32, u32)>, std::collections::HashSet<u32>)> {
        let w = self.bc_walk(Some(fid), fs.scope);
        if w.bad.is_some() {
            return None;
        }
        let own: std::collections::HashSet<u32> = std::iter::once(fs.scope).chain(w.stmts.iter().map(|&(_, s)| s)).collect();
        let (mut captured, mut env_scopes) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for &(_, isc) in &w.funcs {
            let mut p = self.sc_all[isc as usize].parent;
            while p != NONE && p != fs.scope {
                env_scopes.insert(p);
                p = self.sc_all[p as usize].parent;
            }
        }
        let nexprs = self.prog.exprs.len() as u32;
        let mut todo = w.funcs;
        while let Some((f, isc)) = todo.pop() {
            let iw = self.bc_walk(Some(f), isc);
            if iw.bad.is_some() {
                return None;
            }
            todo.extend(iw.funcs);
            for (site, s) in iw.sites {
                let n = if site < nexprs { self.cc.expr_name[site as usize] } else { self.cc.cold.pat_name[(site - nexprs) as usize] };
                let mut t = s;
                while t != NONE {
                    if self.bc_declares(t, n) {
                        if own.contains(&t) {
                            captured.insert((t, n));
                        }
                        break;
                    }
                    t = self.sc_all[t as usize].parent;
                }
            }
        }
        Some((captured, env_scopes))
    }
}

impl Gen<'_, '_> {
    /// Whether a call here is `CallR`, which runs a register function of the same script in the loop of the caller. Every function
    /// uses it, except in the stage 1 listings (`rf_off`).
    pub(super) fn callr_ok(&self) -> bool {
        !self.it.bc.rf_off
    }

    /// Whether scope `sc` of the function has an environment at run time. Every scope of a function of stage 1 has one.
    pub(super) fn scope_has_env(&self, sc: u32) -> bool {
        self.rf.as_ref().is_none_or(|r| r.env_scopes.contains(&sc))
    }

    /// What the name `n` means at a site in scope `sc`, in a register function that has an environment, when no register holds it:
    /// a binding of an environment of this function, or a name of the scopes around it.
    pub(super) fn rfc_site(&mut self, mut sc: u32, n: u32) -> Site {
        let (fscope, outer) = {
            let rf = self.rf.as_ref().expect("a register function");
            (rf.scope, rf.outer)
        };
        let mut hops = 0;
        loop {
            if self.it.bc_declares(sc, n) {
                return match self.it.bc_tmpl(sc).iter().position(|b| b.0 == n) {
                    Some(i) if hops < 0x3FFF && i < 0xFFFF => Site::Env { hops, index: i as u32 },
                    _ => Site::Dynamic,
                };
            }
            if sc == fscope {
                if outer == NONE {
                    return Site::Global;
                }
                return match self.it.bc_resolve(n, outer) {
                    Site::Env { hops: h, index } if h + hops + 1 < 0x3FFF => Site::Env { hops: h + hops + 1, index },
                    Site::Env { .. } => Site::Dynamic,
                    other => other,
                };
            }
            if self.scope_has_env(sc) {
                hops += 1;
            }
            sc = self.it.sc_all[sc as usize].parent;
        }
    }

    /// The registers of the variables of scope `sc` that no inner function names. The scope has an environment (`PushEnv` made all
    /// its bindings), so a binding keeps its place there, and a captured variable has no register.
    pub(super) fn rf_env_scope(&mut self, sc: u32) -> G<()> {
        let mut binds = Vec::new();
        self.it.bc_binds(sc, &mut binds);
        for b in binds {
            if self.rf.as_ref().is_some_and(|r| r.captured.contains(&(sc, b.name))) {
                continue;
            }
            let reg = self.reg()?;
            let l = Local { reg, init: b.flags & INIT != 0, is_const: b.kind == BindKind::Const };
            self.rf.as_mut().expect("a register function").locals.insert((sc, b.name), l);
        }
        Ok(())
    }

    /// The variable `n` that a site in scope `sc` means, when a scope of this function declares it.
    pub(super) fn local_at(&self, mut sc: u32, n: u32) -> Option<((u32, u32), Local)> {
        let rf = self.rf.as_ref()?;
        loop {
            if let Some(&l) = rf.locals.get(&(sc, n)) {
                return Some(((sc, n), l));
            }
            // A captured variable is a binding of an environment: a name of a scope outside it is another variable.
            if rf.captured.contains(&(sc, n)) {
                return None;
            }
            if sc == rf.scope || sc == NONE {
                return None;
            }
            sc = self.it.sc_all[sc as usize].parent;
        }
    }

    /// The registers of the bindings of scope `sc` (a block, a `for` head or a `catch`), above the registers in use. The caller
    /// frees them with `free(mark)` when the scope ends.
    pub(super) fn rf_scope(&mut self, sc: u32) -> G<()> {
        let mut binds = Vec::new();
        self.it.bc_binds(sc, &mut binds);
        for b in binds {
            let reg = self.reg()?;
            let l = Local { reg, init: b.flags & INIT != 0, is_const: b.kind == BindKind::Const };
            self.rf.as_mut().expect("a register function").locals.insert((sc, b.name), l);
        }
        Ok(())
    }

    /// The same, or `None` when the variable is a binding of an environment.
    pub(super) fn rf_decl_opt(&mut self, site: u32, n: u32) -> G<Option<((u32, u32), Local)>> {
        let sc = *self.sites.get(&site).ok_or("a site that the walk did not visit")?;
        Ok(self.local_at(sc, n))
    }

    /// The register of the variable that the declaration site `site` declares, before its initializer ran.
    pub(super) fn rf_decl(&mut self, site: u32, n: u32) -> G<((u32, u32), Local)> {
        let sc = *self.sites.get(&site).ok_or("a site that the walk did not visit")?;
        self.local_at(sc, n).ok_or("a declaration that is not a variable of the function")
    }

    pub(super) fn rf_set_init(&mut self, key: (u32, u32)) {
        if let Some(l) = self.rf.as_mut().and_then(|r| r.locals.get_mut(&key)) {
            l.init = true;
        }
    }

    fn strip(&self, mut e: ExprId) -> ExprId {
        while let ExprKind::Paren(i) = self.prog.expr(e).kind {
            e = i;
        }
        e
    }

    /// The register that holds the value of `e`. A local variable is its own register when `keep` is true (nothing that runs
    /// before the use assigns it); any other expression is evaluated into a new register.
    pub(super) fn operand(&mut self, e: ExprId, keep: bool) -> G<u32> {
        let x = self.strip(e);
        if keep && self.rf.is_some() && matches!(self.prog.expr(x).kind, ExprKind::Ident) {
            let n = self.it.cc.expr_name[x.index()];
            if let Site::Reg(r) = self.site(x.index() as u32, n)? {
                return Ok(r);
            }
        }
        let t = self.reg()?;
        self.expr(e, t)?;
        Ok(t)
    }

    /// `true` when evaluating `e` assigns no variable (no assignment, no `++`), so a register read before it is still current after.
    /// A call cannot assign a register of this function. The test is conservative: an unknown node is `false`.
    pub(super) fn pure(&self, e: ExprId) -> bool {
        let prog = &self.prog;
        match prog.expr(e).kind {
            ExprKind::Ident | ExprKind::This | ExprKind::Null | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String => true,
            ExprKind::Paren(i) | ExprKind::Unary { arg: i, .. } | ExprKind::Member { obj: i, .. } => self.pure(i),
            ExprKind::Binary { left, right, .. } | ExprKind::Logical { left, right, .. } | ExprKind::Index { obj: left, index: right, .. } => self.pure(left) && self.pure(right),
            ExprKind::Cond { test, cons, alt } => self.pure(test) && self.pure(cons) && self.pure(alt),
            ExprKind::Call { callee, args, .. } | ExprKind::New { callee, args } => self.pure(callee) && prog.list(args).all(|a| self.pure(a)),
            _ => false,
        }
    }

    /// The constant of a number literal (also `-1`), when it is one of the first 256 constants.
    fn num_konst(&mut self, e: ExprId) -> Option<u32> {
        let x = self.strip(e);
        let n = match self.prog.expr(x).kind {
            ExprKind::Number(base) => self.it.num_lit(self.prog.expr(x).span, base),
            ExprKind::Unary { op: UnaryOp::Neg, arg } => {
                let a = self.strip(arg);
                match self.prog.expr(a).kind {
                    ExprKind::Number(base) => -self.it.num_lit(self.prog.expr(a).span, base),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let k = self.konst(KeyOf::Number(n.to_bits()));
        (k < 256).then_some(k)
    }

    /// A fused compare and jump with the offset still open: the word of the instruction.
    fn jump_cmp(&mut self, op: Op, a: u32, b: u32) -> G<usize> {
        let at = self.here();
        self.emit_ins(Ins::new(op, a, b, 0).with_x(0))?;
        Ok(at)
    }

    /// The jump that is taken when `test` is false. In a register function a comparison is one fused op. Otherwise the value of the
    /// test goes into a register, and `JmpIfNot` tests it. Patch it with `patch`.
    pub(super) fn jump_if_false(&mut self, test: ExprId) -> G<usize> {
        let mark = self.top;
        let t = self.strip(test);
        if self.rf.is_some()
            && let ExprKind::Binary { op, left, right } = self.prog.expr(t).kind
        {
            let ops = match op {
                BinaryOp::Lt => Some((Op::JnLt, Op::JnLtK)),
                BinaryOp::Le => Some((Op::JnLe, Op::JnLeK)),
                BinaryOp::Gt => Some((Op::JnGt, Op::JnGtK)),
                BinaryOp::Ge => Some((Op::JnGe, Op::JnGeK)),
                BinaryOp::Eq => Some((Op::JnEq, Op::JnEqK)),
                BinaryOp::Ne => Some((Op::JnNe, Op::JnNeK)),
                BinaryOp::StrictEq => Some((Op::JnSEq, Op::JnSEqK)),
                BinaryOp::StrictNe => Some((Op::JnSNe, Op::JnSNeK)),
                _ => None,
            };
            if let Some((plain, konst)) = ops {
                let saved = std::mem::replace(&mut self.pos, self.prog.expr(t).span.start);
                let l = self.operand(left, self.pure(right))?;
                let at = match self.num_konst(right) {
                    Some(k) => self.jump_cmp(konst, l, k)?,
                    None => {
                        let r = self.operand(right, true)?;
                        self.jump_cmp(plain, l, r)?
                    }
                };
                self.pos = saved;
                self.free(mark);
                return Ok(at);
            }
        }
        let r = self.reg()?;
        self.expr(test, r)?;
        let j = self.jump(Op::JmpIfNot, r)?;
        self.free(mark);
        Ok(j)
    }

    /// Whether `e` writes its register once, as its last op, so the register may be a variable that `e` also reads.
    fn direct_ok(&self, e: ExprId) -> bool {
        match self.prog.expr(e).kind {
            ExprKind::Paren(i) => self.direct_ok(i),
            ExprKind::Ident | ExprKind::This | ExprKind::Null | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String => true,
            ExprKind::Binary { .. } | ExprKind::Member { optional: false, .. } | ExprKind::Index { optional: false, .. } | ExprKind::Call { optional: false, .. } | ExprKind::New { .. } => true,
            ExprKind::Unary { op, .. } => !matches!(op, UnaryOp::Typeof | UnaryOp::Void | UnaryOp::Delete),
            _ => false,
        }
    }

    /// `e` into register `r`, which may be a variable.
    pub(super) fn expr_into(&mut self, e: ExprId, r: u32) -> G<()> {
        if self.direct_ok(e) {
            return self.expr(e, r);
        }
        let mark = self.top;
        let t = self.reg()?;
        self.expr(e, t)?;
        self.emit(Op::Mov, r, t, 0)?;
        self.free(mark);
        Ok(())
    }

    /// An expression whose value nobody reads. A register function skips the copy of an assignment and the temporary of `i++`.
    pub(super) fn expr_void(&mut self, e: ExprId) -> G<()> {
        let mark = self.top;
        let x = self.strip(e);
        if self.rf.is_some() {
            let saved = std::mem::replace(&mut self.pos, self.prog.expr(x).span.start);
            let done = match self.prog.expr(x).kind {
                ExprKind::Seq(l) => {
                    let prog = self.prog.clone();
                    prog.list(l).try_for_each(|y| self.expr_void(y))?;
                    true
                }
                ExprKind::Update { op, arg, .. } => match self.reg_of(arg, true)? {
                    Some(r) => {
                        self.emit(if op == UpdateOp::Inc { Op::Inc } else { Op::Dec }, r, r, 0)?;
                        true
                    }
                    None => false,
                },
                ExprKind::Assign { op, target, value } if matches!(self.prog.pat(target).kind, PatKind::Ident) => {
                    let n = self.it.cc.cold.pat_name[target.index()];
                    match self.site_w(self.it.pat_site(target.index()), n)? {
                        Site::Reg(r) => {
                            self.rf_assign(op, r, value, None)?;
                            true
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            self.pos = saved;
            if done {
                return Ok(());
            }
        }
        let r = self.reg()?;
        self.expr(e, r)?;
        self.free(mark);
        Ok(())
    }

    /// The register of an identifier (in parentheses too) that is a variable of the function.
    fn reg_of(&mut self, e: ExprId, write: bool) -> G<Option<u32>> {
        let x = self.strip(e);
        if !matches!(self.prog.expr(x).kind, ExprKind::Ident) {
            return Ok(None);
        }
        let n = self.it.cc.expr_name[x.index()];
        let s = if write { self.site_w(x.index() as u32, n)? } else { self.site(x.index() as u32, n)? };
        Ok(if let Site::Reg(r) = s { Some(r) } else { None })
    }

    /// `x = v`, `x += v` and `x &&= v` for a variable in register `r`. The value of the expression goes to `dst` if there is one.
    pub(super) fn rf_assign(&mut self, op: AssignOp, r: u32, value: ExprId, dst: Option<u32>) -> G<()> {
        let mark = self.top;
        match op {
            AssignOp::Assign => self.expr_into(value, r)?,
            AssignOp::Compound(b) => {
                // The old value is read first. If the right side can assign the variable, keep a copy of the old one.
                let l = if self.pure(value) {
                    r
                } else {
                    let t = self.reg()?;
                    self.emit(Op::Mov, t, r, 0)?;
                    t
                };
                self.rf_binop(b, l, value, r)?;
            }
            AssignOp::Logical(l) => {
                let j = self.jump(short_op(l), r)?;
                self.expr_into(value, r)?;
                self.patch(j)?;
            }
        }
        if let Some(d) = dst
            && d != r
        {
            self.emit(Op::Mov, d, r, 0)?;
        }
        self.free(mark);
        Ok(())
    }

    /// `dst = l <op> right`, where `l` is a register that holds the left value.
    fn rf_binop(&mut self, op: BinaryOp, l: u32, right: ExprId, dst: u32) -> G<()> {
        if let Some(k) = self.num_konst(right) {
            let kop = match op {
                BinaryOp::Add => Some(Op::AddK),
                BinaryOp::Sub => Some(Op::SubK),
                BinaryOp::Mul => Some(Op::MulK),
                BinaryOp::Div => Some(Op::DivK),
                BinaryOp::Mod => Some(Op::ModK),
                _ => None,
            };
            if let Some(kop) = kop {
                return self.emit(kop, dst, l, k);
            }
        }
        let mark = self.top;
        let r = self.operand(right, true)?;
        self.emit(super::expr::binary(op), dst, l, r)?;
        self.free(mark);
        Ok(())
    }

    /// The expressions that a register function writes in its own way. `false`: the general code handles it.
    pub(super) fn rf_expr(&mut self, ex: Expr, dst: u32) -> G<bool> {
        let mark = self.top;
        match ex.kind {
            ExprKind::Binary { op, left, right } => {
                let l = self.operand(left, self.pure(right))?;
                self.rf_binop(op, l, right, dst)?;
            }
            ExprKind::Unary { op, arg } if !matches!(op, UnaryOp::Typeof | UnaryOp::Delete | UnaryOp::Void) => {
                let o = self.operand(arg, true)?;
                let code = match op {
                    UnaryOp::Neg => Op::Neg,
                    UnaryOp::Plus => Op::Plus,
                    UnaryOp::Not => Op::Not,
                    _ => Op::BitNot,
                };
                self.emit(code, dst, o, 0)?;
            }
            ExprKind::Member { obj, name, optional: false } => {
                let o = self.operand(obj, true)?;
                self.field(Op::GetField, dst, o, name)?;
            }
            ExprKind::Index { obj, index, optional: false } => {
                let o = self.operand(obj, self.pure(index))?;
                let k = self.operand(index, true)?;
                self.emit(Op::GetElem, dst, o, k)?;
            }
            ExprKind::This => {
                self.rf.as_mut().expect("a register function").uses_this = true;
                return Ok(false);
            }
            ExprKind::Update { op, prefix, arg } => {
                let Some(r) = self.reg_of(arg, true)? else { return Ok(false) };
                let step = if op == UpdateOp::Inc { Op::Inc } else { Op::Dec };
                if prefix {
                    self.emit(step, r, r, 0)?;
                    self.emit(Op::Mov, dst, r, 0)?;
                } else {
                    self.emit(Op::ToNumeric, dst, r, 0)?;
                    self.emit(step, r, dst, 0)?;
                }
            }
            ExprKind::Assign { op, target, value } if matches!(self.prog.pat(target).kind, PatKind::Ident) => {
                let n = self.it.cc.cold.pat_name[target.index()];
                let Site::Reg(r) = self.site_w(self.it.pat_site(target.index()), n)? else { return Ok(false) };
                self.rf_assign(op, r, value, Some(dst))?;
            }
            ExprKind::Cond { test, cons, alt } => {
                let j = self.jump_if_false(test)?;
                self.expr(cons, dst)?;
                let end = self.jump(Op::Jmp, 0)?;
                self.patch(j)?;
                self.expr(alt, dst)?;
                self.patch(end)?;
            }
            _ => return Ok(false),
        }
        self.free(mark);
        Ok(true)
    }

    /// `return e`. A conditional returns from each branch.
    pub(super) fn return_expr(&mut self, e: ExprId) -> G<()> {
        let x = self.strip(e);
        if let ExprKind::Cond { test, cons, alt } = self.prog.expr(x).kind {
            let j = self.jump_if_false(test)?;
            self.return_expr(cons)?;
            self.patch(j)?;
            return self.return_expr(alt);
        }
        let mark = self.top;
        let r = self.operand(e, true)?;
        self.exit(Exit::Return, r)?;
        self.free(mark);
        Ok(())
    }
}

/// The jump that skips the right side of a logical assignment.
fn short_op(op: LogicalOp) -> Op {
    match op {
        LogicalOp::And => Op::JmpIfNot,
        LogicalOp::Or => Op::JmpIf,
        LogicalOp::Nullish => Op::JmpNotNullish,
    }
}

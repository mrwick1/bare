//! The loop of the bytecode interpreter (step B3 of `docs/engine-plan.md`), core subset: the code of `bc_gen.rs`.
//! A child module of `eval`, so it uses the environments, the names and the operators of the tree walker.
//!
//! - A call of a compiled function comes through `call_script` (`bc_enter`): the environment of the call and `bind_call` are
//!   those of the tree walker, then the loop runs the code. A `Call` op calls `call_fn`, so a JS to JS call is a Rust call (step
//!   B4 changes that).
//! - The registers of all frames are one stack, `BcState::regs`. A frame owns `nregs` registers from its base. Every value in
//!   them is reachable from `Interp::roots` while the frame runs. `collect` refuses to run during a call, and a script body that
//!   runs here is at depth 0 with no host code between, so the rule of `collect` does not change.
//! - `self.env` is the current environment, as in the tree walker. `PushEnv` and `PopEnv` move it. The loop counts what it
//!   pushed, so every exit (a return, a throw that no handler takes, a limit) pops it.
//! - A JS throw (`Error::Throw`) goes to the handler of its word (`Code::handler_at`). Any other error (a limit, `Unsupported`, a
//!   heap fault) leaves the function at once and skips `catch` and `finally`, as in the tree walker.
//! - One step (`tick`) for each backward jump and for each call (in `call_fn`). The tree walker counts a step for each node, so
//!   the step counts of the two engines differ.
use super::bc_gen::G;
use super::*;
use crate::bc::{self, Const, Op};

/// How the loop reads `this`.
#[derive(Clone, Copy)]
pub(super) enum This {
    /// The `this` of the call (a function that is not an arrow and not in a class).
    Fixed(Value),
    /// Through the environments (an arrow function, the code of a class, a script body).
    Dynamic,
}

/// A caller that waits for an inline call (stage 2: a register function calls another one in the same loop).
pub(super) struct Fr {
    code: Rc<Compiled>,
    /// The word after the call, the base of the caller's registers, the register of the caller that gets the result.
    pc: u32,
    base: u32,
    dst: u32,
    /// The environment of the caller.
    env: u32,
    this: This,
    /// The number of environments that the caller pushed. The callee's handlers count from 0.
    depth: u32,
    /// The first iterator of the caller on `RegStack::iters`.
    iters: u32,
    /// The environment of the call that the callee made (a callee with an environment, as stage 1 makes), or `u32::MAX`.
    callee_env: u32,
}

/// The registers and the waiting callers of one run of the loop. One run has one of these from the pool, so a nested run (a
/// native function that calls back into script code) has its own.
#[derive(Default)]
pub(super) struct RegStack {
    regs: Vec<Value>,
    frames: Vec<Fr>,
    /// The open iterators of `for-of` and `for-in` loops of all frames, innermost last. Empty between runs.
    iters: Vec<BcIter>,
}

impl RegStack {
    /// Grow the registers to `n` for a call, by at least half of the capacity. A stack goes back to the pool with its capacity, and
    /// the doubling of `Vec` kept up to twice the deepest run of each stack. Exact growth copied the stack at each new depth.
    #[cold]
    #[inline(never)]
    pub(super) fn grow(&mut self, n: usize) {
        let c = self.regs.capacity();
        if c < n {
            self.regs.reserve_exact(n.max(c + c / 2) - self.regs.len());
        }
        self.regs.resize(n, Value::Undefined);
    }

    /// The length and the capacity of the registers (tests).
    #[cfg(test)]
    pub(super) fn regs_len_cap(&self) -> (usize, usize) {
        (self.regs.len(), self.regs.capacity())
    }
}

/// An open loop iterator (`IterOpen`, `ForInOpen`).
pub(super) enum BcIter {
    Of(It),
    In { obj: Value, keys: Vec<PropertyKey>, i: usize, proxied: bool },
}

macro_rules! op_consts {
    ($($c:ident = $o:ident,)*) => { $(const $c: u8 = Op::$o as u8;)* };
}
op_consts! {
    NOP = Nop, MOV = Mov, LOAD_UNDEF = LoadUndef, LOAD_NULL = LoadNull, LOAD_TRUE = LoadTrue, LOAD_FALSE = LoadFalse,
    LOAD_THIS = LoadThis, LOAD_I = LoadI, LOAD_K = LoadK, GET_ENV = GetEnv, SET_ENV = SetEnv, INIT_ENV = InitEnv, PUSH_ENV = PushEnv,
    DECL_ENV = DeclEnv, WIDE = Wide, POP_ENV = PopEnv, COPY_ENV = CopyEnv, GET_GLOBAL = GetGlobal, SET_GLOBAL = SetGlobal,
    TYPEOF_GLOBAL = TypeofGlobal, GET_NAME = GetName, SET_NAME = SetName, TYPEOF_NAME = TypeofName, GET_FIELD = GetField,
    SET_FIELD = SetField, GET_ELEM = GetElem, SET_ELEM = SetElem, TO_KEY = ToKey, DEF_FIELD = DefField, ARRAY_PUSH = ArrayPush,
    NEW_OBJECT = NewObject, NEW_ARRAY = NewArray, CLOSURE = Closure, CLOSURE_NAME = ClosureName, CALL = Call, NEW = New,
    ADD = Add, SUB = Sub, MUL = Mul, DIV = Div, MOD = Mod, EXP = Exp, BIT_AND = BitAnd, BIT_OR = BitOr, BIT_XOR = BitXor, SHL = Shl,
    SHR = Shr, USHR = UShr, EQ = Eq, NE = Ne, STRICT_EQ = StrictEq, STRICT_NE = StrictNe, LT = Lt, LE = Le, GT = Gt, GE = Ge,
    IN = In, INSTANCE_OF = InstanceOf, NEG = Neg, PLUS = Plus, NOT = Not, BIT_NOT = BitNot, TYPEOF = Typeof, INC = Inc, DEC = Dec,
    TO_NUMERIC = ToNumeric, TO_STR = ToStr, CONCAT = Concat, DELETE = Delete, JMP = Jmp, JMP_IF = JmpIf, JMP_IF_NOT = JmpIfNot,
    JMP_NULLISH = JmpNullish, JMP_NOT_NULLISH = JmpNotNullish, RET = Ret, RET_UNDEF = RetUndef, THROW = Throw,
    JN_LT = JnLt, JN_LE = JnLe, JN_GT = JnGt, JN_GE = JnGe, JN_EQ = JnEq, JN_NE = JnNe, JN_SEQ = JnSEq, JN_SNE = JnSNe,
    JN_LT_K = JnLtK, JN_LE_K = JnLeK, JN_GT_K = JnGtK, JN_GE_K = JnGeK, JN_EQ_K = JnEqK, JN_NE_K = JnNeK, JN_SEQ_K = JnSEqK,
    JN_SNE_K = JnSNeK, CALL_R = CallR, ADD_K = AddK, SUB_K = SubK, MUL_K = MulK, DIV_K = DivK, MOD_K = ModK,
    ITER_OPEN = IterOpen, FOR_IN_OPEN = ForInOpen, ITER_NEXT = IterNext, ITER_CLOSE = IterClose, ITER_STEP = IterStep,
    ITER_REST = IterRest, OBJ_REST = ObjRest, REQUIRE_OBJ = RequireObj, SPREAD_PUSH = SpreadPush, CALL_SPREAD = CallSpread,
    DEF_ELEM = DefElem, REG_EXP_LIT = RegExpLit,
}

/// Every wide word of the (verified) code is one that the loop reads: `GetEnv`, `SetEnv`, `InitEnv` or `DefField`.
fn bc_wide_ok(ops: &[u32]) -> bool {
    let mut pc = 0;
    while pc < ops.len() {
        let Ok((ins, n)) = bc::decode(ops, pc) else { return false };
        if ops[pc] as u8 == Op::Wide as u8 && !matches!(ins.op, Op::GetEnv | Op::SetEnv | Op::InitEnv | Op::DefField) {
            return false;
        }
        pc += n;
    }
    true
}

impl Interp<'_> {
    /// An operator without an arm fast path. Bitwise ops and shifts of two numbers do not enter the large `binary`.
    #[inline(never)]
    fn bc_binary(&mut self, op: u8, l: Value, r: Value) -> Res<Value> {
        if let (Value::Number(x), Value::Number(y), BIT_AND | BIT_OR | BIT_XOR | SHL | SHR | USHR) = (l, r, op) {
            let (i, j) = (to_int32(x), to_uint32(y));
            return Ok(Value::Number(match op {
                BIT_AND => (i & j as i32) as f64,
                BIT_OR => (i | j as i32) as f64,
                BIT_XOR => (i ^ j as i32) as f64,
                SHL => i.wrapping_shl(j & 31) as f64,
                SHR => (i >> (j & 31)) as f64,
                _ => ((i as u32) >> (j & 31)) as f64,
            }));
        }
        self.binary(binop(op), l, r)
    }
}

fn binop(op: u8) -> BinaryOp {
    match op {
        ADD => BinaryOp::Add,
        SUB => BinaryOp::Sub,
        MUL => BinaryOp::Mul,
        DIV => BinaryOp::Div,
        MOD => BinaryOp::Mod,
        EXP => BinaryOp::Exp,
        BIT_AND => BinaryOp::BitAnd,
        BIT_OR => BinaryOp::BitOr,
        BIT_XOR => BinaryOp::BitXor,
        SHL => BinaryOp::Shl,
        SHR => BinaryOp::Shr,
        USHR => BinaryOp::UShr,
        EQ => BinaryOp::Eq,
        NE => BinaryOp::Ne,
        STRICT_EQ => BinaryOp::StrictEq,
        STRICT_NE => BinaryOp::StrictNe,
        LT => BinaryOp::Lt,
        LE => BinaryOp::Le,
        GT => BinaryOp::Gt,
        GE => BinaryOp::Ge,
        IN => BinaryOp::In,
        _ => BinaryOp::Instanceof,
    }
}

impl<'p> Interp<'p> {
    /// Choose the engine for script code. The default is [`Exec::Tree`].
    pub fn set_exec(&mut self, e: Exec) {
        self.bc.exec = e;
    }

    /// The counters of the switch.
    pub fn bc_stats(&self) -> BcStats {
        self.bc.stats
    }

    /// A call that the tree walker runs while the lane is on (the counters of the measurement).
    #[inline(always)]
    pub(super) fn bc_tree_call(&mut self, why: &'static str) {
        if self.bc.exec != Exec::Tree {
            self.bc.stats.calls_tree += 1;
            *self.bc.refused.entry(why).or_default() += 1;
        }
    }

    /// The string constants of compiled code (`roots`).
    pub(super) fn bc_roots(&self, v: &mut Vec<Value>) {
        for s in &self.bc.codes {
            for c in s.iter() {
                if let BcSlot::Byte(c) = c {
                    v.extend(c.kv.iter().filter(|x| matches!(x, Value::String(_))));
                }
            }
        }
    }

    /// Empty the inline caches and the global cells of all compiled code (`reset_caches`, after a shape sweep). A cell holds a
    /// position, not a shape id, but a sweep can change the global object, so it learns again too.
    pub(super) fn bc_reset(&self) {
        for s in &self.bc.codes {
            for c in s.iter() {
                if let BcSlot::Byte(c) = c {
                    c.code.ics.iter().for_each(|ic| ic.set(Ic::EMPTY));
                    c.cells.iter().for_each(GCell::forget);
                }
            }
        }
    }

    /// Compile `code` into a `Compiled` (verify it and make its constants).
    fn bc_finish(&mut self, r: G<crate::bc::Code>) -> Option<Rc<Compiled>> {
        // The loop reads the wide form of `GetEnv`, `SetEnv`, `InitEnv` and `DefField` only: code with another wide word stays on the tree
        // walker.
        let code = match r {
            Ok(c) if bc_verify::verify(&c).is_err() => {
                self.bc.stats.fallbacks += 1;
                self.bc.why = "verify";
                return None;
            }
            Ok(c) if !bc_wide_ok(&c.ops) => {
                self.bc.stats.fallbacks += 1;
                self.bc.why = "wide form";
                return None;
            }
            Ok(c) => c,
            Err(e) => {
                self.bc.stats.fallbacks += 1;
                self.bc.why = e;
                return None;
            }
        };
        let mut kv = Vec::with_capacity(code.consts.len());
        let mut cells = Vec::with_capacity(code.consts.len());
        for k in &code.consts {
            let key = match k {
                Const::Name(n) => match self.name_key(*n) {
                    Ok(key) => key,
                    Err(_) => {
                        self.bc.stats.fallbacks += 1;
                        self.bc.why = "constant";
                        return None;
                    }
                },
                _ => PropertyKey::Index(0),
            };
            cells.push(GCell::new(key));
            kv.push(match k {
                Const::Number(n) => Value::Number(*n),
                Const::String(u) => match self.heap.intern_vec(u.to_vec()) {
                    Ok(s) => Value::String(s),
                    Err(_) => {
                        self.bc.stats.fallbacks += 1;
                        self.bc.why = "constant";
                        return None;
                    }
                },
                Const::Name(_) => Value::Undefined,
            });
        }
        self.bc.stats.compiled += 1;
        let rc = Rc::new(Compiled {
            code,
            script: self.cur as u32,
            kv: kv.into(),
            cells: cells.into(),
            #[cfg(feature = "bc-hist")]
            hist: Default::default(),
        });
        #[cfg(feature = "bc-hist")]
        self.bc.hist.funcs.push((0, 0, "<script>".into(), rc.clone()));
        Some(rc)
    }

    /// The code of function `fid` of the current script (`script`), compiled when the mode says so. `None`: the tree walker runs it.
    fn bc_code(&mut self, script: u32, fid: FuncId) -> Option<Rc<Compiled>> {
        let s = script as usize;
        if self.bc.codes.len() <= s {
            self.bc.codes.resize_with(s + 1, Default::default);
        }
        if self.bc.codes[s].is_empty() {
            self.bc.codes[s] = vec![BcSlot::New; self.prog.stats().funcs + 1].into();
        }
        match &self.bc.codes[s][fid.index()] {
            BcSlot::Byte(c) => return Some(c.clone()),
            BcSlot::Tree(r) => {
                *self.bc.refused.entry(r).or_default() += 1;
                self.bc.stats.calls_tree += 1;
                return None;
            }
            BcSlot::New if self.bc.exec == Exec::Auto && self.cc.scopes[fid.index()].fl & FL_EAGER == 0 => {
                self.bc.codes[s][fid.index()] = BcSlot::Seen;
                return None;
            }
            BcSlot::New | BcSlot::Seen => {}
        }
        let r = self.bc_compile(fid);
        let c = self.bc_finish(r);
        #[cfg(feature = "bc-hist")]
        if c.is_some() {
            let f = self.prog.func(fid);
            let name = f.name.map_or_else(|| format!("@{}", f.span.start), |n| self.prog.text(n).to_string());
            self.bc.hist.label(script, fid.index() as u32, name);
        }
        let why = self.bc.why;
        self.bc.codes[s][fid.index()] = match &c {
            Some(c) => BcSlot::Byte(c.clone()),
            None => BcSlot::Tree(why),
        };
        if c.is_none() {
            *self.bc.refused.entry(why).or_default() += 1;
            self.bc.stats.calls_tree += 1;
        }
        c
    }

    /// The bytecode side of `call_script` (a plain or `Env` function, its script selected). `None`: the function runs on the tree
    /// walker, and the caller goes on.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn bc_enter(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, cenv: u32, self_binding: bool, this: Value, args: &[Value], new: bool, saved: (u32, bool, usize)) -> Option<Res<Value>> {
        let code = self.bc_code(script, fid)?;
        self.bc.stats.calls_lane += 1;
        let (saved_env, saved_strict, saved_script) = saved;
        if code.code.regfn && code.code.has_env {
            // A register function with inner functions: the environment and the bindings of a call are those of stage 1 (the inner
            // functions find the variables that they name there). Its other variables are registers.
            let env = match self.new_env(cenv, true) {
                Ok(e) => e,
                Err(e) => {
                    self.select(saved_script);
                    return Some(Err(e));
                }
            };
            self.env = env;
            let r = self.in_frame(script, Where::Func(fid), new, |it| {
                it.bind_call(fobj, fid, env, self_binding, this, args)?;
                it.bc_run_rf(&code, this, args)
            });
            self.select(saved_script);
            self.env = saved_env;
            self.strict = saved_strict;
            self.release(env);
            return Some(r);
        }
        if code.code.regfn {
            // A register function has no environment: its closure's environment is the one its outer names resolve from.
            self.env = cenv;
            self.strict = code.code.strict;
            let r = self.in_frame(script, Where::Func(fid), new, |it| it.bc_run_rf(&code, this, args));
            self.select(saved_script);
            self.env = saved_env;
            self.strict = saved_strict;
            return Some(r);
        }
        let env = match self.new_env(cenv, true) {
            Ok(e) => e,
            Err(e) => {
                self.select(saved_script);
                return Some(Err(e));
            }
        };
        self.env = env;
        let f = *self.prog.func(fid);
        let fixed = f.kind != FuncKind::Arrow && self.cc.cold.fflags[fid.index()] == 0;
        let r = self.in_frame(script, Where::Func(fid), new, |it| {
            it.bind_call(fobj, fid, env, self_binding, this, args)?;
            let t = if fixed { This::Fixed(it.envs[env as usize].this.unwrap_or(Value::Undefined)) } else { This::Dynamic };
            it.bc_run(&code, t)
        });
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        self.release(env);
        Some(r)
    }

    /// Run the body of the current script on bytecode, after its names were declared (`exec_current`). `None`: the mode is `Tree`,
    /// or the body has no loop and no call, or it does not compile. The tree walker runs it then.
    pub(super) fn bc_script(&mut self) -> Option<Ex> {
        if self.bc.exec == Exec::Tree {
            return None;
        }
        let r = self.bc_compile_script();
        // A body with no call and no backward jump runs once and is short: the tree walker runs it.
        if let Ok(c) = &r
            && c.calls.is_empty()
        {
            let mut pc = 0;
            let mut back = false;
            while let Ok((i, n)) = bc::decode(&c.ops, pc) {
                back |= (matches!(i.op, Op::Jmp | Op::JmpIf | Op::JmpIfNot) || i.op.is_fused_jump()) && i.offset() < 0;
                pc += n;
            }
            if !back {
                return None;
            }
        }
        let code = self.bc_finish(r)?;
        // The loop may leave another script selected: the tree walker goes on in this one.
        let v = self.bc_run(&code, This::Dynamic);
        self.bc_sync(&code);
        let v = match v {
            Ok(v) => v,
            Err(e) => return Some(Err(Ctl::Err(e))),
        };
        self.completion = v;
        Some(Ok(()))
    }

    /// Run `c` in a new frame of registers. Every exit pops the environments that the code pushed.
    fn bc_run(&mut self, c: &Rc<Compiled>, this: This) -> Res<Value> {
        self.bc.stats.runs += 1;
        let mut st = self.bc.pool.pop().unwrap_or_default();
        st.regs.clear();
        st.regs.resize(c.code.nregs as usize, Value::Undefined);
        let mut depth = 0u32;
        let r = self.bc_loop(c, &mut st, &mut depth, this);
        for _ in 0..depth {
            let e = self.env;
            self.env = self.envs[e as usize].parent;
            self.release(e);
        }
        self.bc.pool.push(st);
        r
    }

    /// Run a register function: the arguments are its first registers, the other variables start as `undefined`.
    fn bc_run_rf(&mut self, c: &Rc<Compiled>, this: Value, args: &[Value]) -> Res<Value> {
        self.bc.stats.runs += 1;
        let k = &c.code;
        let t = if k.dyn_this {
            This::Dynamic
        } else if k.uses_this {
            This::Fixed(self.bc_bind_this(this, k.strict)?)
        } else {
            This::Fixed(Value::Undefined)
        };
        let mut st = self.bc.pool.pop().unwrap_or_default();
        st.regs.clear();
        st.regs.resize(k.nregs as usize, Value::Undefined);
        let n = args.len().min(k.nparams as usize);
        st.regs[..n].copy_from_slice(&args[..n]);
        let mut depth = 0u32;
        let r = self.bc_loop(c, &mut st, &mut depth, t);
        // A function with an environment can leave blocks open (a return in a block).
        for _ in 0..depth {
            let e = self.env;
            self.env = self.envs[e as usize].parent;
            self.release(e);
        }
        self.bc.pool.push(st);
        r
    }

    /// `this` of a call, as `bind_call` makes it.
    fn bc_bind_this(&mut self, this: Value, strict: bool) -> Res<Value> {
        Ok(match this {
            Value::Object(_) => this,
            _ if strict => this,
            _ if is_nullish(this) => Value::Object(self.global),
            _ => self.to_object(this)?,
        })
    }

    /// The compiled function that `f` is, of any script: it runs in the loop of the caller. `None`: the call goes through `call_fn`.
    #[inline(always)]
    fn bc_target(&self, f: Value) -> Option<(Rc<Compiled>, FuncId, u32, ObjId)> {
        let Value::Object(o) = f else { return None };
        let Some(&Callee::Script { script, fid, env, self_binding: false, .. }) = self.funcs.get(&o) else { return None };
        match self.bc.codes.get(script as usize)?.get(fid.index())? {
            BcSlot::Byte(c) => Some((c.clone(), fid, env, o)),
            _ => None,
        }
    }

    /// The part of an inline call (`CallR`) of a function with an environment, or of a function that is not a register function:
    /// the environment and the bindings of `bc_enter`, made in the callee's script and frame (default values of parameters run
    /// there). It gives `this` of the callee, the caller's environment, the callee's environment and the registers to clear.
    #[cold]
    #[inline(never)]
    fn bc_call_env(&mut self, callee: &Compiled, (fid, fobj, cenv): (FuncId, ObjId, u32), this_v: Value, args: &[Value], pos: u32) -> Res<(This, u32, u32, usize, usize)> {
        self.bc_sync(callee);
        let k = &callee.code;
        let env = self.new_env(cenv, true)?;
        let caller_env = std::mem::replace(&mut self.env, env);
        self.stk.frames.push(StackFrame { script: callee.script, at: Where::Func(fid), new: false, saved_pos: pos });
        self.stk.pos = 0;
        if let Err(e) = self.bind_call(fobj, fid, env, false, this_v, args) {
            self.stk.frames.pop();
            self.stk.pos = pos;
            self.env = caller_env;
            self.release(env);
            return Err(e);
        }
        let fixed = if k.regfn { !k.dyn_this } else { self.prog.func(fid).kind != FuncKind::Arrow && self.cc.cold.fflags[fid.index()] == 0 };
        let t = if fixed { This::Fixed(self.envs[env as usize].this.unwrap_or(Value::Undefined)) } else { This::Dynamic };
        let (from, to) = if k.regfn { (args.len().min(k.nparams as usize), k.nlocals as usize) } else { (0, k.nregs as usize) };
        Ok((t, caller_env, env, from, to))
    }

    /// Select the script of `c` for an op that reads the code tables of the current script (`prog`, `cc`, `cur`). The loop runs
    /// code of any script and does not select at a call.
    #[inline(always)]
    fn bc_sync(&mut self, c: &Compiled) {
        if self.cur != c.script as usize {
            self.select(c.script as usize);
        }
    }

    /// The caller `fr` of an inline call is current again: leave the callee. It pops the environments that the callee pushed, and
    /// releases the one of its call. `ok`: it returned. The caller sets `strict`.
    #[inline(always)]
    fn bc_frame_exit(&mut self, fr: &Fr, ok: bool, depth: &mut u32) {
        for _ in 0..*depth {
            let e = self.env;
            self.env = self.envs[e as usize].parent;
            self.release(e);
        }
        self.depth -= 1;
        let saved_pos = self.stk.frames.pop().map_or(0, |f| f.saved_pos);
        self.stk.pos = if ok { 0 } else { saved_pos };
        self.env = fr.env;
        if fr.callee_env != u32::MAX {
            self.release(fr.callee_env);
        }
        *depth = fr.depth;
    }

    /// A thrown error at word `pc`: the target of its handler, the register of the exception and the exception. Any other error,
    /// or no handler: the error, and the loop returns it.
    #[cold]
    #[inline(never)]
    fn bc_catch(&mut self, c: &Compiled, pc: usize, depth: &mut u32, iters: &mut Vec<BcIter>, ibase: usize, e: Error) -> Res<(usize, usize, Value)> {
        let Error::Throw(v) = e else { return Err(e) };
        let Some(h) = c.code.handler_at(pc as u32) else { return Err(Error::Throw(v)) };
        let Error::Throw(v) = self.bc_close_iters(iters, ibase + h.iters as usize, Error::Throw(v)) else { unreachable!() };
        while *depth > h.depth as u32 {
            let e = self.env;
            self.env = self.envs[e as usize].parent;
            self.release(e);
            *depth -= 1;
        }
        Ok((h.target as usize, h.reg as usize, v))
    }

    /// Close the iterators above `keep` after the error `e`, innermost first (`iter_close_throw`: an error of `return` is dropped).
    /// An error that is not a thrown value runs no JS: the iterators are dropped.
    #[cold]
    #[inline(never)]
    fn bc_close_iters(&mut self, iters: &mut Vec<BcIter>, keep: usize, mut e: Error) -> Error {
        while iters.len() > keep {
            if let Some(BcIter::Of(mut it)) = iters.pop() {
                e = self.iter_close_throw(&mut it, e);
            }
        }
        e
    }

    /// `IterOpen`: `iter_start`, or for an array pattern `iter_start_val`. Out of the loop, as the pattern ops are: their code in
    /// the loop made every other op slower (2.7% more instructions on a React journey with the same work).
    #[inline(never)]
    fn bc_iter_open(&mut self, c: &Compiled, at: usize, v: Value, pattern: bool) -> Res<It> {
        self.bc_sync(c);
        if pattern { self.iter_start_val(v) } else { self.iter_start(v, Self::bc_call_expr(c, at)) }
    }

    /// The ops of patterns: `IterStep`, `IterRest`, `ObjRest` (`c` keys after register `a`) and `RequireObj`.
    #[inline(never)]
    fn bc_pattern_op(&mut self, op: u8, a: usize, b: usize, c: usize, regs: &mut [Value], iters: &mut [BcIter]) -> Res<()> {
        match op {
            ITER_STEP | ITER_REST => {
                let Some(BcIter::Of(it)) = iters.last_mut() else { return Err(unsupported("bad bytecode", Span::default())) };
                regs[a] = if op == ITER_STEP { self.iter_next(it)?.unwrap_or(Value::Undefined) } else { self.bc_iter_rest(it)? };
            }
            OBJ_REST => regs[a] = self.bc_obj_rest(regs[b], &regs[a + 1..a + 1 + c])?,
            _ => {
                let v = regs[a];
                if is_nullish(v) {
                    let t = self.display(v);
                    return Err(self.type_err(&format!("Cannot destructure '{t}' as it is {t}.")));
                }
            }
        }
        Ok(())
    }

    /// The rare ops without an arm in the loop: the word `w` at `at`. They run in one cold call so that they do not change the code
    /// of the hot arms.
    #[inline(never)]
    fn bc_rare(&mut self, code: &Compiled, at: usize, w: u32, regs: &mut [Value]) -> Res<()> {
        self.bc_sync(code);
        let (op, a, b, c) = (w as u8, (w >> 8 & 255) as usize, (w >> 16 & 255) as usize, (w >> 24) as usize);
        match op {
            SPREAD_PUSH => self.bc_spread_push(code, at, regs, a, b, c != 0)?,
            CALL_SPREAD => regs[a] = self.bc_call_spread(code, at, regs, b, c != 0)?,
            DELETE => regs[a] = self.bc_delete(regs[b], regs[c])?,
            DEF_ELEM => self.bc_def_elem(regs[a], regs[b], regs[c])?,
            REG_EXP_LIT => regs[a] = self.ext_eval_regexp(Self::bc_call_expr(code, at))?,
            _ => return Err(unsupported("bad bytecode", Span::default())),
        }
        Ok(())
    }

    /// `Delete`: `delete base[key]` (`eval_delete`).
    fn bc_delete(&mut self, base: Value, kv: Value) -> Res<Value> {
        let key = self.coerce_key(kv)?;
        let o = match base {
            Value::Object(o) => o,
            Value::Undefined | Value::Null => {
                let t = self.display(base);
                let k = self.key_text(key);
                return Err(self.type_err(&format!("Cannot convert undefined or null to object ({t}, deleting '{k}')")));
            }
            _ => return Ok(Value::Bool(true)),
        };
        let ok = hxw!(self, self.heap.delete(o, key), self.px_delete(o, key)?);
        if !ok && self.strict {
            let k = self.key_text(key);
            return Err(self.type_err(&format!("Cannot delete property '{k}' of #<Object>")));
        }
        Ok(Value::Bool(ok))
    }

    /// `DefElem`: a computed key of an object literal.
    fn bc_def_elem(&mut self, o: Value, k: Value, v: Value) -> Res<()> {
        let Value::Object(o) = o else { return Err(unsupported("DefElem on a value that is not an object", Span::default())) };
        let key = self.coerce_key(k)?;
        self.def(o, key, v, true, true, true)
    }

    /// `SpreadPush`: the values of the iteration of `regs[b]` go to the end of the array `regs[a]`. The call table of the word names
    /// the spread operand for the error text. `args` is a spread in the arguments of a call (the buffer limit of `eval_args`).
    fn bc_spread_push(&mut self, c: &Compiled, pc: usize, regs: &[Value], a: usize, b: usize, args: bool) -> Res<()> {
        let Value::Object(arr) = regs[a] else { return Err(unsupported("SpreadPush on a value that is not an array", Span::default())) };
        let v = regs[b];
        let mut n = h!(self, self.heap.array_length(arr)).unwrap_or(0);
        if !args && let Value::Object(o) = v && let Some(more) = h!(self, self.heap.array_length(o)) {
            h!(self, self.heap.reserve_elems(arr, more as usize));
        }
        let mut it = self.iter_start(v, Self::bc_call_expr(c, pc))?;
        while let Some(item) = self.iter_next(&mut it)? {
            if args {
                self.iter_buffer_room(n as usize)?;
            }
            self.def(arr, PropertyKey::Index(n), item, true, true, true)?;
            n += 1;
        }
        Ok(())
    }

    /// `CallSpread`: `bc_call` with the arguments in an array.
    fn bc_call_spread(&mut self, c: &Compiled, pc: usize, regs: &[Value], b: usize, new: bool) -> Res<Value> {
        let e = Self::bc_call_expr(c, pc);
        let f = regs[b];
        let Value::Object(arr) = regs[b + if new { 1 } else { 2 }] else { return Err(unsupported("CallSpread without an array", Span::default())) };
        let mut argv = self.arg_pool.pop().unwrap_or_default();
        argv.clear();
        for i in 0..h!(self, self.heap.array_length(arr)).unwrap_or(0) {
            argv.push(self.get_prop(Value::Object(arr), PropertyKey::Index(i))?);
        }
        self.mark_call(e);
        let callee = match self.prog.expr(e).kind {
            ExprKind::Call { callee, .. } | ExprKind::New { callee, .. } => callee,
            _ => e,
        };
        let r = if new {
            self.construct(f, &argv, callee)
        } else if self.is_callable(f) {
            self.call_fn(f, regs[b + 1], &argv)
        } else {
            Err(self.not_a_function(callee))
        };
        self.stk.pos = 0;
        let r = r?;
        self.arg_pool.push(argv);
        Ok(r)
    }

    /// The values that are left in `it`, as a new array (a rest element, `bind_array_elems`).
    fn bc_iter_rest(&mut self, it: &mut It) -> Res<Value> {
        let mut rest = Vec::new();
        while let Some(x) = self.iter_next(it)? {
            self.iter_buffer_room(rest.len())?;
            rest.push(x);
        }
        self.new_array_from(&rest)
    }

    /// A rest property: a new object with the properties of `src` except `keys` (`bind_pattern`).
    fn bc_obj_rest(&mut self, src: Value, keys: &[Value]) -> Res<Value> {
        let mut used = Vec::with_capacity(keys.len());
        for &k in keys {
            used.push(self.coerce_key(k)?);
        }
        let o = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.copy_props(o, src, &used)?;
        Ok(Value::Object(o))
    }

    /// The next key of a `for-in` that the object still has (`exec_for_each`).
    fn bc_for_in_next(&mut self, obj: Value, keys: &[PropertyKey], i: &mut usize, proxied: bool) -> Res<Option<Value>> {
        while let Some(&k) = keys.get(*i) {
            *i += 1;
            if let Value::Object(o) = obj
                && !(if proxied { self.px_for_in_has(o, k)? } else { self.o_has_property(o, k)? })
            {
                continue;
            }
            return Ok(Some(h!(self, self.heap.key_to_value(k))));
        }
        Ok(None)
    }

    /// The bindings of the environment of scope `s`, in the order of `bc_binds`.
    pub(super) fn bc_tmpl(&mut self, s: u32) -> Rc<[(u32, u8)]> {
        if let Some(t) = self.bc.tmpl.get(&s) {
            return t.clone();
        }
        let mut b = Vec::new();
        self.bc_binds(s, &mut b);
        let t: Rc<[(u32, u8)]> = b.iter().map(|b| (b.name, b.flags)).collect();
        self.bc.tmpl.insert(s, t.clone());
        t
    }

    /// `PushEnv` and `DeclEnv`: the bindings of scope `s` from the count of the environment on.
    #[inline(never)]
    fn bc_declare(&mut self, c: &Compiled, s: u32, push: bool) -> Res<()> {
        self.bc_sync(c);
        if push {
            self.env = self.new_env(self.env, false)?;
        }
        let t = self.bc_tmpl(s);
        let env = self.env;
        let from = self.envs[env as usize].bindings.len();
        for &(n, f) in t.get(from..).unwrap_or(&[]) {
            self.env_declare(env, n, f, Value::Undefined);
        }
        Ok(())
    }

    /// The environment `hops` levels up and binding `i` of it.
    #[inline(always)]
    fn bc_slot(&self, hops: u32, i: u32) -> (u32, usize) {
        let mut e = self.env;
        for _ in 0..hops {
            e = self.envs[e as usize].parent;
        }
        (e, i as usize)
    }

    /// A binding that is in its TDZ, or a binding that the code expected and that is not there.
    #[cold]
    #[inline(never)]
    fn bc_env_err(&mut self, e: u32, i: usize) -> Error {
        match self.envs[e as usize].bindings.get(i) {
            Some(b) => self.tdz(b.name),
            None => unsupported("a binding that the code generator expected", Span::default()),
        }
    }

    /// `GetGlobal`: environment 0 (a lexical name of another script), then the global object.
    #[inline(never)]
    fn bc_get_global(&mut self, n: u32, cell: &GCell) -> Res<Value> {
        if !self.envs[0].bindings.is_empty()
            && let Some(i) = self.find_in(0, n)
        {
            cell.st.set((GC_LEX as u64) << 32 | i as u64);
            let b = self.envs[0].bindings[i];
            return if b.flags & INIT != 0 { Ok(b.value) } else { Err(self.tdz(n)) };
        }
        let v = self.get_global(n)?;
        self.bc_learn(cell);
        Ok(v)
    }

    /// `GetGlobal` when environment 0 has bindings: the value if the cell knows where the name is, else `None` (the slow path).
    #[inline(never)]
    fn bc_global_hit(&self, cell: &GCell) -> Option<Value> {
        let st = cell.st.get();
        if (st >> 32) as usize == self.envs[0].bindings.len() {
            self.heap.cell_get(self.global, st as u32, cell.key)
        } else if (st >> 32) as u32 == GC_LEX {
            match self.envs[0].bindings.get(st as u32 as usize) {
                Some(b) if b.flags & INIT != 0 => Some(b.value),
                _ => None,
            }
        } else {
            None
        }
    }

    /// `SetGlobal` when environment 0 has bindings: `true` if the cell stored the value.
    #[inline(never)]
    fn bc_global_put(&mut self, cell: &GCell, v: Value) -> bool {
        let st = cell.st.get();
        if (st >> 32) as usize == self.envs[0].bindings.len() {
            self.heap.cell_set(self.global, st as u32, cell.key, v)
        } else if (st >> 32) as u32 == GC_LEX {
            match self.envs[0].bindings.get_mut(st as u32 as usize) {
                Some(b) if b.flags & (INIT | MUT) == INIT | MUT => {
                    b.value = v;
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    /// Keep the position of a global property in its cell, after a slow path.
    fn bc_learn(&mut self, cell: &GCell) {
        cell.st.set(match self.heap.own_pos(self.global, cell.key) {
            Some(p) => (self.envs[0].bindings.len() as u64) << 32 | p as u64,
            None => GC_NONE,
        });
    }

    #[inline(never)]
    fn bc_set_global(&mut self, n: u32, v: Value, cell: &GCell) -> Res<()> {
        if !self.envs[0].bindings.is_empty()
            && let Some(i) = self.find_in(0, n)
        {
            cell.st.set((GC_LEX as u64) << 32 | i as u64);
            return self.bc_assign(0, i, v);
        }
        self.assign_global(n, v)?;
        self.bc_learn(cell);
        Ok(())
    }

    fn bc_assign(&mut self, e: u32, i: usize, v: Value) -> Res<()> {
        let b = &mut self.envs[e as usize].bindings[i];
        if b.flags & (INIT | MUT) == INIT | MUT {
            b.value = v;
            return Ok(());
        }
        let n = b.name;
        self.assign_name_refused(n, e, i, v)
    }

    /// `GetName`: the full search, then the global object.
    #[inline(never)]
    fn bc_get_name(&mut self, n: u32) -> Res<Value> {
        match self.lookup(n) {
            Some((e, i)) => {
                let b = self.envs[e as usize].bindings[i];
                if b.flags & INIT != 0 { Ok(b.value) } else { Err(self.tdz(n)) }
            }
            None => self.get_global(n),
        }
    }

    #[inline(never)]
    fn bc_set_name(&mut self, n: u32, v: Value) -> Res<()> {
        match self.lookup(n) {
            Some((e, i)) => self.bc_assign(e, i, v),
            None => self.assign_global(n, v),
        }
    }

    /// `TypeofGlobal` and `TypeofName` (`eval_unary`): a name that nothing binds gives "undefined".
    #[inline(never)]
    fn bc_typeof_name(&mut self, n: u32, full: bool) -> Res<Value> {
        let found = if full { self.lookup(n) } else if self.envs[0].bindings.is_empty() { None } else { self.find_in(0, n).map(|i| (0, i)) };
        let v = match found {
            Some((e, i)) => {
                let b = self.envs[e as usize].bindings[i];
                if b.flags & INIT == 0 {
                    return Err(self.tdz(n));
                }
                b.value
            }
            None => {
                let key = self.name_key(n)?;
                if !hx!(self, self.heap.has_property_lazy(self.global, key)) && (self.hs.scope.is_empty() || self.hscope_find(n)?.is_none()) {
                    return Ok(Value::String(h!(self, self.heap.intern_str("undefined"))));
                }
                self.get_global(n)?
            }
        };
        let t = self.type_of(v);
        Ok(Value::String(h!(self, self.heap.intern_str(t))))
    }

    /// `GetField` when the inline own hit failed (`member_ic`).
    #[inline(never)]
    fn bc_get_field(&mut self, base: Value, n: u32, ic: &std::cell::Cell<Ic>) -> Res<Value> {
        #[cfg(feature = "bc-hist")]
        {
            self.bc.hist.miss[0] += 1;
        }
        let key = self.name_key(n)?;
        let Value::Object(o) = base else { return self.get_prop(base, key) };
        let got = if self.heap.ic_refuses(o) {
            self.heap.get_refused(o, key)
        } else {
            let mut e = ic.get();
            let r = self.heap.get_ic(o, key, &mut e);
            ic.set(e);
            r
        };
        let got = match got {
            Ok(g) => g,
            Err(err) => self.get_miss(o, key, base, err)?,
        };
        match got {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), base, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// `SetField` when the inline store hit failed (`member_put_ic`).
    #[inline(never)]
    fn bc_set_field(&mut self, base: Value, n: u32, v: Value, ic: &std::cell::Cell<Ic>) -> Res<()> {
        #[cfg(feature = "bc-hist")]
        {
            self.bc.hist.miss[1] += 1;
        }
        let key = self.name_key(n)?;
        let Value::Object(o) = base else { return self.put_prop(base, key, v) };
        if ic.get().is_mega() || self.heap.ic_refuses(o) || self.heap.has_exotics() && self.heap.is_view_index_key(o, key) {
            return self.put_prop(base, key, v);
        }
        let mut e = ic.get();
        let r = self.heap.set_ic(o, key, v, &mut e);
        ic.set(e);
        let r = match r {
            Ok(r) => r,
            Err(err) => self.set_miss(o, key, v, base, err)?,
        };
        self.put_done(base, key, v, r)
    }

    /// `GetElem`: `b[c]` (`link_inner`, `Index`).
    #[inline(never)]
    fn bc_get_elem(&mut self, base: Value, iv: Value) -> Res<Value> {
        if is_nullish(base) {
            return Err(self.index_nullish_err(base, iv));
        }
        let key = self.coerce_key(iv)?;
        self.get_prop(base, key)
    }

    /// `ToKey`: a primitive key stays as it is (its property key is the same later); an object becomes a string or a symbol.
    #[inline(never)]
    fn bc_to_key(&mut self, base: Value, iv: Value) -> Res<Value> {
        if is_nullish(base) {
            return Err(self.index_nullish_err(base, iv));
        }
        if !matches!(iv, Value::Object(_)) {
            return Ok(iv);
        }
        Ok(match self.coerce_key(iv)? {
            PropertyKey::String(s) => Value::String(s),
            PropertyKey::Symbol(s) => Value::Symbol(s),
            PropertyKey::Index(i) => Value::Number(i as f64),
        })
    }

    /// The call or `new` node of the `Call` or `New` at word `pc`.
    fn bc_call_expr(c: &Compiled, pc: usize) -> ExprId {
        let i = c.code.calls.partition_point(|&(w, _)| (w as usize) < pc);
        ExprId::from_index(c.code.calls[i].1 as usize)
    }

    /// `Call` and `New`: the callee in register `b`, then `this` (a call) and the arguments.
    #[inline(never)]
    fn bc_call(&mut self, c: &Compiled, pc: usize, regs: &[Value], b: usize, n: usize, new: bool) -> Res<Value> {
        let f = regs[b];
        let first = b + if new { 1 } else { 2 };
        let mut argv = self.arg_pool.pop().unwrap_or_default();
        argv.clear();
        argv.extend_from_slice(&regs[first..first + n]);
        // `CallR` holds the position of its call: a callable callee needs no table of the script.
        let r = if c.code.ops[pc] as u8 == CALL_R && self.is_callable(f) {
            self.stk.pos = c.code.ops[pc + 1] & 0x7fff_ffff;
            self.call_fn(f, regs[b + 1], &argv)
        } else {
            self.bc_sync(c);
            let e = Self::bc_call_expr(c, pc);
            self.mark_call(e);
            let callee = match self.prog.expr(e).kind {
                ExprKind::Call { callee, .. } | ExprKind::New { callee, .. } => callee,
                _ => e,
            };
            if new {
                self.construct(f, &argv, callee)
            } else if self.is_callable(f) {
                self.call_fn(f, regs[b + 1], &argv)
            } else {
                Err(self.not_a_function(callee))
            }
        };
        self.stk.pos = 0;
        let r = r?;
        self.arg_pool.push(argv);
        Ok(r)
    }

    /// A `CallR` that does not stay in the loop. The caller's registers are the arguments: no copy. The loop holds `st.regs`, so
    /// the callee cannot change them. A native runs here as `call_fn` and `call_fn_inner` would run it, without a second lookup. A
    /// native that calls script code does it through `call_fn`, which counts and resets the frames. `x`: the second word of the op
    /// (the call position). A value that is not callable goes to `bc_call`, which names it in the error.
    #[inline(never)]
    fn bc_call_r(&mut self, c: &Compiled, pc: usize, regs: &[Value], b: usize, n: usize, x: u32) -> Res<Value> {
        let Value::Object(o) = regs[b] else { return self.bc_call(c, pc, regs, b, n, false) };
        let nf = match self.funcs.get(&o) {
            Some(&Callee::Native(nf)) => nf,
            Some(_) => return self.bc_call_fn(regs, b, n, x),
            None => return self.bc_call(c, pc, regs, b, n, false),
        };
        self.stk.pos = x & 0x7fff_ffff;
        let r = self.bc_native(nf, regs[b + 1], &regs[b + 2..b + 2 + n]);
        self.stk.pos = 0;
        r
    }

    /// `bc_call_r` of a callable value that is not a native.
    #[inline(never)]
    fn bc_call_fn(&mut self, regs: &[Value], b: usize, n: usize, x: u32) -> Res<Value> {
        self.stk.pos = x & 0x7fff_ffff;
        let r = self.call_fn(regs[b], regs[b + 1], &regs[b + 2..b + 2 + n]);
        self.stk.pos = 0;
        r
    }

    /// `call_fn` and `call_fn_inner` for a native callee.
    #[inline(always)]
    fn bc_native(&mut self, nf: Nat, this: Value, args: &[Value]) -> Res<Value> {
        if self.frames >= self.limits.max_traversal_depth {
            return Err(Error::Limit(LimitKind::TraversalDepth));
        }
        if self.depth >= self.limits.max_call_depth {
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        self.tick()?;
        self.bc.stats.natives += 1;
        self.frames += 1;
        self.depth += 1;
        let r = self.call_native(nf, this, args, false);
        self.depth -= 1;
        self.frames -= 1;
        r
    }

    /// `Closure` and `ClosureName`.
    #[inline(never)]
    fn bc_closure(&mut self, c: &Compiled, fid: FuncId, name: Option<u32>) -> Res<Value> {
        self.bc_sync(c);
        let f = *self.prog.func(fid);
        // Only a named plain function expression gets its own name binding: ask the scope whether the node is an expression. A lazy
        // function has no scope yet: its slot keeps what the first walk saw.
        let is_expr = name.is_some()
            || f.name.is_some()
                && f.kind == FuncKind::Normal
                && match f.body {
                    Body::Lazy(idx) => self.lazy_is_expr(idx),
                    _ => self.bc_fn_scope(fid).is_some_and(|s| matches!(self.sc_all[s.scope as usize].kind, RKind::Fn(_, true))),
                };
        let s = match name {
            Some(n) => Some(self.name_str(n)?),
            None => None,
        };
        self.make_closure(fid, s, is_expr)
    }

    #[inline(never)]
    fn bc_concat(&mut self, regs: &[Value]) -> Res<Value> {
        let mut out: Vec<u16> = Vec::new();
        for &r in regs {
            let Value::String(s) = r else { return Err(unsupported("a template part that is not a string", Span::default())) };
            self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
        }
        Ok(Value::String(h!(self, self.heap.intern_vec(out))))
    }

    #[inline(never)]
    fn bc_unary(&mut self, op: u8, v: Value) -> Res<Value> {
        match op {
            NEG => match self.to_numeric(v)? {
                Value::Number(n) => Ok(Value::Number(-n)),
                big => self.big_neg(big),
            },
            PLUS => Ok(Value::Number(self.coerce_number(v)?)),
            BIT_NOT => match self.to_numeric(v)? {
                Value::Number(n) => Ok(Value::Number(!to_int32(n) as f64)),
                big => self.big_not(big),
            },
            TYPEOF => {
                let t = self.type_of(v);
                Ok(Value::String(h!(self, self.heap.intern_str(t))))
            }
            TO_NUMERIC => self.to_numeric(v),
            TO_STR => Ok(Value::String(self.coerce_string(v)?)),
            // `Inc` and `Dec` of a value that is not a number.
            _ => match self.to_numeric(v)? {
                Value::Number(n) => Ok(Value::Number(if op == INC { n + 1.0 } else { n - 1.0 })),
                big => self.big_step(big, op == INC),
            },
        }
    }

    #[inline(never)]
    fn bc_array_push(&mut self, a: Value, v: Value) -> Res<()> {
        let Value::Object(o) = a else { return Err(unsupported("ArrayPush on a value that is not an array", Span::default())) };
        let n = h!(self, self.heap.array_length(o)).unwrap_or(0);
        self.def(o, PropertyKey::Index(n), v, true, true, true)
    }

    #[inline(never)]
    fn bc_def_field(&mut self, o: Value, n: u32, v: Value) -> Res<()> {
        let Value::Object(o) = o else { return Err(unsupported("DefField on a value that is not an object", Span::default())) };
        let key = self.name_key(n)?;
        self.def(o, key, v, true, true, true)
    }

    #[inline(never)]
    fn bc_this(&mut self) -> Res<Value> {
        self.this_binding()
    }

    /// The loop. `depth` counts the environments that the code pushed and did not pop. The verifier checked the code, and
    /// `bc_finish` refused wide forms, so every word is a narrow instruction.
    ///
    /// Stage 2: a `Call` of a register function from a register function does not leave the loop. It pushes the caller on
    /// `st.frames`, puts the callee's registers on `st.regs` from the argument registers of the caller, and goes on. A `Ret`
    /// pops it. An error that no handler of the callee takes goes to the handler of the caller, and so on up. Every exit of the
    /// loop leaves `st.frames` empty and `depth`, `strict`, `env` and the call stack as the entry had them (`bc_inline_exit`).
    fn bc_loop(&mut self, code0: &Rc<Compiled>, st: &mut RegStack, depth: &mut u32, this0: This) -> Res<Value> {
        let mut code = code0.clone();
        let mut c: &Compiled = &code;
        let mut ops: &[u32] = &c.code.ops[..];
        let mut this = this0;
        let mut base = 0usize;
        let mut regs: &mut [Value] = &mut st.regs[..c.code.nregs as usize];
        let mut pc = 0usize;
        // The first iterator of the running frame on `st.iters`. A run starts with none.
        debug_assert!(st.iters.is_empty());
        let mut ibase = 0usize;
        'run: loop {
            let at = pc;
            let w = ops[pc];
            pc += 1;
            let a = (w >> 8 & 255) as usize;
            let b = (w >> 16 & 255) as usize;
            let cc = (w >> 24) as usize;
            // `Abx`, `AsBx` and `Sj` operands.
            let bx = w >> 16;
            let sbx = w as i32 >> 16;
            #[cfg(feature = "bc-hist")]
            self.bc.hist.step(w, at, c);
            // Poison mode: code of another script runs with the current script parked (`select_parked`).
            #[cfg(any(test, feature = "bc-poison"))]
            if self.cur != c.script as usize {
                self.select(PARKED);
            }
            let step: Result<(), Error> = 'op: {
                // The value of a fallible step, or the exit from this instruction with its error.
                macro_rules! tri {
                    ($at:expr, $e:expr) => {
                        match $e {
                            Ok(v) => v,
                            Err(err) => break 'op Err(err),
                        }
                    };
                }
                macro_rules! name {
                    ($k:expr) => {
                        match c.code.consts[$k as usize] {
                            Const::Name(n) => n,
                            _ => unreachable!("the verifier checks name operands"),
                        }
                    };
                }
                // `GetEnv`, `SetEnv` and `InitEnv`: register `$a`, binding `$i` of the environment `$h` levels up.
                macro_rules! get_env {
                    ($a:expr, $h:expr, $i:expr) => {{
                        let (e, i) = self.bc_slot($h as u32, $i as u32);
                        regs[$a] = match self.envs[e as usize].bindings.get(i) {
                            Some(bd) if bd.flags & INIT != 0 => bd.value,
                            _ => tri!(at, Err::<Value, _>(self.bc_env_err(e, i))),
                        };
                    }};
                }
                macro_rules! set_env {
                    ($a:expr, $h:expr, $i:expr) => {{
                        let (e, i) = self.bc_slot($h as u32, $i as u32);
                        let v = regs[$a];
                        match self.envs[e as usize].bindings.get_mut(i) {
                            Some(bd) if bd.flags & (INIT | MUT) == INIT | MUT => bd.value = v,
                            Some(_) => tri!(at, self.bc_assign(e, i, v)),
                            None => tri!(at, Err::<(), _>(self.bc_env_err(e, i))),
                        }
                    }};
                }
                macro_rules! init_env {
                    ($a:expr, $h:expr, $i:expr) => {{
                        let (e, i) = self.bc_slot($h as u32, $i as u32);
                        let v = regs[$a];
                        match self.envs[e as usize].bindings.get_mut(i) {
                            Some(bd) => {
                                bd.value = v;
                                bd.flags |= INIT;
                            }
                            None => tri!(at, Err::<(), _>(self.bc_env_err(e, i))),
                        }
                    }};
                }
                // `a = f(l, r)` on two numbers, else the general operator.
                macro_rules! arith {
                    ($l:expr, $r:expr, $f:expr, $bop:expr) => {{
                        let (l, r) = ($l, $r);
                        regs[a] = match (l, r) {
                            (Value::Number(p), Value::Number(q)) => Value::Number($f(p, q)),
                            _ => tri!(at, self.binary($bop, l, r)),
                        };
                    }};
                }
                macro_rules! cmp {
                    ($l:expr, $r:expr, $f:expr, $bop:expr) => {{
                        let (l, r) = ($l, $r);
                        regs[a] = match (l, r) {
                            (Value::Number(p), Value::Number(q)) => Value::Bool($f(p, q)),
                            _ => tri!(at, self.binary($bop, l, r)),
                        };
                    }};
                }
                // The fused test and jump: jump when the comparison of `regs[a]` and the right side is false.
                macro_rules! jn {
                    ($r:expr, $f:expr, $bop:expr) => {{
                        let x = ops[pc];
                        pc += 1;
                        let (l, r) = (regs[a], $r);
                        let t = match (l, r) {
                            (Value::Number(p), Value::Number(q)) => $f(p, q),
                            _ => matches!(tri!(at, self.binary($bop, l, r)), Value::Bool(true)),
                        };
                        if !t {
                            pc = (pc as isize + x as i32 as isize) as usize;
                        }
                    }};
                }
                macro_rules! jn_strict {
                    ($r:expr, $want:expr) => {{
                        let x = ops[pc];
                        pc += 1;
                        let (l, r) = (regs[a], $r);
                        let t = match (l, r) {
                            (Value::Number(p), Value::Number(q)) => (p == q) == $want,
                            _ => self.heap.strict_equals(l, r) == $want,
                        };
                        if !t {
                            pc = (pc as isize + x as i32 as isize) as usize;
                        }
                    }};
                }
                // `Ret`: back to the caller of an inline call, or out of the loop.
                macro_rules! ret {
                    ($v:expr) => {{
                        let v = $v;
                        let Some(fr) = st.frames.pop() else { return Ok(v) };
                        self.bc_frame_exit(&fr, true, depth);
                        ibase = fr.iters as usize;
                        code = fr.code;
                        c = &code;
                        ops = &c.code.ops[..];
                        this = fr.this;
                        base = fr.base as usize;
                        pc = fr.pc as usize;
                        self.strict = c.code.strict;
                        regs = &mut st.regs[base..base + c.code.nregs as usize];
                        regs[fr.dst as usize] = v;
                    }};
                }
                match w as u8 {
                    NOP => {}
                    MOV => regs[a] = regs[b],
                    LOAD_UNDEF => regs[a] = Value::Undefined,
                    LOAD_NULL => regs[a] = Value::Null,
                    LOAD_TRUE => regs[a] = Value::Bool(true),
                    LOAD_FALSE => regs[a] = Value::Bool(false),
                    LOAD_THIS => {
                        regs[a] = match this {
                            This::Fixed(t) => t,
                            This::Dynamic => tri!(at, self.bc_this()),
                        }
                    }
                    LOAD_I => regs[a] = Value::Number(sbx as f64),
                    LOAD_K => regs[a] = c.kv[bx as usize],
                    GET_ENV => get_env!(a, b, cc),
                    SET_ENV => set_env!(a, b, cc),
                    INIT_ENV => init_env!(a, b, cc),
                    // The wide form of the words that name a binding or a constant past 255 (the many bindings of a webpack module
                    // function, an object literal with many keys). `bc_finish` refuses any other wide word.
                    WIDE => {
                        let w2 = ops[pc];
                        pc += 1;
                        let (a, b, cc) = ((w >> 16) as usize, (w2 & 0xffff) as usize, (w2 >> 16) as usize);
                        match (w >> 8) as u8 {
                            GET_ENV => get_env!(a, b, cc),
                            SET_ENV => set_env!(a, b, cc),
                            INIT_ENV => init_env!(a, b, cc),
                            DEF_FIELD => tri!(at, self.bc_def_field(regs[a], name!(cc), regs[b])),
                            _ => unreachable!("bc_finish refuses this wide word"),
                        }
                    }
                    PUSH_ENV => {
                        tri!(at, self.bc_declare(c, bx, true));
                        *depth += 1;
                    }
                    DECL_ENV => tri!(at, self.bc_declare(c, bx, false)),
                    POP_ENV => {
                        let e = self.env;
                        self.env = self.envs[e as usize].parent;
                        self.release(e);
                        *depth -= 1;
                    }
                    COPY_ENV => {
                        if self.envs[self.env as usize].captured {
                            tri!(at, self.copy_env());
                        }
                    }
                    // A cell hit is what `get_global` and `assign_global` find first. A lexical binding of environment 0 shadows the cells.
                    GET_GLOBAL => {
                        let cell = &c.cells[bx as usize];
                        let hit = if self.envs[0].bindings.is_empty() { self.heap.cell_get(self.global, cell.st.get() as u32, cell.key) } else { self.bc_global_hit(cell) };
                        regs[a] = match hit {
                            Some(v) => v,
                            None => tri!(at, self.bc_get_global(name!(bx), cell)),
                        };
                    }
                    SET_GLOBAL => {
                        let cell = &c.cells[bx as usize];
                        let v = regs[a];
                        let done = if self.envs[0].bindings.is_empty() { self.heap.cell_set(self.global, cell.st.get() as u32, cell.key, v) } else { self.bc_global_put(cell, v) };
                        if !done {
                            tri!(at, self.bc_set_global(name!(bx), v, cell));
                        }
                    }
                    TYPEOF_GLOBAL => regs[a] = tri!(at, self.bc_typeof_name(name!(bx), false)),
                    GET_NAME => regs[a] = tri!(at, self.bc_get_name(name!(bx))),
                    SET_NAME => tri!(at, self.bc_set_name(name!(bx), regs[a])),
                    TYPEOF_NAME => regs[a] = tri!(at, self.bc_typeof_name(name!(bx), true)),
                    GET_FIELD => {
                        let x = ops[pc];
                        pc += 1;
                        let base_v = regs[b];
                        let ic = &c.code.ics[cc];
                        let hit = if let Value::Object(o) = base_v { self.heap.ic_own(o, &ic.get()) } else { None };
                        regs[a] = match hit {
                            Some(v) => v,
                            None => tri!(at, self.bc_get_field(base_v, name!(x), ic)),
                        };
                    }
                    SET_FIELD => {
                        let x = ops[pc];
                        pc += 1;
                        let (o, v) = (regs[a], regs[b]);
                        let ic = &c.code.ics[cc];
                        let e = ic.get();
                        if !matches!(o, Value::Object(ob) if self.heap.ic_store(ob, &e, v) || matches!(self.heap.ic_add(ob, &e, v), Ok(true))) {
                            tri!(at, self.bc_set_field(o, name!(x), v, ic));
                        }
                    }
                    GET_ELEM => regs[a] = tri!(at, self.bc_get_elem(regs[b], regs[cc])),
                    SET_ELEM => {
                        let (o, k, v) = (regs[a], regs[b], regs[cc]);
                        let key = tri!(at, self.coerce_key(k));
                        tri!(at, self.put_prop(o, key, v));
                    }
                    TO_KEY => regs[a] = tri!(at, self.bc_to_key(regs[b], regs[cc])),
                    DEF_FIELD => tri!(at, self.bc_def_field(regs[a], name!(cc), regs[b])),
                    ARRAY_PUSH => tri!(at, self.bc_array_push(regs[a], regs[b])),
                    NEW_OBJECT => regs[a] = Value::Object(tri!(at, self.heap.new_object(Some(self.object_proto)).map_err(|e| self.herr(e)))),
                    NEW_ARRAY => regs[a] = Value::Object(tri!(at, self.heap.new_array(Some(self.array_proto)).map_err(|e| self.herr(e)))),
                    CLOSURE => regs[a] = tri!(at, self.bc_closure(c, FuncId::from_index(bx as usize), None)),
                    CLOSURE_NAME => {
                        let x = ops[pc];
                        pc += 1;
                        regs[a] = tri!(at, self.bc_closure(c, FuncId::from_index(x as usize), Some(name!(b))));
                    }
                    CALL => regs[a] = tri!(at, self.bc_call(c, at, regs, b, cc, false)),
                    CALL_R => {
                        let x = ops[pc];
                        pc += 1;
                        // `x` bit 31: no object, so `this` is `undefined`.
                        if x >> 31 != 0 {
                            regs[b + 1] = Value::Undefined;
                        }
                        let Some((callee, fid, cenv, fobj)) = self.bc_target(regs[b]) else {
                            regs[a] = tri!(at, self.bc_call_r(c, at, regs, b, cc, x));
                            continue 'run;
                        };
                        // What `call_fn_inner` and `call_script` do for a call, without the Rust frames.
                        if self.depth >= self.limits.max_call_depth {
                            break 'op Err(Error::Limit(LimitKind::CallDepth));
                        }
                        tri!(at, self.tick());
                        self.bc.stats.calls_lane += 1;
                        let k = &callee.code;
                        let (nb, nregs, this_v) = (base + b + 2, k.nregs as usize, regs[b + 1]);
                        // Every step that can fail comes before the first use of `st.regs`.
                        let (t, caller_env, callee_env, from, to);
                        if k.regfn && !k.has_env {
                            // A register function with no environment: the closure's environment is where its outer names come from.
                            t = if k.dyn_this {
                                This::Dynamic
                            } else if k.uses_this {
                                This::Fixed(tri!(at, self.bc_bind_this(this_v, k.strict)))
                            } else {
                                This::Fixed(Value::Undefined)
                            };
                            caller_env = std::mem::replace(&mut self.env, cenv);
                            callee_env = u32::MAX;
                            (from, to) = (cc.min(k.nparams as usize), k.nlocals as usize);
                            self.stk.frames.push(StackFrame { script: callee.script, at: Where::Func(fid), new: false, saved_pos: x & 0x7fff_ffff });
                            self.stk.pos = 0;
                        } else {
                            (t, caller_env, callee_env, from, to) = tri!(at, self.bc_call_env(&callee, (fid, fobj, cenv), this_v, &regs[b + 2..b + 2 + cc], x & 0x7fff_ffff));
                        }
                        if st.regs.len() < nb + nregs {
                            st.grow(nb + nregs);
                        }
                        // The arguments are the first registers of the callee already. The other variables start as `undefined`.
                        if from < to {
                            st.regs[nb + from..nb + to].fill(Value::Undefined);
                        }
                        self.depth += 1;
                        self.strict = k.strict;
                        st.frames.push(Fr { code: std::mem::replace(&mut code, callee), pc: pc as u32, base: base as u32, dst: a as u32, env: caller_env, this, depth: std::mem::take(depth), iters: ibase as u32, callee_env });
                        ibase = st.iters.len();
                        c = &code;
                        ops = &c.code.ops[..];
                        this = t;
                        base = nb;
                        pc = 0;
                        regs = &mut st.regs[nb..nb + nregs];
                    }
                    NEW => regs[a] = tri!(at, self.bc_call(c, at, regs, b, cc, true)),
                    ADD => arith!(regs[b], regs[cc], |p: f64, q: f64| p + q, BinaryOp::Add),
                    SUB => arith!(regs[b], regs[cc], |p: f64, q: f64| p - q, BinaryOp::Sub),
                    MUL => arith!(regs[b], regs[cc], |p: f64, q: f64| p * q, BinaryOp::Mul),
                    DIV => arith!(regs[b], regs[cc], |p: f64, q: f64| p / q, BinaryOp::Div),
                    MOD => arith!(regs[b], regs[cc], |p: f64, q: f64| p % q, BinaryOp::Mod),
                    ADD_K => arith!(regs[b], c.kv[cc], |p: f64, q: f64| p + q, BinaryOp::Add),
                    SUB_K => arith!(regs[b], c.kv[cc], |p: f64, q: f64| p - q, BinaryOp::Sub),
                    MUL_K => arith!(regs[b], c.kv[cc], |p: f64, q: f64| p * q, BinaryOp::Mul),
                    DIV_K => arith!(regs[b], c.kv[cc], |p: f64, q: f64| p / q, BinaryOp::Div),
                    MOD_K => arith!(regs[b], c.kv[cc], |p: f64, q: f64| p % q, BinaryOp::Mod),
                    LT => cmp!(regs[b], regs[cc], |p: f64, q: f64| p < q, BinaryOp::Lt),
                    LE => cmp!(regs[b], regs[cc], |p: f64, q: f64| p <= q, BinaryOp::Le),
                    GT => cmp!(regs[b], regs[cc], |p: f64, q: f64| p > q, BinaryOp::Gt),
                    GE => cmp!(regs[b], regs[cc], |p: f64, q: f64| p >= q, BinaryOp::Ge),
                    EQ => cmp!(regs[b], regs[cc], |p: f64, q: f64| p == q, BinaryOp::Eq),
                    NE => cmp!(regs[b], regs[cc], |p: f64, q: f64| p != q, BinaryOp::Ne),
                    STRICT_EQ => regs[a] = Value::Bool(self.heap.strict_equals(regs[b], regs[cc])),
                    STRICT_NE => regs[a] = Value::Bool(!self.heap.strict_equals(regs[b], regs[cc])),
                    JN_LT => jn!(regs[b], |p: f64, q: f64| p < q, BinaryOp::Lt),
                    JN_LE => jn!(regs[b], |p: f64, q: f64| p <= q, BinaryOp::Le),
                    JN_GT => jn!(regs[b], |p: f64, q: f64| p > q, BinaryOp::Gt),
                    JN_GE => jn!(regs[b], |p: f64, q: f64| p >= q, BinaryOp::Ge),
                    JN_EQ => jn!(regs[b], |p: f64, q: f64| p == q, BinaryOp::Eq),
                    JN_NE => jn!(regs[b], |p: f64, q: f64| p != q, BinaryOp::Ne),
                    JN_SEQ => jn_strict!(regs[b], true),
                    JN_SNE => jn_strict!(regs[b], false),
                    JN_LT_K => jn!(c.kv[b], |p: f64, q: f64| p < q, BinaryOp::Lt),
                    JN_LE_K => jn!(c.kv[b], |p: f64, q: f64| p <= q, BinaryOp::Le),
                    JN_GT_K => jn!(c.kv[b], |p: f64, q: f64| p > q, BinaryOp::Gt),
                    JN_GE_K => jn!(c.kv[b], |p: f64, q: f64| p >= q, BinaryOp::Ge),
                    JN_EQ_K => jn!(c.kv[b], |p: f64, q: f64| p == q, BinaryOp::Eq),
                    JN_NE_K => jn!(c.kv[b], |p: f64, q: f64| p != q, BinaryOp::Ne),
                    JN_SEQ_K => jn_strict!(c.kv[b], true),
                    JN_SNE_K => jn_strict!(c.kv[b], false),
                    op @ (EXP | BIT_AND | BIT_OR | BIT_XOR | SHL | SHR | USHR | IN | INSTANCE_OF) => regs[a] = tri!(at, self.bc_binary(op, regs[b], regs[cc])),
                    NOT => {
                        let v = regs[b];
                        let t = match v {
                            Value::Bool(t) => t,
                            _ => tri!(at, self.truthy(v)),
                        };
                        regs[a] = Value::Bool(!t);
                    }
                    op @ (INC | DEC) => {
                        regs[a] = match regs[b] {
                            Value::Number(n) => Value::Number(if op == INC { n + 1.0 } else { n - 1.0 }),
                            v => tri!(at, self.bc_unary(op, v)),
                        };
                    }
                    TO_NUMERIC => {
                        let v = regs[b];
                        regs[a] = if matches!(v, Value::Number(_)) { v } else { tri!(at, self.bc_unary(TO_NUMERIC, v)) };
                    }
                    op @ (NEG | PLUS | BIT_NOT | TYPEOF | TO_STR) => regs[a] = tri!(at, self.bc_unary(op, regs[b])),
                    CONCAT => regs[a] = tri!(at, self.bc_concat(&regs[b..b + cc])),
                    JMP => {
                        let off = w as i32 >> 8;
                        if off < 0 {
                            tri!(at, self.tick());
                        }
                        pc = (pc as isize + off as isize) as usize;
                    }
                    op @ (JMP_IF | JMP_IF_NOT | JMP_NULLISH | JMP_NOT_NULLISH) => {
                        let v = regs[a];
                        let on = match op {
                            JMP_IF | JMP_IF_NOT => {
                                let t = match v {
                                    Value::Bool(t) => t,
                                    _ => tri!(at, self.truthy(v)),
                                };
                                t == (op == JMP_IF)
                            }
                            JMP_NULLISH => is_nullish(v),
                            _ => !is_nullish(v),
                        };
                        if on {
                            if sbx < 0 {
                                tri!(at, self.tick());
                            }
                            pc = (pc as isize + sbx as isize) as usize;
                        }
                    }
                    ITER_OPEN => {
                        let it = tri!(at, self.bc_iter_open(c, at, regs[a], b == 1));
                        st.iters.push(BcIter::Of(it));
                    }
                    FOR_IN_OPEN => {
                        let obj = regs[a];
                        let (keys, proxied) = tri!(at, self.for_in_keys(obj));
                        st.iters.push(BcIter::In { obj, keys, i: 0, proxied });
                    }
                    ITER_NEXT => {
                        let next = match st.iters.last_mut() {
                            Some(BcIter::Of(it)) => self.iter_next(it),
                            Some(BcIter::In { obj, keys, i, proxied }) => self.bc_for_in_next(*obj, keys, i, *proxied),
                            None => break 'op Err(unsupported("bad bytecode", Span::default())),
                        };
                        match tri!(at, next) {
                            Some(v) => regs[a] = v,
                            None => {
                                st.iters.pop();
                                pc = (pc as isize + sbx as isize) as usize;
                            }
                        }
                    }
                    op @ (ITER_STEP | ITER_REST | OBJ_REST | REQUIRE_OBJ) => tri!(at, self.bc_pattern_op(op, a, b, cc, regs, &mut st.iters)),
                    ITER_CLOSE => match st.iters.pop() {
                        Some(BcIter::Of(mut it)) => tri!(at, self.iter_close(&mut it)),
                        Some(BcIter::In { .. }) => {}
                        None => break 'op Err(unsupported("bad bytecode", Span::default())),
                    },
                    RET => ret!(regs[a]),
                    RET_UNDEF => ret!(Value::Undefined),
                    THROW => tri!(at, Err::<(), _>(Error::Throw(regs[a]))),
                    // Every op without an arm here is one cold call: it must not change the code of the hot arms.
                    _ => tri!(at, self.bc_rare(c, at, w, regs)),
                }
                Ok(())
            };
            if let Err(err) = step {
                // The handler of the running function, else the caller's (at the word of its call), and so on up. Any error that
                // is not a thrown value leaves the whole loop.
                let (mut err, mut fault) = (err, at);
                loop {
                    match self.bc_catch(c, fault, depth, &mut st.iters, ibase, err) {
                        Ok((t, r, v)) => {
                            regs[r] = v;
                            pc = t;
                            break;
                        }
                        Err(e) => {
                            // No handler in this frame: its loops close before the caller's handler runs.
                            let e = self.bc_close_iters(&mut st.iters, ibase, e);
                            if st.frames.is_empty() || !matches!(e, Error::Throw(_)) {
                                let e = self.bc_close_iters(&mut st.iters, 0, e);
                                while let Some(fr) = st.frames.pop() {
                                    self.bc_frame_exit(&fr, false, depth);
                                }
                                return Err(e);
                            }
                            let Some(fr) = st.frames.pop() else { return Err(e) };
                            self.bc_frame_exit(&fr, false, depth);
                            ibase = fr.iters as usize;
                            code = fr.code;
                            c = &code;
                            ops = &c.code.ops[..];
                            this = fr.this;
                            base = fr.base as usize;
                            self.strict = c.code.strict;
                            regs = &mut st.regs[base..base + c.code.nregs as usize];
                            fault = fr.pc as usize - 2;
                            err = e;
                        }
                    }
                }
            }
        }
    }
}

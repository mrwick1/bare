//! The code generator, core subset (step B2 of `docs/engine-plan.md`): the syntax tree of one function to bytecode (`bc.rs`).
//! A child module of `eval`, so it reads the tables of the tree walker. Nothing runs the code until step B3.
//!
//! The contract of stage 1 (see `bc_scope.rs`): the code of a function runs in the environment that `bind_call` made (`this`,
//! the parameters, `arguments` and the `var` names). Its first op adds the `let`, `const` and function names of the body
//! (`DeclEnv`). A block, a `switch`, a `for` with `let` or `const` and a `catch` push one environment each, as the tree walker
//! does, so every name reads its binding by (hops, index) from `bc_resolve`. A name behind a class or named `arguments` uses the
//! full search (`GetName`).
//!
//! Registers are a stack: an expression writes its value into the register that the caller gives, and its temporaries are above.
//! No local lives in a register in stage 1.
//!
//! `finally` uses two registers and ordinary jumps (`docs/decisions.md`, "Bytecode"): `kind` and `val`. The end of the `try` or
//! `catch` block sets `kind` to 0, the handler sets it to 1 (the exception is in `val`), and a `break`, `continue` or `return` that
//! leaves the block sets it to 2 + the number of that exit (a return value is in `val`). The `finally` block ends with a test of
//! `kind` for each exit.
//!
//! A node outside the subset, more than 255 registers or a jump that does not fit in one word gives an error: the function stays on
//! the tree walker. The error names the reason, for the coverage count (`bc_gen_check`).
// Until the loop (step B3) runs the code, only the check and the tests call these functions.
#![allow(dead_code)]
use super::bc_scope::{BindKind, Site};
use super::*;
use crate::bc::{self, Code, Const, Handler, Ins, Op};
use crate::heap::Ic;
use std::cell::Cell;
use std::collections::HashMap;

#[path = "bc_gen_expr.rs"]
mod expr;
#[path = "bc_gen_rf.rs"]
mod rf;
#[path = "bc_gen_stmt.rs"]
mod stmt;
#[cfg(test)]
#[path = "bc_gen_tests.rs"]
mod tests;

/// The result of the code generator. The error is the reason a function stays on the tree walker.
pub(super) type G<T> = Result<T, &'static str>;

/// The most registers of a frame in step B2 (`docs/decisions.md`).
const MAX_REGS: u32 = 255;

/// Where a `break` or a `continue` of a loop or a `switch` goes.
struct Jumps {
    /// `false` for a `switch`: a `continue` passes it.
    is_loop: bool,
    /// The environment depth inside the statement, where both jumps land.
    depth: u32,
    /// The number of open `finally` blocks around the statement.
    fins: usize,
    breaks: Vec<usize>,
    /// The word of the `continue` target when it is known, else the jumps to it.
    cont: Option<usize>,
    conts: Vec<usize>,
}

/// An open `try` with a `finally` block (see the module comment).
struct Fin {
    kind: u32,
    val: u32,
    depth: u32,
    /// The jumps to the `finally` block.
    entry: Vec<usize>,
    exits: Vec<Exit>,
}

/// A jump out of statements: to the `break` or `continue` target of `jumps[i]`, or out of the function.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    Break(usize),
    Continue(usize),
    Return,
}

/// A constant of the pool, as a key to find it again.
#[derive(Clone, PartialEq, Eq, Hash)]
enum KeyOf {
    Number(u64),
    String(Box<[u16]>),
    Name(u32),
}

/// A variable of a register function.
#[derive(Clone, Copy)]
struct Local {
    reg: u32,
    /// `false` for a `let` or `const` whose declaration the generator has not reached: a read or write there is a TDZ error, so
    /// the function does not compile as a register function.
    init: bool,
    is_const: bool,
}

/// Stage 2, a register function: no environment. The parameters, the `var`s and the `let`s and `const`s of all its scopes are
/// registers (`bc_scope.rs` gives each scope's bindings in order). The function has no inner function or class, no `with`, no
/// `arguments`, so nothing else can reach them.
struct Rf {
    /// The function scope, and its parent (names that the function does not declare resolve from the parent: the function pushes
    /// no environment, so the environment of its closure is hop 0).
    scope: u32,
    outer: u32,
    locals: HashMap<(u32, u32), Local>,
    uses_this: bool,
}

pub(super) struct Gen<'g, 'p> {
    it: &'g mut Interp<'p>,
    prog: Rc<Program<'static>>,
    code: Code,
    pool: HashMap<KeyOf, u32>,
    ics: u32,
    /// The first free register, and the most registers in use.
    top: u32,
    max: u32,
    /// The scope of each identifier site of the function, and the scope of each statement that opens one (`bc_walk`).
    sites: HashMap<u32, u32>,
    scopes: HashMap<StmtId, u32>,
    /// The number of environments that the code pushed at this point.
    depth: u32,
    jumps: Vec<Jumps>,
    fins: Vec<Fin>,
    /// The source offset of the node being compiled, for `Code::lines`.
    pos: u32,
    strict: bool,
    /// The completion register of a script body: each expression statement writes it, and `if`, loops, `switch` and `try`
    /// clear it as they start (`enter` and `exec_simple` at depth 0). `None` in a function.
    compl: Option<u32>,
    rf: Option<Rf>,
}

/// The result of `bc_gen_check`.
#[derive(Clone, Debug, Default)]
pub struct GenCheck {
    pub functions: usize,
    pub compiled: usize,
    /// The words of code of the functions that compile.
    pub words: usize,
    /// Why the other functions do not compile: each reason and its count, the largest first.
    pub unsupported: Vec<(&'static str, usize)>,
    /// One line for each function that the verifier rejected. Must be empty.
    pub bad: Vec<String>,
}

impl<'p> Interp<'p> {
    /// Compile function `fid` of the current script. Call it after the script ran (the walker knows the names of environment 0
    /// then).
    pub(super) fn bc_compile(&mut self, fid: FuncId) -> G<Code> {
        let off = self.bc.rf_off;
        let r = self.bc_compile_in(fid);
        if r.is_ok() || off {
            return r;
        }
        // A construct that a register function does not hold (a `switch` with `let`): compile it as a stage 1 function.
        self.bc.rf_off = true;
        let r = self.bc_compile_in(fid);
        self.bc.rf_off = false;
        r
    }

    fn bc_compile_in(&mut self, fid: FuncId) -> G<Code> {
        let f = *self.prog.func(fid);
        if !matches!(f.mode, FuncMode::Plain | FuncMode::Env) {
            return Err("generator or async function");
        }
        let fs = self.bc_fn_scope(fid).ok_or("no scope")?;
        // A name that leaves a `with` body meets its object environment, which the loop does not search: the walker runs it.
        let mut p = self.sc_all[fs.scope as usize].parent;
        while p != NONE {
            if self.sc_all[p as usize].with {
                return Err("a function inside with");
            }
            p = self.sc_all[p as usize].parent;
        }
        let w = self.bc_walk(Some(fid), fs.scope);
        if w.bad.is_some() {
            return Err("scope walk");
        }
        let rf = if !self.bc.rf_off { self.rf_start(fid, &f, &fs) } else { None };
        let (top, nparams) = rf.as_ref().map_or((0u32, 0usize), |(_, n, nl)| (*nl as u32, *n));
        let mut g = Gen {
            prog: self.prog.clone(),
            it: self,
            code: Code::new(0),
            pool: HashMap::new(),
            ics: 0,
            top,
            max: top,
            sites: w.sites.into_iter().collect(),
            scopes: w.stmts.into_iter().collect(),
            depth: 0,
            jumps: Vec::new(),
            fins: Vec::new(),
            pos: f.span.start,
            strict: f.strict,
            compl: None,
            rf: rf.map(|r| r.0),
        };
        match f.body {
            Body::Expr(e) if g.rf.is_some() => g.return_expr(e)?,
            Body::Expr(e) => {
                let r = g.reg()?;
                g.expr(e, r)?;
                g.emit(Op::Ret, r, 0, 0)?;
            }
            Body::Lazy(_) => return Err("a function with a lazy body"),
            Body::Block(l) if g.rf.is_some() => {
                let prog = g.prog.clone();
                for s in prog.list(l) {
                    g.stmt(s)?;
                }
                g.emit(Op::RetUndef, 0, 0, 0)?;
            }
            Body::Block(l) => {
                if g.it.has_lexical(l) {
                    if fs.flags & bc_scope::BF_SELF_NAME != 0 && g.prog.list(l).any(|s| matches!(g.prog.stmt(s).kind, StmtKind::Function(d) if g.it.func_name[d.index()] == g.it.func_name[fid.index()])) {
                        // The declaration takes the flags of the own name (`SELF`), which `InitEnv` does not change.
                        return Err("a function declaration with the name of its function");
                    }
                    g.emit(Op::DeclEnv, 0, fs.scope, 0)?;
                    g.hoist(&[l], fs.scope)?;
                }
                let prog = g.prog.clone();
                for s in prog.list(l) {
                    g.stmt(s)?;
                }
                g.emit(Op::RetUndef, 0, 0, 0)?;
            }
        }
        let mut c = g.code;
        c.nregs = g.max as u16;
        c.ics = (0..g.ics).map(|_| Cell::new(Ic::EMPTY)).collect();
        if let Some(rf) = &g.rf {
            c.regfn = true;
            c.nparams = nparams as u16;
            c.nlocals = top as u16;
            c.uses_this = rf.uses_this;
            c.dyn_this = f.kind == FuncKind::Arrow;
            c.strict = f.strict;
        }
        Ok(c)
    }

    /// Compile the body of the current script. Call it after `declare_scope` ran for it (`exec_current`): the code does not
    /// declare the names of the script. The code returns the completion value.
    pub(super) fn bc_compile_script(&mut self) -> G<Code> {
        let root = self.bc_root().ok_or("no scope")?;
        let w = self.bc_walk(None, root);
        if w.bad.is_some() {
            return Err("scope walk");
        }
        let prog = self.prog.clone();
        let mut g = Gen {
            prog: prog.clone(),
            strict: prog.strict,
            it: self,
            code: Code::new(0),
            pool: HashMap::new(),
            ics: 0,
            top: 0,
            max: 0,
            sites: w.sites.into_iter().collect(),
            scopes: w.stmts.into_iter().collect(),
            depth: 0,
            jumps: Vec::new(),
            fins: Vec::new(),
            pos: 0,
            compl: None,
            rf: None,
        };
        let c = g.reg()?;
        g.compl = Some(c);
        for s in prog.list(prog.body) {
            g.stmt(s)?;
        }
        g.emit(Op::Ret, c, 0, 0)?;
        let mut c = g.code;
        c.nregs = g.max as u16;
        c.ics = (0..g.ics).map(|_| Cell::new(Ic::EMPTY)).collect();
        Ok(c)
    }

    /// Compile and verify every function of every script. Call it after the code ran. The current script is selected at the end.
    #[doc(hidden)]
    pub fn bc_gen_check(&mut self) -> GenCheck {
        let mut r = GenCheck::default();
        let mut why: HashMap<&'static str, usize> = HashMap::new();
        let old = self.cur;
        for id in 0..self.scripts.len() {
            self.select(id);
            let Some(root) = self.bc_root() else { continue };
            let mut todo = self.bc_walk(None, root).funcs;
            while let Some((fid, scope)) = todo.pop() {
                todo.extend(self.bc_walk(Some(fid), scope).funcs);
                r.functions += 1;
                match self.bc_compile(fid) {
                    Ok(code) => match bc_verify::verify(&code) {
                        Ok(()) => {
                            r.compiled += 1;
                            r.words += code.ops.len();
                        }
                        Err(e) => r.bad.push(format!("script {id} function {}: {e}", fid.index())),
                    },
                    Err(e) => *why.entry(e).or_default() += 1,
                }
            }
        }
        self.select(old);
        r.unsupported = why.into_iter().collect();
        r.unsupported.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        r
    }
}

impl Gen<'_, '_> {
    fn emit(&mut self, op: Op, a: u32, b: u32, c: u32) -> G<()> {
        self.emit_ins(Ins::new(op, a, b, c))
    }

    fn emit_ins(&mut self, i: Ins) -> G<()> {
        let w = self.code.ops.len() as u32;
        if self.code.lines.last().is_none_or(|&(_, p)| p != self.pos) {
            self.code.lines.push((w, self.pos));
        }
        bc::encode(&mut self.code.ops, &i).map(|_| ()).map_err(|_| "operand too large")
    }

    /// The next free register.
    fn reg(&mut self) -> G<u32> {
        let r = self.top;
        self.top += 1;
        if self.top > MAX_REGS {
            return Err("more than 255 registers");
        }
        self.max = self.max.max(self.top);
        Ok(r)
    }

    /// Free register `r` and all above it.
    fn free(&mut self, r: u32) {
        self.top = r;
    }

    fn konst(&mut self, k: KeyOf) -> u32 {
        if let Some(&i) = self.pool.get(&k) {
            return i;
        }
        let i = self.code.consts.len() as u32;
        self.code.consts.push(match &k {
            KeyOf::Number(b) => Const::Number(f64::from_bits(*b)),
            KeyOf::String(u) => Const::String(u.clone()),
            KeyOf::Name(n) => Const::Name(*n),
        });
        self.pool.insert(k, i);
        i
    }

    fn name(&mut self, n: u32) -> u32 {
        self.konst(KeyOf::Name(n))
    }

    fn ic(&mut self) -> u32 {
        self.ics += 1;
        self.ics - 1
    }

    fn here(&self) -> usize {
        self.code.ops.len()
    }

    /// A jump forward. `patch` gives it its target later. Gives the word of the jump.
    fn jump(&mut self, op: Op, a: u32) -> G<usize> {
        let at = self.here();
        self.emit(op, if op == Op::Jmp { 0 } else { a }, 0, 0)?;
        Ok(at)
    }

    fn patch_to(&mut self, at: usize, target: usize) -> G<()> {
        let w = self.code.ops[at];
        // A fused compare and jump has two words: the offset is the second, and it counts from the word after it.
        if Op::from_u8(w as u8).is_some_and(|o| o.is_fused_jump()) {
            self.code.ops[at + 1] = (target as i64 - at as i64 - 2) as i32 as u32;
            return Ok(());
        }
        let off = target as i64 - at as i64 - 1;
        self.code.ops[at] = if w as u8 == Op::Jmp as u8 {
            if !(-(1 << 23)..1 << 23).contains(&off) {
                return Err("jump too far");
            }
            w & 0xff | (off as u32) << 8
        } else {
            if off < i16::MIN as i64 || off > i16::MAX as i64 {
                return Err("jump too far");
            }
            w & 0xffff | (off as u32 & 0xffff) << 16
        };
        Ok(())
    }

    /// Point the jump at word `at` to the next word.
    fn patch(&mut self, at: usize) -> G<()> {
        self.patch_to(at, self.here())
    }

    fn jump_back(&mut self, op: Op, a: u32, target: usize) -> G<()> {
        let at = self.jump(op, a)?;
        self.patch_to(at, target)
    }

    /// What the name `n` at identifier site `site` means.
    fn site(&mut self, site: u32, n: u32) -> G<Site> {
        self.site_of(site, n, false)
    }

    /// The same for a site that writes the name: a `const` is not a register that the code may write.
    fn site_w(&mut self, site: u32, n: u32) -> G<Site> {
        self.site_of(site, n, true)
    }

    fn site_of(&mut self, site: u32, n: u32, write: bool) -> G<Site> {
        let sc = *self.sites.get(&site).ok_or("a site that the walk did not visit")?;
        if let Some(rf) = &self.rf {
            return match self.local_at(sc, n) {
                Some((_, l)) if !l.init => Err("a variable used before its declaration"),
                Some((_, l)) if write && l.is_const => Err("assignment to a const"),
                Some((_, l)) => Ok(Site::Reg(l.reg)),
                None if rf.outer == NONE => Ok(Site::Global),
                None => Ok(self.it.bc_resolve(n, rf.outer)),
            };
        }
        Ok(self.it.bc_resolve(n, sc))
    }

    fn load(&mut self, s: Site, n: u32, dst: u32) -> G<()> {
        match s {
            Site::Reg(r) if r == dst => Ok(()),
            Site::Reg(r) => self.emit(Op::Mov, dst, r, 0),
            Site::Env { hops, index } => self.emit(Op::GetEnv, dst, hops, index),
            Site::Global => {
                let k = self.name(n);
                self.emit(Op::GetGlobal, dst, k, 0)
            }
            Site::Dynamic => {
                let k = self.name(n);
                self.emit(Op::GetName, dst, k, 0)
            }
        }
    }

    fn store(&mut self, s: Site, n: u32, src: u32) -> G<()> {
        match s {
            Site::Reg(r) if r == src => Ok(()),
            Site::Reg(r) => self.emit(Op::Mov, r, src, 0),
            Site::Env { hops, index } => self.emit(Op::SetEnv, src, hops, index),
            Site::Global => {
                let k = self.name(n);
                self.emit(Op::SetGlobal, src, k, 0)
            }
            Site::Dynamic => {
                let k = self.name(n);
                self.emit(Op::SetName, src, k, 0)
            }
        }
    }

    /// Make the closures of the function declarations of `lists` (a scope that starts), as `declare_scope` does, in source order.
    fn hoist(&mut self, lists: &[List<Stmt>], scope: u32) -> G<()> {
        let mut binds = Vec::new();
        self.it.bc_binds(scope, &mut binds);
        let prog = self.prog.clone();
        for &l in lists {
            for s in prog.list(l) {
                match prog.stmt(s).kind {
                    StmtKind::Function(f) => {
                        let n = self.it.func_name[f.index()];
                        let i = binds.iter().position(|b| b.name == n && matches!(b.kind, BindKind::Func(_))).ok_or("no binding")?;
                        let r = self.reg()?;
                        self.emit(Op::Closure, r, f.index() as u32, 0)?;
                        self.emit(Op::InitEnv, r, 0, i as u32)?;
                        self.free(r);
                    }
                    StmtKind::Class(_) => return Err("class"),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

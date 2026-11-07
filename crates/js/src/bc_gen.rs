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
        let f = *self.prog.func(fid);
        if !matches!(f.mode, FuncMode::Plain | FuncMode::Env) {
            return Err("generator or async function");
        }
        let fs = self.bc_fn_scope(fid).ok_or("no scope")?;
        let w = self.bc_walk(Some(fid), fs.scope);
        if w.bad.is_some() {
            return Err("scope walk");
        }
        let mut g = Gen {
            prog: self.prog.clone(),
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
            pos: f.span.start,
            strict: f.strict,
            compl: None,
        };
        match f.body {
            Body::Expr(e) => {
                let r = g.reg()?;
                g.expr(e, r)?;
                g.emit(Op::Ret, r, 0, 0)?;
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
        let off = target as i64 - at as i64 - 1;
        let w = self.code.ops[at];
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
        let sc = *self.sites.get(&site).ok_or("a site that the walk did not visit")?;
        Ok(self.it.bc_resolve(n, sc))
    }

    fn load(&mut self, s: Site, n: u32, dst: u32) -> G<()> {
        match s {
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

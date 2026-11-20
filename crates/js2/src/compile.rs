//! The compiler of the new engine (`docs/design-engine-rewrite.md` 6.5, 6.10 items 1 to 7 and 6.11 items 5 to 7; plan step S2.5).
//!
//! It turns the tree of **one function** (`parse_function`) into a [`Code`]. The tree is dropped by the caller after the compile. Inner
//! functions are not compiled here: each one is a [`Child`] (a number of the facts) that the caller compiles when it first runs.
//!
//! **Variables.** A variable that no closure uses is a *register* (a parameter is the register of its argument). A variable that a
//! closure uses is a *context slot*: the facts of the children (`FnFacts::caps`) say which names they take from outside, and a name is
//! captured when a child inside the scope of the declaration lists it. A scope with captured variables pushes one context (`PushCtx`);
//! a closure takes the current context. A child finds an outer variable by name in the chain of [`Level`]s that the parent gives it, and
//! counts the hops. A name that no scope declares is a global (`GetGlobal`, by site). A script's own `var`, `let` and functions are
//! globals too (the interpreter's global lookup must see them).
//!
//! **Result contexts.** [`Fc::to`] gives a value in a register that the parent chose (destination-driven: the last instruction of the
//! expression writes it, so there is no `Mov` after it). [`Fc::any`] gives a register that holds the value, which is the variable's own
//! register for a register variable. [`Fc::effect`] drops the value (and compiles `&&`, `||`, `?:` to jumps). [`Fc::cond`] jumps on the
//! truth of an expression (compare-jumps with a constant on either side; both polarities for `<`).
//!
//! **Root maps.** The compiler keeps the set of registers that hold a value (`live`): a register joins it when an instruction writes it
//! and leaves it when its scope or temporary ends. Each safepoint instruction gets that set as it is *before* the instruction. So a map
//! names only registers that every path has written (the verifier checks this), and a frame needs no fill (design 6.10 item 4). A
//! register that is dead but still in scope stays in the map until its scope ends: a superset, which is safe (it keeps garbage).
//!
//! **Not built** (each one is recorded in [`Compiled::unsupported`] and the code is not to be run; the driver throws the TypeError of
//! design 6.12 that names it): see [`Fc::unsup`] and the PR text. The reason is mostly that the format of S2.4 has no op for it yet.
use crate::ast::*;
use crate::atoms::SharedAtoms;
use crate::bytecode::{Asm, BcError, Code, Handler, Ins, Label, Op, Recipes};
use crate::facts::{self, Facts, FnId};
use crate::roots::RootMaps;
use crate::string::{Chars, Str};
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug)]
pub enum CompileError {
    /// A limit of the format: more than 65,535 registers, slots or table entries.
    Limit(&'static str),
    /// The atom table could not make an atom.
    Memory,
    Asm(BcError),
}

impl From<BcError> for CompileError {
    fn from(e: BcError) -> Self {
        CompileError::Asm(e)
    }
}

pub(crate) type CR<T = ()> = Result<T, CompileError>;

/// One context of the chain that a closure sees: the captured variables of one scope of the function that made it.
#[derive(Debug)]
pub struct Level {
    pub parent: Option<Rc<Level>>,
    pub vars: Vec<LevelVar>,
}

#[derive(Debug)]
pub struct LevelVar {
    pub name: Box<str>,
    pub slot: u32,
    /// A `let`, `const` or `class`: a read can find the hole.
    pub tdz: bool,
    pub konst: bool,
}

impl Level {
    fn find(&self, name: &str) -> Option<&LevelVar> {
        self.vars.iter().rev().find(|v| &*v.name == name)
    }
}

/// What a child needs from its parent to find outer variables.
#[derive(Clone, Debug, Default)]
pub struct Input {
    /// The contexts around the function, innermost first. `None`: no outer function has a context.
    pub chain: Option<Rc<Level>>,
}

/// A function inside the compiled one: the entry of the facts, and the chain that its closure sees. Parallel to `Code::funcs`.
#[derive(Clone, Debug)]
pub struct Child {
    pub id: FnId,
    pub chain: Option<Rc<Level>>,
}

#[derive(Debug)]
pub struct Compiled {
    pub code: Code,
    pub children: Vec<Child>,
    pub strict: bool,
    /// The constructs that this compiler does not build. When it is not empty the code is not complete and must not run.
    pub unsupported: Vec<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VK {
    Var,
    Let,
    Const,
    Func,
    Param,
    Class,
    Catch,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Home {
    Reg(u32),
    /// The slot of the context that was the `level`-th that this function pushed (1 is the first).
    Ctx { level: u32, slot: u32 },
}

pub(crate) struct Var<'a> {
    pub name: Cow<'a, str>,
    pub kind: VK,
    pub home: Home,
    /// A read at an offset before this one can find the hole.
    pub decl_end: u32,
    pub always_tdz: bool,
}

#[derive(Default)]
pub(crate) struct Scope<'a> {
    pub vars: Vec<Var<'a>>,
}

/// A name that a scope declares.
pub(crate) struct Decl<'a> {
    pub name: Cow<'a, str>,
    pub kind: VK,
    /// The end of the declaration: a read before it can find the hole.
    pub end: u32,
    /// The variable already has this register (a parameter).
    pub reg: Option<u32>,
}

/// What a name means at one place.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Res {
    Reg { reg: u32, tdz: bool, konst: bool },
    Ctx { hops: u32, slot: u32, tdz: bool, konst: bool },
    Global,
}

/// A label with a flag: is there a jump to it? Code after an unconditional jump is dropped until a used label is bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Lb(pub usize);

/// A target of `break`, `continue` and `return`, and what they must run on the way out.
pub(crate) struct Frame {
    pub labels: Vec<String>,
    pub kind: FrameKind,
    /// The contexts that the function has pushed where `continue` goes (and where a `try` starts).
    pub depth: u32,
    /// The contexts that it has pushed where `break` goes.
    pub brk_depth: u32,
    pub ctx: Option<Rc<Level>>,
    /// The number of scopes at the frame (a repeated finalizer sees only these).
    pub nscopes: usize,
}

pub(crate) enum FrameKind {
    Loop { brk: Lb, cont: Lb, iter: Option<u32> },
    /// A `switch` (`is_switch`: an unlabeled `break` ends it) or a labeled statement.
    Block { brk: Lb, is_switch: bool },
    /// A `try` with a `finally`: the code of the finalizer is repeated on each jump out. `pieces` are the ranges of instructions that the
    /// handlers protect; a repeated finalizer is cut out of them.
    Try { fin: Option<StmtId>, pieces: Vec<(usize, usize)>, open: Option<usize> },
}

pub(crate) struct HandlerRec {
    pub start: usize,
    pub end: usize,
    pub target: Lb,
    pub reg: u32,
    pub depth: u32,
}

pub(crate) struct Fc<'a> {
    pub prog: &'a Program<'a>,
    pub src: &'a str,
    pub facts: &'a Facts,
    pub atoms: &'a SharedAtoms,
    pub asm: Asm,
    /// Each label: the assembler label, whether a jump or a handler reaches it, and the live registers that all jumps to it share.
    pub labels: Vec<(Label, bool, Option<Vec<u64>>)>,
    pub dead: bool,
    // tables
    pub atom_tab: Vec<Str>,
    pub atom_ix: HashMap<Vec<u16>, u32>,
    pub sites: Vec<u32>,
    pub numbers: Vec<f64>,
    pub num_ix: HashMap<u64, u32>,
    pub funcs: Vec<u32>,
    pub children: Vec<Child>,
    pub recipes: Recipes,
    pub unit_buf: Vec<u16>,
    // registers
    pub next: u32,
    pub max: u32,
    pub live: Vec<u64>,
    pub is_var: Vec<u64>,
    pub overflow: Option<&'static str>,
    // the position of each safepoint's map in `root_regs`
    pub root_at: Vec<(usize, usize)>,
    pub root_regs: Vec<u16>,
    pub lines: Vec<(usize, u32)>,
    pub handlers: Vec<HandlerRec>,
    // scopes
    pub scopes: Vec<Scope<'a>>,
    pub outer: Option<Rc<Level>>,
    pub cur_ctx: Option<Rc<Level>>,
    pub depth: u32,
    pub cap: HashMap<Cow<'a, str>, Vec<u32>>,
    pub force_ctx: bool,
    pub strict: bool,
    pub is_script: bool,
    pub is_arrow: bool,
    pub fn_flags: u16,
    // control flow
    pub frames: Vec<Frame>,
    pub chain_short: Option<Lb>,
    /// Labels of the statement that is about to be compiled (`a: b: for (...)`).
    pub pending: Vec<String>,
    pub unsupported: Vec<&'static str>,
}

/// The first `PRESCAN` bytes of text are searched for an early use of a name; past that the compiler assumes one.
const PRESCAN: u32 = 4096;

impl<'a> Fc<'a> {
    pub(crate) fn new(prog: &'a Program<'a>, facts: &'a Facts, atoms: &'a SharedAtoms, input: &Input) -> Fc<'a> {
        Fc {
            prog,
            src: prog.source,
            facts,
            atoms,
            asm: Asm::default(),
            labels: Vec::new(),
            dead: false,
            atom_tab: Vec::new(),
            atom_ix: HashMap::new(),
            sites: Vec::new(),
            numbers: Vec::new(),
            num_ix: HashMap::new(),
            funcs: Vec::new(),
            children: Vec::new(),
            recipes: Recipes::default(),
            unit_buf: Vec::new(),
            next: 0,
            max: 0,
            live: Vec::new(),
            is_var: Vec::new(),
            overflow: None,
            root_at: Vec::new(),
            root_regs: Vec::new(),
            lines: Vec::new(),
            handlers: Vec::new(),
            scopes: Vec::new(),
            outer: input.chain.clone(),
            cur_ctx: input.chain.clone(),
            depth: 0,
            cap: HashMap::new(),
            force_ctx: false,
            strict: false,
            is_script: false,
            is_arrow: false,
            fn_flags: 0,
            frames: Vec::new(),
            chain_short: None,
            pending: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    /// Record a construct that is not built. The code that follows is not meant to run.
    pub(crate) fn unsup(&mut self, what: &'static str) {
        if !self.unsupported.contains(&what) {
            self.unsupported.push(what);
        }
    }

    // ------------------------------------------------------------ tables

    pub(crate) fn atom_units(&mut self, units: &[u16]) -> CR<u32> {
        if let Some(&i) = self.atom_ix.get(units) {
            return Ok(i);
        }
        let bytes: Option<Vec<u8>> = units.iter().map(|&u| u8::try_from(u).ok()).collect();
        let s = match &bytes {
            Some(b) => self.atoms.intern(Chars::One(b)),
            None => self.atoms.intern(Chars::Two(units)),
        }
        .map_err(|_| CompileError::Memory)?;
        let i = self.atom_tab.len() as u32;
        self.atom_tab.push(s);
        self.atom_ix.insert(units.to_vec(), i);
        Ok(i)
    }

    pub(crate) fn atom(&mut self, s: &str) -> CR<u32> {
        let mut buf = std::mem::take(&mut self.unit_buf);
        buf.clear();
        buf.extend(s.encode_utf16());
        let r = self.atom_units(&buf);
        self.unit_buf = buf;
        r
    }

    /// The atom of the string literal at `span`.
    pub(crate) fn string_atom(&mut self, span: Span) -> CR<u32> {
        let mut buf = std::mem::take(&mut self.unit_buf);
        buf.clear();
        if self.prog.string(span, &mut buf).is_err() {
            buf.clear();
        }
        let r = self.atom_units(&buf);
        self.unit_buf = buf;
        r
    }

    /// A new site for the atom `atom`. Each property or global instruction has its own.
    pub(crate) fn site(&mut self, atom: u32) -> u32 {
        // Dead code is not emitted, so its instruction would leave the site unused.
        if self.dead {
            return 0;
        }
        self.sites.push(atom);
        self.sites.len() as u32 - 1
    }

    pub(crate) fn site_of(&mut self, name: &str) -> CR<u32> {
        let a = self.atom(name)?;
        Ok(self.site(a))
    }

    pub(crate) fn number(&mut self, v: f64) -> u32 {
        *self.num_ix.entry(v.to_bits()).or_insert_with(|| {
            self.numbers.push(v);
            self.numbers.len() as u32 - 1
        })
    }

    // ------------------------------------------------------------ registers

    pub(crate) fn alloc(&mut self) -> u32 {
        let r = self.next;
        self.next += 1;
        if self.next > self.max {
            self.max = self.next;
            if self.max > u16::MAX as u32 && self.overflow.is_none() {
                self.overflow = Some("more than 65,535 registers");
            }
        }
        r
    }

    /// `n` registers in a row, for a call or a literal.
    pub(crate) fn window(&mut self, n: u32) -> u32 {
        let base = self.next;
        for _ in 0..n.max(1) {
            self.alloc();
        }
        base
    }

    pub(crate) fn mark(&self) -> u32 {
        self.next
    }

    /// Free the registers from `m` up; they no longer hold a value.
    pub(crate) fn free(&mut self, m: u32) {
        self.next = m;
        Self::below(&mut self.live, m);
        Self::below(&mut self.is_var, m);
    }

    /// Clear the bits from `m` up.
    fn below(v: &mut [u64], m: u32) {
        for (w, word) in v.iter_mut().enumerate() {
            let lo = w as u32 * 64;
            if lo >= m {
                *word = 0;
            } else if lo + 64 > m {
                *word &= (1u64 << (m - lo)) - 1;
            }
        }
    }

    fn bit(v: &mut Vec<u64>, r: u32, on: bool) {
        let w = r as usize / 64;
        if v.len() <= w {
            v.resize(w + 1, 0);
        }
        if on {
            v[w] |= 1 << (r % 64);
        } else {
            v[w] &= !(1 << (r % 64));
        }
    }

    pub(crate) fn set_live(&mut self, r: u32) {
        Self::bit(&mut self.live, r, true);
    }

    pub(crate) fn set_var(&mut self, r: u32) {
        Self::bit(&mut self.is_var, r, true);
    }

    pub(crate) fn is_var_reg(&self, r: u32) -> bool {
        self.is_var.get(r as usize / 64).is_some_and(|w| w >> (r % 64) & 1 != 0)
    }

    // ------------------------------------------------------------ emitting

    pub(crate) fn label(&mut self) -> Lb {
        self.labels.push((self.asm.label(), false, None));
        Lb(self.labels.len() - 1)
    }

    /// Put the label here. Code after an unconditional jump is live again when a jump goes to the label.
    pub(crate) fn bind(&mut self, l: Lb) {
        self.asm.bind(self.labels[l.0].0);
        // A register is live after a join only when every path to it wrote the register.
        if let Some(mut lv) = self.labels[l.0].2.take() {
            if self.dead {
                Self::below(&mut lv, self.next);
                self.live = lv;
            } else {
                for (w, word) in self.live.iter_mut().enumerate() {
                    *word &= lv.get(w).copied().unwrap_or(0);
                }
            }
        }
        if self.labels[l.0].1 {
            self.dead = false;
        }
    }

    /// The label is reached by a handler, not by a jump.
    pub(crate) fn mark_used(&mut self, l: Lb) {
        self.labels[l.0].1 = true;
    }

    pub(crate) fn emit(&mut self, ins: Ins) -> Option<usize> {
        if self.dead {
            return None;
        }
        let idx = self.asm.emit(ins);
        self.after(idx, ins.op);
        Some(idx)
    }

    fn after(&mut self, idx: usize, op: Op) {
        if op.is_safepoint() {
            self.root_at.push((idx, self.root_regs.len()));
            for (w, &word) in self.live.iter().enumerate() {
                let mut word = word;
                while word != 0 {
                    let b = word.trailing_zeros();
                    word &= word - 1;
                    self.root_regs.push((w as u32 * 64 + b) as u16);
                }
            }
        }
        if !op.falls_through() {
            self.dead = true;
        }
    }

    pub(crate) fn op(&mut self, op: Op, a: u32, b: u32, c: u32) {
        self.emit(Ins::new(op, a, b, c));
    }

    pub(crate) fn opx(&mut self, op: Op, a: u32, b: u32, c: u32, x: u32) {
        self.emit(Ins::new(op, a, b, c).with_x(x));
    }

    fn jump_ins(&mut self, ins: Ins, to: Lb) {
        if self.dead {
            return;
        }
        let idx = self.asm.jump(ins, self.labels[to.0].0);
        self.after(idx, ins.op);
        // A backward jump goes to a label that is already bound; its registers are those of the first entry.
        if ins.op != Op::JmpBack {
            let lab = &mut self.labels[to.0];
            lab.1 = true;
            match &mut lab.2 {
                None => lab.2 = Some(self.live.clone()),
                Some(lv) => {
                    for (w, word) in lv.iter_mut().enumerate() {
                        *word &= self.live.get(w).copied().unwrap_or(0);
                    }
                }
            }
        } else {
            self.labels[to.0].1 = true;
        }
    }

    pub(crate) fn jmp(&mut self, to: Lb) {
        self.jump_ins(Ins::new(Op::Jmp, 0, 0, 0), to);
    }

    pub(crate) fn jmp_back(&mut self, to: Lb) {
        self.jump_ins(Ins::new(Op::JmpBack, 0, 0, 0), to);
    }

    /// A jump on one register (`JmpIf`, `JSEqNull`, ...): the form `AsBx`.
    pub(crate) fn jmp_r(&mut self, op: Op, r: u32, to: Lb) {
        self.jump_ins(Ins::new(op, r, 0, 0), to);
    }

    /// A compare of two registers with a jump: the form `AbcX`.
    pub(crate) fn jmp_rr(&mut self, op: Op, a: u32, b: u32, to: Lb) {
        self.jump_ins(Ins::new(op, a, b, 0), to);
    }

    /// A compare with a constant `x` and a jump: the form `AsBxX`.
    pub(crate) fn jmp_rx(&mut self, op: Op, r: u32, x: u32, to: Lb) {
        self.jump_ins(Ins::new(op, r, 0, 0).with_x(x), to);
    }

    /// The source offset of the next instruction, for error positions.
    pub(crate) fn line(&mut self, off: u32) {
        if !self.dead {
            let at = self.asm_len();
            match self.lines.last_mut() {
                Some(l) if l.0 == at => l.1 = off,
                Some(l) if l.1 == off => {}
                _ => self.lines.push((at, off)),
            }
        }
    }

    fn asm_len(&self) -> usize {
        self.asm.len()
    }

    // ------------------------------------------------------------ scopes

    /// Is a variable named `name`, declared in the scope `span`, used by a closure inside that scope?
    pub(crate) fn captured(&self, name: &str, span: Span) -> bool {
        self.force_ctx || self.cap.get(name).is_some_and(|starts| starts.iter().any(|&s| s >= span.start && s < span.end))
    }

    /// Declare the variables of a scope. Gives the scope's depth marker. A captured variable goes to a context slot, and when there is one
    /// the scope pushes a context. `decls` are `(name, kind, end of the declaration)`.
    pub(crate) fn enter(&mut self, span: Span, decls: &[Decl<'a>], switch: bool) -> CR<ScopeMark> {
        let mark = ScopeMark { reg: self.next, scopes: self.scopes.len(), ctx: self.depth, cur: self.cur_ctx.clone() };
        let mut vars: Vec<Var<'a>> = Vec::with_capacity(decls.len());
        let mut slots: Vec<LevelVar> = Vec::new();
        let level = self.depth + 1;
        for d in decls {
            let (name, kind) = (&d.name, d.kind);
            if let Some(v) = vars.iter_mut().find(|v| v.name == *name) {
                // A second declaration of the name in one scope (`var a; var a`, a parameter and a `var`) is the same variable.
                if matches!(kind, VK::Func) {
                    v.kind = VK::Func;
                }
                if let (Some(r), Home::Reg(_)) = (d.reg, v.home) {
                    v.home = Home::Reg(r);
                }
                continue;
            }
            let tdz = matches!(kind, VK::Let | VK::Const | VK::Class);
            let home = if self.captured(name, span) {
                let slot = slots.len() as u32;
                slots.push(LevelVar { name: name.as_ref().into(), slot, tdz, konst: kind == VK::Const });
                Home::Ctx { level, slot }
            } else {
                let r = d.reg.unwrap_or_else(|| self.alloc());
                self.set_var(r);
                Home::Reg(r)
            };
            vars.push(Var { name: name.clone(), kind, home, decl_end: d.end, always_tdz: switch && tdz });
        }
        self.scopes.push(Scope { vars });
        if !slots.is_empty() {
            if slots.len() > u16::MAX as usize {
                return Err(CompileError::Limit("more than 65,535 context slots"));
            }
            self.op(Op::PushCtx, 0, slots.len() as u32, 0);
            self.depth += 1;
            self.cur_ctx = Some(Rc::new(Level { parent: self.cur_ctx.clone(), vars: slots }));
        }
        Ok(mark)
    }

    /// Leave the scope: pop its context (when this path is live) and free its registers.
    pub(crate) fn leave(&mut self, m: ScopeMark) {
        if self.depth > m.ctx {
            self.op(Op::PopCtx, 0, 0, 0);
        }
        self.depth = m.ctx;
        self.cur_ctx = m.cur;
        self.scopes.truncate(m.scopes);
        self.free(m.reg);
    }

    pub(crate) fn find_var(&self, name: &str) -> Option<&Var<'a>> {
        self.scopes.iter().rev().find_map(|s| s.vars.iter().rev().find(|v| v.name == name))
    }

    /// What `name` is at the source offset `at`.
    pub(crate) fn resolve(&self, name: &str, at: u32) -> Res {
        if let Some(v) = self.find_var(name) {
            let tdz = matches!(v.kind, VK::Let | VK::Const | VK::Class) && (v.always_tdz || at < v.decl_end);
            let konst = v.kind == VK::Const;
            return match v.home {
                Home::Reg(reg) => Res::Reg { reg, tdz, konst },
                Home::Ctx { level, slot } => Res::Ctx { hops: self.depth - level, slot, tdz, konst },
            };
        }
        let mut l = self.outer.as_ref();
        let mut hops = self.depth;
        while let Some(level) = l {
            if let Some(v) = level.find(name) {
                return Res::Ctx { hops, slot: v.slot, tdz: v.tdz, konst: v.konst };
            }
            hops += 1;
            l = level.parent.as_ref();
        }
        Res::Global
    }

    /// Does `name` occur in the text `from..to`? See [`Fc::occurrences`].
    pub(crate) fn may_occur(&self, name: &str, from: u32, to: u32) -> bool {
        self.occurrences(name, from, to) > 0
    }

    /// How often `name` occurs as a whole name in the text `from..to`. A name with an escape cannot be searched, so a `\` in the text
    /// counts as many. Too much text counts as many as well.
    pub(crate) fn occurrences(&self, name: &str, from: u32, to: u32) -> usize {
        if to <= from {
            return 0;
        }
        if to - from > PRESCAN {
            return usize::MAX;
        }
        let t = self.src.get(from as usize..to as usize).unwrap_or("");
        if t.contains('\\') {
            return usize::MAX;
        }
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
        let bytes = t.as_bytes();
        t.match_indices(name)
            // A hit inside a longer name is not a use (`i` in `function`).
            .filter(|(i, m)| {
                let inside = (*i > 0 && ident(bytes[i - 1])) || bytes.get(i + m.len()).is_some_and(|&b| ident(b));
                !inside
            })
            .count()
    }

    /// Can a read of the `let`, `const` or `class` name `d` come before its declaration is done? The text from `from` to the end of the
    /// declaration holds the declaring name once; any other hit is an early use.
    pub(crate) fn early_use(&self, d: &Decl, from: u32) -> bool {
        self.occurrences(&d.name, from, d.end) > 1
    }

    // ------------------------------------------------------------ children

    /// The child function whose body the pre-parser skipped. Gives its index in `Code::funcs`.
    pub(crate) fn child(&mut self, f: FuncId) -> CR<u32> {
        let func = self.prog.func(f);
        let id = match func.body {
            Body::Skipped(id) => id,
            _ => {
                self.unsup("a function that the pre-parser did not read");
                0
            }
        };
        self.funcs.push(id);
        self.children.push(Child { id, chain: self.cur_ctx.clone() });
        if self.funcs.len() > u16::MAX as usize {
            return Err(CompileError::Limit("more than 65,535 child functions"));
        }
        Ok(self.funcs.len() as u32 - 1)
    }

    // ------------------------------------------------------------ finish

    pub(crate) fn finish(mut self, nparams: u32) -> CR<Compiled> {
        if !self.dead {
            self.op(Op::RetUndef, 0, 0, 0);
        }
        if let Some(why) = self.overflow {
            return Err(CompileError::Limit(why));
        }
        let nregs = self.max.max(nparams).max(1) as u16;
        let asm = std::mem::take(&mut self.asm).finish()?;
        let mut code = Code::new(nregs, nparams as u16);
        let mut roots = RootMaps::new(nregs as usize);
        for (k, &(idx, start)) in self.root_at.iter().enumerate() {
            let end = self.root_at.get(k + 1).map_or(self.root_regs.len(), |n| n.1);
            roots.add(asm.pcs[idx], self.root_regs[start..end].iter().map(|&r| r as usize));
        }
        for h in &self.handlers {
            let (s, e) = (asm.pcs[h.start], asm.pcs[h.end]);
            if s < e {
                code.handlers.push(Handler { start: s, end: e, target: asm.label_pcs[h.target.0], reg: h.reg as u16, depth: h.depth as u16 });
            }
        }
        let mut last = u32::MAX;
        for &(idx, off) in &self.lines {
            let pc = asm.pcs[idx];
            if pc as usize >= asm.words.len() {
                continue;
            }
            if pc == last {
                code.lines.last_mut().expect("a pair was pushed").1 = off;
            } else {
                code.lines.push((pc, off));
                last = pc;
            }
        }
        code.words = asm.words;
        code.loops = asm.loops;
        code.numbers = self.numbers;
        code.atoms = self.atom_tab;
        code.funcs = self.funcs;
        code.recipes = self.recipes;
        code.sites = self.sites;
        code.roots = roots;
        Ok(Compiled { code, children: self.children, strict: self.strict, unsupported: self.unsupported })
    }
}

/// What `enter` saves for `leave`.
pub(crate) struct ScopeMark {
    reg: u32,
    scopes: usize,
    ctx: u32,
    cur: Option<Rc<Level>>,
}

// ---------------------------------------------------------------- entry points

/// Compile the top level of a script (`parse_script_lazy`). Its `var`, `let`, `const` and functions are globals.
pub fn compile_script(prog: &Program, facts: &Facts, atoms: &SharedAtoms) -> CR<Compiled> {
    let mut c = Fc::new(prog, facts, atoms, &Input::default());
    c.is_script = true;
    c.strict = prog.strict;
    c.fn_flags = 0;
    let kids: Vec<_> = facts.children(None).collect();
    c.note_children(&kids);
    c.script_body(prog.body)?;
    c.finish(0)
}

/// Compile function `id` of `facts`. `prog` and `fid` come from `parse_function(source, facts, id, ..)`. `input` is the chain that the
/// parent recorded for this child ([`Child::chain`]).
pub fn compile_function(prog: &Program, fid: FuncId, id: FnId, facts: &Facts, atoms: &SharedAtoms, input: &Input) -> CR<Compiled> {
    let mut c = Fc::new(prog, facts, atoms, input);
    let f = facts.get(id);
    c.strict = prog.func(fid).strict;
    c.is_arrow = f.kind == FuncKind::Arrow;
    c.fn_flags = f.flags;
    c.force_ctx = f.flags & facts::INNER_EVAL != 0;
    if f.flags & (facts::EVAL | facts::WITH | facts::IN_WITH) != 0 {
        c.unsup("direct eval or with");
    }
    let kids: Vec<_> = facts.children(Some(id)).collect();
    c.note_children(&kids);
    c.function_body(fid)?;
    let n = prog.func(fid).params.len() as u32;
    c.finish(n)
}

impl<'a> Fc<'a> {
    /// Record which outer names the direct children use, by the offset where the child starts.
    fn note_children(&mut self, kids: &[FnId]) {
        let facts = self.facts;
        for &k in kids {
            let start = facts.get(k).start;
            for &span in facts.captures(k) {
                let name = name_in(self.src, span);
                // `this` of an arrow is a flag, not a capture: it is the pseudo-name `this`.
                self.cap.entry(name).or_default().push(start);
            }
            if facts.get(k).kind == FuncKind::Arrow && facts.get(k).flags & facts::THIS != 0 {
                self.cap.entry(Cow::Borrowed("this")).or_default().push(start);
            }
        }
    }
}

/// A driver for tests and tools: compile a script and, after it, every function inside it, parents first. The chain of each child comes
/// from its parent. Gives the script and then the functions in the order of the facts' ids that were reached.
pub fn compile_all(source: &str, atoms: &SharedAtoms, limits: crate::parse::Limits) -> Result<(Compiled, Vec<(FnId, Compiled)>), String> {
    use crate::parse::{parse_function, parse_script_lazy};
    let mut facts = Facts::new();
    let prog = parse_script_lazy(source, limits, &mut facts).map_err(|e| format!("parse: {}", e.msg))?;
    let script = compile_script(&prog, &facts, atoms).map_err(|e| format!("{e:?}"))?;
    let mut out: Vec<(FnId, Compiled)> = Vec::new();
    let mut todo: Vec<Child> = script.children.clone();
    while let Some(ch) = todo.pop() {
        let (p, fid) = parse_function(source, &mut facts, ch.id, limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
        let c = compile_function(&p, fid, ch.id, &facts, atoms, &Input { chain: ch.chain.clone() }).map_err(|e| format!("{e:?}"))?;
        todo.extend(c.children.iter().cloned());
        out.push((ch.id, c));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok((script, out))
}

#[path = "compile_expr.rs"]
mod expr;
#[path = "compile_pat.rs"]
mod pat;
#[path = "compile_stmt.rs"]
mod stmt;

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;

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
//! **Classes.** A class makes its constructor, its prototype and its elements in the order of the text. The initializers of fields and
//! the `static` blocks are functions that the parent compiles itself (their text is in its tree): see `compile_class.rs` and [`Child::ready`].
//!
//! **Not built** (each one is recorded in [`Compiled::unsupported`] and the code is not to be run; the driver throws the TypeError of
//! design 6.12 that names it): generators, async functions, `await`, `yield`, `for await`, `with`, direct `eval`,
//! `delete` of a global name. See [`Fc::unsup`] and the progress entry.
use crate::ast::*;
use crate::atoms::SharedAtoms;
use crate::bytecode::{Asm, BcError, Code, GK_ASYNC, GK_ASYNCGEN, GK_GEN, Handler, Ins, Label, NO_NAME, Op, Recipes};
use crate::data_lit::DataLiteral;
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
    /// Name to position in `vars`, for a level with more than `INDEX_MIN` names.
    index: HashMap<Box<str>, u32>,
}

#[derive(Debug)]
pub struct LevelVar {
    pub name: Box<str>,
    pub slot: u32,
    /// A `let`, `const` or `class`: a read can find the hole.
    pub tdz: bool,
    pub konst: bool,
    /// The name of a named function expression, inside the function: an assignment throws in strict code and does nothing in sloppy code.
    pub callee: bool,
    /// A module binding that lives in a property of an environment object (`docs/design-js2-host-api.md` section 12): the slot holds the
    /// object and this is the name of the property. Only the `Level` of a module has such a variable.
    pub env: Option<Box<str>>,
}

impl Level {
    pub fn new(parent: Option<Rc<Level>>, vars: Vec<LevelVar>) -> Level {
        let index = if vars.len() > INDEX_MIN { vars.iter().enumerate().map(|(i, v)| (v.name.clone(), i as u32)).collect() } else { HashMap::new() };
        Level { parent, vars, index }
    }

    fn find(&self, name: &str) -> Option<&LevelVar> {
        if self.index.is_empty() {
            self.vars.iter().rev().find(|v| &*v.name == name)
        } else {
            self.index.get(name).map(|&i| &self.vars[i as usize])
        }
    }
}

/// What a child needs from its parent to find outer variables.
#[derive(Clone, Debug, Default)]
pub struct Input {
    /// The contexts around the function, innermost first. `None`: no outer function has a context.
    pub chain: Option<Rc<Level>>,
    /// The function is a named function expression: its name is a variable inside it that holds the function ([`Child::self_name`]).
    pub self_name: bool,
    /// The `name` of the function ([`Child::name`]).
    pub name: Option<Box<str>>,
    /// The function is in the code of an event handler (`compile_function`): a name that no scope declares is read by the full search
    /// (`GetName`), which looks in the objects of `call_scoped` first.
    pub scoped: bool,
    /// The function is in the text of the prelude (`prelude.js`): a free name that is a slot of `prelude_pre.rs` is read by `GetPre`.
    pub pre: bool,
}

/// A function inside the compiled one: the entry of the facts, and the chain that its closure sees. Parallel to `Code::funcs`.
#[derive(Clone, Debug)]
pub struct Child {
    pub id: FnId,
    pub chain: Option<Rc<Level>>,
    /// A named function expression. Its name is a binding in a scope of its own, inside the function and outside its parameters. The
    /// child compiles it as a variable (`LoadCallee`) when its text uses the name. (A function declaration binds the name outside.)
    pub self_name: bool,
    /// The `name` property of the function: its own name, else the name that the text around it gives (`var f = function () {}`).
    pub name: Option<Box<str>>,
    /// The function has no tree to parse: the parent compiled it, and its code is `Compiled::inline[ready]` of the parent (the initializers
    /// of the fields of a class and its `static` blocks: their text is in the tree of the function that holds the class).
    pub ready: Option<u32>,
}

/// The name that the function or class `0` gets when the compiler makes it (`SetFunctionName` of an anonymous function definition).
#[derive(Clone, Debug)]
pub enum Hint {
    Func(FuncId, Box<str>),
    Class(ClassId, Box<str>),
}

#[derive(Debug)]
pub struct Compiled {
    pub code: Code,
    pub children: Vec<Child>,
    pub strict: bool,
    /// The constructs that this compiler does not build. When it is not empty the code is not complete and must not run.
    pub unsupported: Vec<&'static str>,
    /// The functions of the children with [`Child::ready`], in the order of those children.
    pub inline: Vec<Compiled>,
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
    /// The name of a named function expression, inside the function. Read only (an assignment throws in strict code; in sloppy code it
    /// should do nothing, and here it assigns).
    Callee,
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
    /// Name to position in `vars`, for a scope with more than `INDEX_MIN` names (a scan of a few names is faster than a hash).
    pub index: HashMap<Cow<'a, str>, u32>,
}

/// A scope with more names than this has a hash index.
const INDEX_MIN: usize = 8;

impl<'a> Scope<'a> {
    pub(crate) fn find(&self, name: &str) -> Option<&Var<'a>> {
        if self.index.is_empty() {
            self.vars.iter().rev().find(|v| v.name == name)
        } else {
            self.index.get(name).map(|&i| &self.vars[i as usize])
        }
    }
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
    /// `env`: the slot holds an environment object and the value is its property ([`Fc::env_key`]).
    Ctx { hops: u32, slot: u32, tdz: bool, konst: bool, env: bool },
    Global,
}

/// How a member of an assignment target or an update is read and written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Plain,
    /// `super.x`: the object register holds `this`.
    Super,
    /// `o.#x`: the key register holds the private name.
    Private,
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
    /// The ranges of instructions that the handler of this frame covers (a `try` with a `finally`, a `for-of` body). Code that a jump out
    /// of the frame repeats (a finalizer, an iterator close) is cut out of them: an exception there is not for this frame's handler.
    pub pieces: Vec<(usize, usize)>,
    /// The start of the range that is open now, `None` while the frame is paused.
    pub open: Option<usize>,
}

pub(crate) enum FrameKind {
    Loop { brk: Lb, cont: Lb, iter: Option<u32> },
    /// A `switch` (`is_switch`: an unlabeled `break` ends it) or a labeled statement.
    Block { brk: Lb, is_switch: bool },
    /// A `try` with a `finally`: the code of the finalizer is repeated on each jump out.
    Try { fin: Option<StmtId> },
}

pub(crate) struct HandlerRec {
    pub start: usize,
    pub end: usize,
    pub target: Lb,
    pub reg: u32,
    pub depth: u32,
}

/// A handler block that is emitted after the last instruction of the function, where no path falls into it: close the iterator that
/// record `rec` holds (unless `done` is true), and throw the exception that is in `ex`.
pub(crate) struct Cold {
    pub label: Lb,
    /// The registers that are live at the start of the range that the handler covers, and `ex`.
    pub live: Vec<u64>,
    pub rec: u32,
    pub done: u32,
    pub ex: u32,
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
    pub inline: Vec<Compiled>,
    pub recipes: Recipes,
    pub data: Vec<DataLiteral>,
    /// The switch tables (`Code::tables`). A target is 0 until `finish`, which fills each `(index, label)` of `table_fix` with the `pc`.
    pub tables: Vec<u32>,
    pub table_fix: Vec<(usize, Lb)>,
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
    pub cold: Vec<Cold>,
    // scopes
    pub scopes: Vec<Scope<'a>>,
    pub outer: Option<Rc<Level>>,
    pub cur_ctx: Option<Rc<Level>>,
    pub depth: u32,
    pub cap: HashMap<Cow<'a, str>, Vec<u32>>,
    pub force_ctx: bool,
    pub strict: bool,
    pub is_script: bool,
    /// The register of the completion value of a script that is run for its value (`ScriptMode::value`).
    pub value: Option<u32>,
    /// The script is the code of an `eval`: its `let`, `const` and `class` names are not global bindings (`ScriptMode::eval`).
    pub eval: bool,
    /// Event handler code: free names go through `GetName`, `SetName` and `TypeofName` (`Input::scoped`).
    pub scoped: bool,
    /// The function is in the prelude (`Input::pre`).
    pub pre: bool,
    pub is_arrow: bool,
    /// The function cannot be a constructor (an arrow, a method, a getter or a setter).
    pub noctor: bool,
    pub fn_flags: u16,
    pub fn_id: Option<FnId>,
    pub self_name: bool,
    /// The `name` of this function (from the parent), its `length`, and the name that the next function or class made here gets.
    pub fn_name: Option<Box<str>>,
    pub fn_len: u16,
    pub hint: Option<Hint>,
    /// The function is the constructor of a class with `extends`.
    pub derived: bool,
    // control flow
    pub frames: Vec<Frame>,
    pub chain_short: Option<Lb>,
    /// Labels of the statement that is about to be compiled (`a: b: for (...)`).
    pub pending: Vec<String>,
    pub unsupported: Vec<&'static str>,
    /// [`GK_GEN`] while a generator is compiled, else 0 (`compile_gen.rs`), and the longest save list that it made.
    pub gen_kind: u8,
    pub gen_regs: u16,
    /// The catch-all handler of an async function: where its range starts (an instruction index), the register that gets the exception
    /// and the context depth (`compile_async.rs`).
    pub async_h: Option<(usize, u32, u32)>,
    /// The head of a `for-of` is being bound: its loop frame is not pushed yet, but a throw of a pattern there closes the iterator.
    pub of_head: bool,
    /// The iterator records (`rec` registers) of the `for await` loops that are open now: their exits wait for `return()` (`compile_asyncgen.rs`).
    pub await_recs: Vec<u32>,
    /// While the default of parameter `i` is compiled and it names a parameter that is not set yet: `(i, number of parameters, register
    /// of the hole)`. A read of such a parameter reads the hole, so it throws.
    pub param_dz: Option<(u32, u32, u32)>,
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
            inline: Vec::new(),
            recipes: Recipes::default(),
            data: Vec::new(),
            tables: Vec::new(),
            table_fix: Vec::new(),
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
            cold: Vec::new(),
            scopes: Vec::new(),
            outer: input.chain.clone(),
            cur_ctx: input.chain.clone(),
            depth: 0,
            cap: HashMap::new(),
            force_ctx: false,
            strict: false,
            is_script: false,
            value: None,
            eval: false,
            scoped: input.scoped,
            pre: input.pre,
            is_arrow: false,
            noctor: false,
            fn_flags: 0,
            fn_id: None,
            self_name: input.self_name,
            fn_name: input.name.clone(),
            fn_len: 0,
            hint: None,
            derived: false,
            frames: Vec::new(),
            chain_short: None,
            pending: Vec::new(),
            unsupported: Vec::new(),
            gen_kind: 0,
            async_h: None,
            gen_regs: 0,
            of_head: false,
            await_recs: Vec::new(),
            param_dz: None,
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

    /// Clear the bits from `m` up. Words beyond the end of the vector are zero.
    fn below(v: &mut Vec<u64>, m: u32) {
        v.truncate(m.div_ceil(64) as usize);
        if let Some(word) = v.get_mut((m / 64) as usize) {
            *word &= (1u64 << (m % 64)) - 1;
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
            self.reach(to);
        } else {
            self.labels[to.0].1 = true;
        }
    }

    /// A forward jump reaches `to`: the label is used, and the registers live there are those that every jump to it has written.
    pub(crate) fn reach(&mut self, to: Lb) {
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
        // The names that a class makes (`#x`, `%...`) are no identifiers: a method or an initializer reads them, and the facts do not list them.
        self.force_ctx || name.starts_with(['#', '%']) || self.cap.get(name).is_some_and(|starts| starts.iter().any(|&s| s >= span.start && s < span.end))
    }

    /// Declare the variables of a scope. Gives the scope's depth marker. A captured variable goes to a context slot, and when there is one
    /// the scope pushes a context. `decls` are `(name, kind, end of the declaration)`.
    pub(crate) fn enter(&mut self, span: Span, decls: &[Decl<'a>], switch: bool) -> CR<ScopeMark> {
        let mark = ScopeMark { reg: self.next, scopes: self.scopes.len(), ctx: self.depth, cur: self.cur_ctx.clone() };
        let mut vars: Vec<Var<'a>> = Vec::with_capacity(decls.len());
        let mut index: HashMap<Cow<'a, str>, u32> = HashMap::new();
        let indexed = decls.len() > INDEX_MIN;
        let mut slots: Vec<LevelVar> = Vec::new();
        let level = self.depth + 1;
        for d in decls {
            let (name, kind) = (&d.name, d.kind);
            let known = if indexed { index.get(name).map(|&i| i as usize) } else { vars.iter().position(|v| v.name == *name) };
            if let Some(v) = known.map(|i| &mut vars[i]) {
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
                slots.push(LevelVar { name: name.as_ref().into(), slot, tdz, konst: kind == VK::Const || kind == VK::Callee, callee: kind == VK::Callee, env: None });
                Home::Ctx { level, slot }
            } else {
                let r = d.reg.unwrap_or_else(|| self.alloc());
                self.set_var(r);
                Home::Reg(r)
            };
            if indexed {
                index.insert(name.clone(), vars.len() as u32);
            }
            vars.push(Var { name: name.clone(), kind, home, decl_end: d.end, always_tdz: switch && tdz });
        }
        self.scopes.push(Scope { vars, index });
        if !slots.is_empty() {
            if slots.len() > u16::MAX as usize {
                return Err(CompileError::Limit("more than 65,535 context slots"));
            }
            self.op(Op::PushCtx, 0, slots.len() as u32, 0);
            self.depth += 1;
            self.cur_ctx = Some(Rc::new(Level::new(self.cur_ctx.clone(), slots)));
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
        self.scopes.iter().rev().find_map(|s| s.find(name))
    }

    /// What `name` is at the source offset `at`.
    pub(crate) fn resolve(&self, name: &str, at: u32) -> Res {
        if let Some(v) = self.find_var(name) {
            if let (Some((from, n, hole)), VK::Param, Home::Reg(r)) = (self.param_dz, v.kind, v.home)
                && (from..n).contains(&r)
            {
                return Res::Reg { reg: hole, tdz: true, konst: false };
            }
            let tdz = matches!(v.kind, VK::Let | VK::Const | VK::Class) && (v.always_tdz || at < v.decl_end);
            let konst = matches!(v.kind, VK::Const | VK::Callee);
            return match v.home {
                Home::Reg(reg) => Res::Reg { reg, tdz, konst },
                Home::Ctx { level, slot } => Res::Ctx { hops: self.depth - level, slot, tdz, konst, env: false },
            };
        }
        let mut l = self.outer.as_ref();
        let mut hops = self.depth;
        while let Some(level) = l {
            if let Some(v) = level.find(name) {
                return Res::Ctx { hops, slot: v.slot, tdz: v.tdz, konst: v.konst, env: v.env.is_some() };
            }
            hops += 1;
            l = level.parent.as_ref();
        }
        Res::Global
    }

    /// The property name of the module binding `name` that [`Res::Ctx`] marked with `env`.
    pub(crate) fn env_key(&self, name: &str) -> Option<Box<str>> {
        let mut l = self.outer.as_ref();
        while let Some(level) = l {
            if let Some(v) = level.find(name) {
                return v.env.clone();
            }
            l = level.parent.as_ref();
        }
        None
    }

    /// Is `name` the name of a named function expression that is a variable of this function or of one around it?
    pub(crate) fn is_callee(&self, name: &str) -> bool {
        if let Some(v) = self.find_var(name) {
            return v.kind == VK::Callee;
        }
        let mut l = self.outer.as_ref();
        while let Some(level) = l {
            if let Some(v) = level.find(name) {
                return v.callee;
            }
            l = level.parent.as_ref();
        }
        false
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
    pub(crate) fn child(&mut self, f: FuncId, self_name: bool, name: Option<Box<str>>) -> CR<u32> {
        let func = self.prog.func(f);
        let id = match func.body {
            Body::Skipped(id) => id,
            _ => {
                self.unsup("a function that the pre-parser did not read");
                0
            }
        };
        self.funcs.push(id);
        self.children.push(Child { id, chain: self.cur_ctx.clone(), self_name, name, ready: None });
        if self.funcs.len() > u16::MAX as usize {
            return Err(CompileError::Limit("more than 65,535 child functions"));
        }
        Ok(self.funcs.len() as u32 - 1)
    }

    // ------------------------------------------------------------ finish

    pub(crate) fn finish(mut self, nparams: u32) -> CR<Compiled> {
        if !self.dead {
            self.ret_undef();
        }
        for c in std::mem::take(&mut self.cold) {
            self.bind(c.label);
            self.live = c.live;
            self.set_live(c.ex);
            let skip = self.label();
            self.jmp_r(Op::JmpIf, c.done, skip);
            self.op(Op::IterClose, c.rec, 0, 1);
            self.bind(skip);
            self.op(Op::Throw, c.ex, 0, 0);
        }
        let async_end = self.asm.len();
        self.async_catch_all(async_end);
        if let Some(why) = self.overflow {
            return Err(CompileError::Limit(why));
        }
        let nregs = self.max.max(nparams).max(1) as u16;
        let fn_name = match self.fn_name.take() {
            Some(n) => self.atom(&n)?,
            None => NO_NAME,
        };
        let asm = std::mem::take(&mut self.asm).finish()?;
        for &(at, l) in &self.table_fix {
            self.tables[at] = asm.label_pcs[l.0];
        }
        let mut code = Code::new(nregs, nparams as u16);
        code.fn_len = self.fn_len;
        code.strict = self.strict;
        code.noctor = self.noctor;
        code.gen_kind = self.gen_kind;
        code.gen_regs = self.gen_regs;
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
        code.fn_name = fn_name;
        code.words = asm.words;
        code.loops = asm.loops;
        code.numbers = self.numbers;
        code.atoms = self.atom_tab;
        code.funcs = self.funcs;
        code.recipes = self.recipes;
        code.data = self.data;
        code.tables = self.tables;
        code.sites = self.sites;
        code.roots = roots;
        Ok(Compiled { code, children: self.children, strict: self.strict, unsupported: self.unsupported, inline: self.inline })
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

/// What a script is compiled for. The default is a script that nobody asks for a value.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScriptMode {
    /// The code returns the completion value of the script. The other code returns `undefined` and pays nothing for it.
    pub value: bool,
    /// The code of an `eval`: its `let`, `const` and `class` names stay in the code (the `var`s and functions are globals).
    pub eval: bool,
    /// The code of an event handler (`Input::scoped`), for the functions inside it too.
    pub scoped: bool,
}

/// Compile the top level of a script (`parse_script_lazy`). Its `var`, `let`, `const` and functions are globals.
pub fn compile_script(prog: &Program, facts: &Facts, atoms: &SharedAtoms) -> CR<Compiled> {
    compile_script_mode(prog, facts, atoms, ScriptMode::default())
}

/// [`compile_script`] for a `mode`.
pub fn compile_script_mode(prog: &Program, facts: &Facts, atoms: &SharedAtoms, mode: ScriptMode) -> CR<Compiled> {
    let mut c = Fc::new(prog, facts, atoms, &Input { scoped: mode.scoped, ..Input::default() });
    c.eval = mode.eval;
    c.value = mode.value.then_some(0);
    c.is_script = true;
    c.strict = prog.strict;
    c.fn_flags = 0;
    let kids: Vec<_> = facts.children(None).collect();
    c.note_children(&kids);
    c.script_body(prog.body)?;
    c.finish(0)
}

pub use module::{Exp, ExpSrc, Imp, ModInfo, compile_module};

/// Compile function `id` of `facts`. `prog` and `fid` come from `parse_function(source, facts, id, ..)`. `input` is the chain that the
/// parent recorded for this child ([`Child::chain`]).
pub fn compile_function(prog: &Program, fid: FuncId, id: FnId, facts: &Facts, atoms: &SharedAtoms, input: &Input) -> CR<Compiled> {
    let mut c = Fc::new(prog, facts, atoms, input);
    let f = facts.get(id);
    c.strict = prog.func(fid).strict;
    c.is_arrow = f.kind == FuncKind::Arrow;
    c.noctor = matches!(f.kind, FuncKind::Arrow | FuncKind::Method | FuncKind::Getter | FuncKind::Setter);
    c.fn_flags = f.flags;
    c.fn_id = Some(id);
    c.derived = f.kind == FuncKind::DerivedConstructor;
    if f.mode == FuncMode::Generator {
        // A generator is no constructor, and `new.target` is `undefined` in it (`compile_gen.rs`).
        (c.gen_kind, c.noctor) = (GK_GEN, true);
    } else if f.mode == FuncMode::Async {
        (c.gen_kind, c.noctor) = (GK_ASYNC, true);
    } else if f.mode == FuncMode::AsyncGen {
        (c.gen_kind, c.noctor) = (GK_ASYNCGEN, true);
    }
    c.fn_len = prog.list(prog.func(fid).params).take_while(|&p| !matches!(prog.pat(p).kind, PatKind::Assign { .. } | PatKind::Rest(_))).count().min(u16::MAX as usize) as u16;
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
            // `super.x` and `super()` in an arrow read `this` of the function around it.
            if facts.get(k).kind == FuncKind::Arrow && facts.get(k).flags & (facts::THIS | facts::SUPER) != 0 {
                self.cap.entry(Cow::Borrowed("this")).or_default().push(start);
            }
            if facts.get(k).kind == FuncKind::Arrow && facts.get(k).flags & facts::NEW_TARGET != 0 {
                self.cap.entry(Cow::Borrowed("new.target")).or_default().push(start);
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
    // A child with `ready` takes the code that its parent made.
    fn take(c: &mut Compiled) -> Vec<(Child, Option<Compiled>)> {
        let mut inline: Vec<Option<Compiled>> = std::mem::take(&mut c.inline).into_iter().map(Some).collect();
        c.children.iter().map(|ch| (ch.clone(), ch.ready.and_then(|i| inline[i as usize].take()))).collect()
    }
    let mut script = script;
    let mut todo = take(&mut script);
    while let Some((ch, ready)) = todo.pop() {
        let mut c = match ready {
            Some(c) => c,
            None => {
                let (p, fid) = parse_function(source, &mut facts, ch.id, limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
                compile_function(&p, fid, ch.id, &facts, atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone(), ..Default::default() }).map_err(|e| format!("{e:?}"))?
            }
        };
        todo.extend(take(&mut c));
        out.push((ch.id, c));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok((script, out))
}

#[path = "compile_class.rs"]
mod class;
#[path = "compile_async.rs"]
mod async_code;
#[path = "compile_gen.rs"]
mod gen_code;
#[path = "compile_asyncgen.rs"]
mod asyncgen_code;
#[path = "compile_expr.rs"]
mod expr;
#[path = "compile_module.rs"]
mod module;
#[path = "compile_pat.rs"]
mod pat;
#[path = "compile_stmt.rs"]
mod stmt;

#[cfg(test)]
#[path = "compile_tests.rs"]
mod tests;

//! Run a script: lex, parse, compile and interpret (plan steps S2.1 to S2.6, end to end). [`load`] makes a [`Program`] and a [`Vm`] for a
//! script; [`run`] also runs it. The top level of the script is compiled at once. A function inside it is a stub (`Op::Lazy`) that waits
//! for its first call ([`Lazy`]): the pre-parse of the whole text finds every early error at load, and the full parse and the compile of
//! one function happen at its first call. [`compile`] still compiles every function (the examples and the tests of the compiler use it).
//!
//! An unsupported construct stops the load when the top level of the script contains it. In the code of a function
//! it fails at the first call of the function that holds it: the call throws an `Error` ("not supported yet: ...") that a `try` can
//! catch, and a function that nobody calls never fails. A function with `super` in it or in a function inside it is compiled at load
//! (the `CF_LINK` flag of its code is not known before the compile).
//!
//! A child with [`Child::ready`] (the initializers of the fields of a class, `static` blocks) takes its code from `Compiled::inline`
//! of its parent. `Code::funcs` of the compiler holds the id of the child in the facts; the driver replaces it by the index in the
//! program (the facts id of a synthetic function is not always unique).
use crate::ast::{ExprKind, StmtKind};
use crate::atoms::SharedAtoms;
use crate::string::Chars;
use crate::bytecode::{CF_ARGS, CF_GEN, CF_LINK, Code, Ins, Op};
use crate::codes::Codes;
use crate::code_cache::{self, Mode, UnitCode};
use crate::compile::{Child, Compiled, Input, ModInfo, ScriptMode, compile_function, compile_module, compile_script, compile_script_mode};
use crate::facts::{ARGS, EVAL, Facts, FnFacts, FnId, INNER_EVAL, SUPER};
use crate::interp::{Exit, LazyCompile, Program, Vm};
use crate::interp_rt::Realm;
use crate::parse::{Limits, parse_eval_lazy, parse_function, parse_module_lazy, parse_script_lazy};
use crate::shapes::A_E;
use crate::value::Value;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// The stub of a function that waits for its first call, and its one instruction (`Op::Lazy`). A function with plain parameters that does
/// not read `arguments` gets a stub with its parameter registers: the arguments are in them when the stub runs, so the call makes no
/// array. Any other function gets a stub with no parameters and `CF_ARGS`: the call makes the array of all arguments.
/// The stub has the `name` and the `length` of the function (`Realm::fn_props` reads them before the first call).
fn stub(atoms: &SharedAtoms, f: &FnFacts, name: Option<&str>) -> Result<Code, String> {
    fn kept_word(array: bool) -> u32 {
        let mut w = Vec::new();
        crate::bytecode::encode(&mut w, &Ins::new(Op::Lazy, array as u32, 0, 0)).expect("encode Lazy");
        assert_eq!(w.len(), 1, "Lazy is one word");
        w[0]
    }
    let kept = f.simple && f.flags & (ARGS | EVAL | INNER_EVAL) == 0 && f.length <= 64;
    let mut c = if kept { Code::new((f.length as u16).max(1), f.length as u16) } else { Code::new(0, 0) };
    // The two words, made once: the general encoder costs about 500 instructions for each stub.
    static WORDS: OnceLock<[u32; 2]> = OnceLock::new();
    let w = WORDS.get_or_init(|| [kept_word(false), kept_word(true)]);
    c.words = vec![w[!kept as usize]];
    c.flags = if kept { 0 } else { CF_ARGS };
    c.fn_len = f.length.min(u16::MAX as u32) as u16;
    c.span = crate::compile::text_span(f);
    c.noctor = matches!(f.kind, crate::ast::FuncKind::Arrow | crate::ast::FuncKind::Method | crate::ast::FuncKind::Getter | crate::ast::FuncKind::Setter);
    // `prototype` of a generator function inherits the generator prototype, and `new` on it throws: both are read before the first call.
    if f.mode == crate::ast::FuncMode::Generator {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_GEN, true);
    } else if f.mode == crate::ast::FuncMode::Async {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_ASYNC, true);
    } else if f.mode == crate::ast::FuncMode::AsyncGen {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_ASYNCGEN, true);
    }
    if c.gen_kind != 0 {
        c.flags |= CF_GEN;
    }
    if let Some(n) = name {
        let units: Vec<u16> = n.encode_utf16().collect();
        let bytes: Option<Vec<u8>> = units.iter().map(|&u| u8::try_from(u).ok()).collect();
        let s = match &bytes {
            Some(b) => atoms.intern(Chars::One(b)),
            None => atoms.intern(Chars::Two(&units)),
        }
        .map_err(|_| "out of memory")?;
        c.fn_name = 0;
        c.atoms.push(s);
    }
    Ok(c)
}

/// One source text that has stubs: the text and its facts.
struct Unit {
    /// None denotes the static prelude source.
    source: Option<Rc<str>>,
    facts: Facts,
    /// The text is event handler code (`ScriptMode::scoped`): its functions get `Input::scoped`.
    scoped: bool,
    /// The text is the prelude: its functions get `Input::pre`.
    pre: bool,
    /// The line starts of the text, made at its first stack text (`error_stack::Lines`).
    lines: std::cell::OnceCell<crate::error_stack::Lines>,
    /// The URL of this load of the text, for stack frames. The cached codes of a text are shared, so the name is on the unit, not in the
    /// cache. `None`: `<anonymous>` (eval code, `new Function`, a text that the host did not name).
    name: Option<Rc<str>>,
}

/// The functions that wait for their first call: their stubs in `codes`, and what compiles them. The loader makes a stub for a function
/// with no `super` in it or in a function inside it (`CF_LINK` is not known before the compile), and [`LazyCompile::compile`] fills it
/// at the first call. A program has one unit for each text that has stubs (the script and the prelude).
pub struct Lazy {
    units: Vec<Unit>,
    atoms: Arc<SharedAtoms>,
    limits: Limits,
    /// The functions that wait, sorted by the code index of the stub (a new stub is always after the others). A function that was compiled
    /// stays in the list with no `Child`: a search costs `log n`, and no entry moves.
    pending: Vec<Wait>,
    /// The unit of `prelude.js`, made at the first function that a realm asks for (`prelude_fn`).
    pre: Option<usize>,
    /// `(first code index, unit)` of each text and each lazy compile, in the order of the first index: the unit of a code that has no
    /// entry in `pending` (a top level, a child that is ready) is the last span at or before it (`source_of`).
    spans: Vec<(usize, usize)>,
}

/// A function that waits at the code index `ci`, and the unit that holds its text. It keeps the parts of its [`Child`] that the stub
/// does not have: the stub's record in [`Codes`] has its name (`design-code-memory.md`, C4). 24 B, not 64 B and a name.
struct Wait {
    ci: u32,
    unit: u32,
    id: FnId,
    chain: Option<Rc<crate::compile::Level>>,
    self_name: bool,
    /// The function was compiled (or its compile failed): the entry stays, so no entry moves.
    done: bool,
}

impl Wait {
    fn new(ci: usize, unit: usize, ch: Child) -> Wait {
        debug_assert!(ch.ready.is_none(), "a ready child has no stub");
        Wait { ci: ci as u32, unit: unit as u32, id: ch.id, chain: ch.chain, self_name: ch.self_name, done: false }
    }
}

/// The text of a shared atom (a function name: it came from a `&str`, so it has no lone surrogate).
fn atom_text(s: crate::string::Str) -> Box<str> {
    match s.chars().expect("a shared atom is flat") {
        Chars::One(b) => b.iter().map(|&c| c as char).collect(),
        Chars::Two(u) => String::from_utf16_lossy(u).into(),
    }
}

impl Lazy {
    fn new(atoms: Arc<SharedAtoms>, limits: Limits) -> Lazy {
        Lazy { units: Vec::new(), atoms, limits, pending: Vec::new(), pre: None, spans: Vec::new() }
    }

    /// Add a text and the stubs that its loader made.
    fn add(&mut self, source: Rc<str>, facts: Facts, pending: Vec<(usize, Child)>) {
        let u = self.units.len();
        // The text of a new program: its codes start at 0.
        self.spans.push((0, u));
        self.units.push(Unit { source: Some(source), facts, scoped: false, pre: false, lines: Default::default(), name: None });
        self.wait(u, pending);
    }

    /// The unit of the code `ci`: its stub's unit, else the last span at or before it.
    fn unit_of(&self, ci: usize) -> Option<usize> {
        match self.pending.binary_search_by_key(&ci, |w| w.ci as usize) {
            Ok(at) => Some(self.pending[at].unit as usize),
            Err(_) => Some(self.spans.get(self.spans.partition_point(|&(b, _)| b <= ci).checked_sub(1)?)?.1),
        }
    }

    fn wait(&mut self, unit: usize, pending: impl IntoIterator<Item = (usize, Child)>) {
        self.pending.extend(pending.into_iter().map(|(ci, ch)| Wait::new(ci, unit, ch)));
        debug_assert!(self.pending.windows(2).all(|w| w[0].ci < w[1].ci), "the stubs are added in the order of their indexes");
    }
}

// Keep verifier cost out of normal release builds. Conformance and fuzz builds can opt in.
const VERIFY_COMPILED: bool = cfg!(any(debug_assertions, feature = "verify-compiled"));

impl LazyCompile for Lazy {
    fn compile(&mut self, codes: &Codes, ci: usize) -> Result<(), String> {
        let at = self.pending.binary_search_by_key(&ci, |w| w.ci as usize).map_err(|_| "no function waits at this code")?;
        let w = &mut self.pending[at];
        if std::mem::replace(&mut w.done, true) {
            return Err("this function was compiled already".into());
        }
        let name = codes.name_len(ci).0.map(atom_text);
        let ch = Child { id: w.id, chain: w.chain.take(), self_name: w.self_name, name, ready: None };
        let u = w.unit as usize;
        let base = codes.len();
        let unit = &mut self.units[u];
        let source = unit.source.as_deref().unwrap_or(PRELUDE);
        let (p, fid) = parse_function(source, &mut unit.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
        let sub = compile_function(&p, fid, ch.id, &unit.facts, &self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone(), scoped: unit.scoped, pre: unit.pre })
            .map_err(|e| format!("compile fn {}: {e:?}", ch.id))?;
        // The new codes wait in `new` (their indexes start at `base`) and go into the program only when all are made: a failure leaves it as it was.
        // The room is one code for each child and one more, for the code of `ci` (`Codes::set` takes room from the last chunk).
        let (mut new, mut pending) = (Vec::with_capacity(sub.children.len() + 1), Vec::new());
        let mut l = Loader { source, facts: &mut unit.facts, atoms: &self.atoms, limits: self.limits, base, codes: &mut new, lazy: Some(&mut pending), scoped: unit.scoped, pre: unit.pre };
        let mut sub = sub;
        l.children(&mut sub)?;
        let mut code = sub.code;
        code.noctor |= codes[ci].noctor;
        debug_assert!(code.flags & CF_LINK == 0, "a lazy function must not use super");
        debug_assert_eq!(codes.len(), base, "nothing else adds codes during a compile");
        if VERIFY_COMPILED {
            let total = base + new.len();
            for (offset, child) in new.iter().enumerate() {
                crate::verify::verify_at(child, base + offset, total)?;
            }
            crate::verify::verify_at(&code, ci, total)?;
        }
        codes.push_all(new);
        codes.set(ci, code);
        self.spans.push((base, u));
        self.wait(u, pending);
        Ok(())
    }

    fn source_of(&self, ci: usize) -> Option<&str> {
        let u = self.unit_of(ci)?;
        Some(self.units[u].source.as_deref().unwrap_or(PRELUDE))
    }

    fn line_col(&self, ci: usize, off: usize) -> Option<(usize, usize)> {
        let u = &self.units[self.unit_of(ci)?];
        let src = u.source.as_deref().unwrap_or(PRELUDE);
        Some(u.lines.get_or_init(|| crate::error_stack::Lines::new(src)).get(src, off))
    }

    fn set_name(&mut self, ci: usize, name: &str) {
        if let Some(u) = self.unit_of(ci) {
            self.units[u].name = Some(name.into());
        }
    }

    fn name_of(&self, ci: usize) -> Option<&str> {
        self.units[self.unit_of(ci)?].name.as_deref()
    }

    fn page_source_of(&self, ci: usize) -> Option<&str> {
        let u = &self.units[self.unit_of(ci)?];
        if u.pre { None } else { u.source.as_deref() }
    }

    /// A stub for the top-level function `top` of `prelude.js`, added at the end of `codes`. The unit of the prelude is made here, at the first
    /// call: a realm that uses no prelude function never reads the facts.
    fn prelude_fn(&mut self, codes: &Codes, top: &str, name: &str) -> Result<usize, String> {
        let u = match self.pre {
            Some(u) => u,
            None => {
                self.units.push(Unit { source: None, facts: crate::prelude_facts::facts(), scoped: false, pre: true, lines: Default::default(), name: None });
                self.pre = Some(self.units.len() - 1);
                self.units.len() - 1
            }
        };
        let id = crate::prelude_facts::TOP.iter().find(|(n, _)| *n == top).map(|&(_, id)| id).ok_or_else(|| format!("the prelude has no function {top}"))?;
        let mut code = stub(&self.atoms, self.units[u].facts.get(id), Some(name))?;
        // A built-in has no construct slot, even when its body is a function declaration in the prelude.
        code.noctor = true;
        if VERIFY_COMPILED {
            crate::verify::verify_at(&code, codes.len(), codes.len() + 1)?;
        }
        let ci = codes.push_all(vec![code]);
        self.pending.push(Wait { ci: ci as u32, unit: u as u32, id, chain: None, self_name: false, done: false });
        Ok(ci)
    }

    fn script_in(
        &mut self, codes: &Codes, src: &str, mode: ScriptMode,
        chain: Option<Rc<crate::compile::Level>>, strict: bool, new_target: bool,
    ) -> Result<usize, String> {
        if !mode.eval {
            return load_unit(self, codes, src, Mode::Script(mode)).map(|(at, _)| at);
        }
        // Eval parses and compiles against the caller's environment.
        let mut facts = Facts::new();
        let prog = parse_eval_lazy(src, self.limits, &mut facts, strict, new_target)
            .map_err(|e| format!("SyntaxError: {}", e.msg))?;
        let top = crate::compile::compile_script_in(&prog, &facts, &self.atoms, mode, chain).map_err(|e| match e {
            crate::compile::CompileError::Syntax(msg) => format!("SyntaxError: {msg}"),
            _ => format!("compile: {e:?}"),
        })?;
        let (mut new, mut pending) = (Vec::new(), Vec::new());
        Loader { source: src, facts: &mut facts, atoms: &self.atoms, limits: self.limits, base: 0, codes: &mut new, lazy: Some(&mut pending), scoped: mode.scoped, pre: false }.add(top)?;
        let unit = UnitCode { codes: new, facts, pending, info: None };
        bind_unit(self, codes, Rc::from(src), unit, mode.scoped).map(|(at, _)| at)
    }

    fn module(&mut self, codes: &Codes, src: &str) -> Result<(usize, ModInfo), String> {
        let (at, info) = load_unit(self, codes, src, Mode::Module)?;
        Ok((at, info.expect("module metadata")))
    }
}

/// Bind a unit before changing the program. A failure adds nothing.
fn load_unit(lazy: &mut Lazy, codes: &Codes, src: &str, mode: Mode) -> Result<(usize, Option<ModInfo>), String> {
    let scoped = matches!(mode, Mode::Script(s) if s.scoped);
    let (source, unit) =
        code_cache::get_or_compile(src, mode, lazy.limits, &lazy.atoms, || compile_unit(src, mode, lazy.limits, &lazy.atoms))?;
    bind_unit(lazy, codes, source, unit, scoped)
}

fn bind_unit(lazy: &mut Lazy, codes: &Codes, source: Rc<str>, unit: UnitCode, scoped: bool) -> Result<(usize, Option<ModInfo>), String> {
    let UnitCode { codes: mut new, facts, pending, info } = unit;
    let base = codes.len();
    if VERIFY_COMPILED {
        crate::verify::verify_program(&new)?;
    }
    if base.checked_add(new.len() - 1).is_none_or(|last| last > u32::MAX as usize) {
        return Err("too many codes in the realm".into());
    }
    for c in &mut new {
        for child in &mut c.funcs {
            *child += base as u32;
        }
    }
    codes.push_all(new);
    let u = lazy.units.len();
    lazy.spans.push((base, u));
    lazy.units.push(Unit { source: Some(source), facts, scoped, pre: false, lines: Default::default(), name: None });
    lazy.wait(u, pending.into_iter().map(|(i, ch)| (base + i, ch)));
    Ok((base, info))
}

/// Compile a top level and its stubs with unit-relative function indexes.
pub(crate) fn compile_unit(src: &str, mode: Mode, limits: Limits, atoms: &SharedAtoms) -> Result<UnitCode, String> {
    let mut facts = Facts::new();
    let scoped = matches!(mode, Mode::Script(s) if s.scoped);
    let syntax = |e: crate::parse::Error| format!("SyntaxError: {}", e.msg);
    let compile_error = |e| format!("compile: {e:?}");
    let (top, info) = match mode {
        Mode::Script(mode) => {
            let prog = parse_script_lazy(src, limits, &mut facts).map_err(syntax)?;
            // An event handler is one function expression. Its body cannot close the function and go on.
            if scoped {
                let mut body = prog.list(prog.body);
                let one = matches!((body.next(), body.next()), (Some(s), None) if matches!(prog.stmt(s).kind, StmtKind::Expr(e) if matches!(prog.expr(e).kind, ExprKind::Function(_))));
                if !one {
                    return Err("SyntaxError: Unexpected token '}'".into());
                }
            }
            (compile_script_mode(&prog, &facts, atoms, mode).map_err(compile_error)?, None)
        }
        Mode::Module => {
            let prog = parse_module_lazy(src, limits, &mut facts).map_err(syntax)?;
            let (top, info) = compile_module(&prog, &facts, atoms).map_err(compile_error)?;
            (top, Some(info))
        }
    };
    let (mut codes, mut pending) = (Vec::new(), Vec::new());
    Loader { source: src, facts: &mut facts, atoms, limits, base: 0, codes: &mut codes, lazy: Some(&mut pending), scoped, pre: false }.add(top)?;
    Ok(UnitCode { codes, facts, pending, info })
}

struct Loader<'a> {
    source: &'a str,
    facts: &'a mut Facts,
    atoms: &'a SharedAtoms,
    limits: Limits,
    /// The program index of `codes[0]`: the codes before it are in the program already (a lazy compile adds to a program that runs).
    base: usize,
    codes: &'a mut Vec<Code>,
    /// Make a stub for each function that may wait (the `Child` goes here); `None`: compile every function now.
    lazy: Option<&'a mut Vec<(usize, Child)>>,
    /// The text is event handler code (`Input::scoped`).
    scoped: bool,
    /// The text is the prelude (`Input::pre`).
    pre: bool,
}

impl Loader<'_> {
    /// Does function `id`, or a function inside it, use `super`? The code of such a function may get `CF_LINK`, which the stub cannot know.
    fn super_inside(&self, id: FnId) -> bool {
        let f = &self.facts.fns;
        f[f[id as usize].sub as usize..=id as usize].iter().any(|f| f.flags & SUPER != 0)
    }

    /// Add `c` and, below it, its children. Gives the index of `c` in the program.
    fn add(&mut self, c: Compiled) -> Result<usize, String> {
        let at = self.base + self.codes.len();
        self.codes.push(Code::new(0, 0));
        let mut c = c;
        self.children(&mut c)?;
        self.codes[at - self.base] = c.code;
        Ok(at)
    }

    /// Add the children of `c` (a stub or a compiled code each) and put their indexes in `c.code.funcs`. The caller puts `c.code` at its
    /// index (a placeholder or a stub).
    fn children(&mut self, c: &mut Compiled) -> Result<(), String> {
        if !c.unsupported.is_empty() {
            return Err(format!("not supported yet: {:?}", c.unsupported));
        }
        let mut inline: Vec<Option<Compiled>> = std::mem::take(&mut c.inline).into_iter().map(Some).collect();
        let children = std::mem::take(&mut c.children);
        for (i, ch) in children.iter().enumerate() {
            let sub = match ch.ready {
                Some(r) => inline[r as usize].take().ok_or("a ready child is used twice")?,
                None if self.lazy.is_some() && !self.super_inside(ch.id) => {
                    let at = self.base + self.codes.len();
                    c.code.funcs[i] = at as u32;
                    self.lazy.as_mut().expect("lazy").push((at, ch.clone()));
                    self.codes.push(stub(self.atoms, self.facts.get(ch.id), ch.name.as_deref())?);
                    continue;
                }
                None => {
                    let (p, fid) = parse_function(self.source, self.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
                    compile_function(&p, fid, ch.id, self.facts, self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone(), scoped: self.scoped, pre: self.pre }).map_err(|e| format!("compile fn {}: {e:?}", ch.id))?
                }
            };
            let at = self.add(sub)?;
            c.code.flags |= self.codes[at - self.base].flags & CF_LINK;
            c.code.funcs[i] = at as u32;
        }
        Ok(())
    }
}

/// Compile `source` as a script after the codes that `codes` has. The top level is the code at index `codes.len()` before the call.
fn compile_after(mut codes: Vec<Code>, source: &str, atoms: &SharedAtoms, limits: Limits) -> Result<Vec<Code>, String> {
    let mut facts = Facts::new();
    let prog = parse_script_lazy(source, limits, &mut facts).map_err(|e| format!("SyntaxError: {}", e.msg))?;
    let script = compile_script(&prog, &facts, atoms).map_err(|e| format!("compile: {e:?}"))?;
    Loader { source, facts: &mut facts, atoms, limits, base: 0, codes: &mut codes, lazy: None, scoped: false, pre: false }.add(script)?;
    Ok(codes)
}

/// Compile `source` as a script. The top level is `Program::codes[0]`.
pub fn compile(source: &str, atoms: &SharedAtoms, limits: Limits) -> Result<Program, String> {
    Ok(Program { codes: compile_after(Vec::new(), source, atoms, limits)? })
}

/// An `eval_load` program (statements and then one expression) as a script: the value of the last expression goes to the global
/// `__result`. The last statement boundary is the last `;` or `}` at brace depth 0; quotes are skipped.
pub fn eval_program(src: &str) -> String {
    let t = src.trim_end();
    let (mut depth, mut quote, mut at) = (0i32, None, 0);
    for (k, c) in t.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '{' | '(' | '[') => depth += 1,
            (None, '}' | ')' | ']') => {
                depth -= 1;
                if c == '}' && depth == 0 {
                    at = k + 1;
                }
            }
            (None, ';') if depth == 0 => at = k + 1,
            _ => {}
        }
    }
    format!("{}\nvar __result = ({});", &t[..at], t[at..].trim())
}

/// The built-ins that call script functions (`builtins.rs`), written in JavaScript.
pub const PRELUDE: &str = include_str!("prelude.js");

/// Compile `source` and make a [`Vm`] that can run it (`Vm::run_main(0)`). The prelude is not compiled or run: `Array.prototype.forEach` and
/// the others are lazy table entries that make a stub at their first lookup (`prelude_pre.rs`).
pub fn load(source: &str) -> Result<Vm, String> {
    load_with("", source)
}

/// [`load`] with a host script that has run when this returns, before `source` (a test harness: it sets globals, and `source` keeps its
/// own directive prologue).
pub fn load_with(host: &str, source: &str) -> Result<Vm, String> {
    code_cache::new_realm();
    let shared = Arc::new(SharedAtoms::default());
    let mut lazy = Lazy::new(shared.clone(), Limits::default());
    let (mode, limits) = (Mode::Script(ScriptMode::default()), Limits::default());
    let (source, UnitCode { codes, facts, pending, .. }) =
        code_cache::get_or_compile(source, mode, limits, &shared, || compile_unit(source, mode, limits, &shared))?;
    lazy.add(source, facts, pending);
    let at = codes.len();
    let mut prog = Program { codes };
    if !host.is_empty() {
        prog.codes = compile_after(prog.codes, host, &shared, Limits::default()).map_err(|e| format!("host script: {e}"))?;
    }
    if VERIFY_COMPILED {
        crate::verify::verify_program(&prog.codes)?;
    }
    let mut vm = Vm::new_compiled(prog, shared);
    vm.realm.lazy = Some(Box::new(lazy));
    if !host.is_empty()
        && let Err(e) = vm.run_main(at)
    {
        return Err(format!("host script failed: {e:?}"));
    }
    Ok(vm)
}

/// Compile and run `source`. On `Ok` the [`Vm`] holds the globals that the script made. An uncaught exception is `Err` with its text.
pub fn run(source: &str) -> Result<Vm, String> {
    let mut vm = load(source)?;
    match vm.run_main(0) {
        Ok(_) => Ok(vm),
        Err(Exit::Throw(e)) => Err(vm.display(e, false)),
        Err(Exit::Fuel) => Err("the budget ran out".into()),
    }
}

impl Vm {
    /// The global `name` (`undefined` when it is not there).
    pub fn global(&mut self, name: &str) -> Value {
        let k = self.realm.intern(name);
        let g = self.realm.global.value();
        self.realm.get_prop(g, crate::interp_rt::Key::Name(k)).unwrap_or(Value::UNDEFINED)
    }

    /// The global `name` as text (`display`).
    pub fn global_text(&mut self, name: &str) -> String {
        let v = self.global(name);
        self.display(v, false)
    }

    /// A value as text for a person or a test: a string is bare at the top and quoted inside; an array is `[1,2]`, an object `{a:1}`, an
    /// error `Name: message`, a function `function`. A nesting past 6 levels is `...`.
    pub fn display(&mut self, v: Value, quoted: bool) -> String {
        self.display_at(v, quoted, 0)
    }

    fn display_at(&mut self, v: Value, quoted: bool, depth: u32) -> String {
        if let Some(s) = Realm::str_of(v) {
            let s = self.realm.flat(s);
            let t = self.realm.text(s);
            return if quoted { format!("{t:?}") } else { t };
        }
        let r = &self.realm;
        if let Some(i) = v.as_int() {
            return i.to_string();
        }
        if let Some(f) = v.as_f64() {
            return Realm::num_to_string(f);
        }
        if v.is_undefined() {
            return "undefined".into();
        }
        if v.is_null() {
            return "null".into();
        }
        if let Some(b) = v.as_bool() {
            return b.to_string();
        }
        if let Some(b) = Realm::big_of(v) {
            return crate::bigint::to_string_radix(b.bigint(), 10);
        }
        let Some(o) = Realm::obj(v) else { return format!("{v:?}") };
        if o.is_callable() {
            return "function".into();
        }
        if depth > 6 {
            return "...".into();
        }
        let id = o.shape(&r.shapes);
        if r.is_error(v) {
            return r.error_text(v);
        }
        let mut parts: Vec<String> = Vec::new();
        let indexed = r.shapes.get(id).layout == crate::cell::Layout::Indexed;
        let entries: Vec<_> = r.shapes.entries(id).filter(|e| e.attrs() & A_E != 0 && !e.key().is_symbol()).map(|e| (r.text(e.key()), o.slot(&r.shapes, e.slot()))).collect();
        let elems: Vec<Value> = if indexed { (0..o.len()).map(|i| o.get_element(i).unwrap_or(Value::HOLE)).collect() } else { vec![] };
        for e in elems {
            parts.push(if e.is_hole() { String::new() } else { self.display_at(e, true, depth + 1) });
        }
        if indexed {
            for (k, val) in entries {
                let t = self.display_at(val, true, depth + 1);
                parts.push(format!("{k}:{t}"));
            }
            return format!("[{}]", parts.join(","));
        }
        for (k, val) in entries {
            let t = self.display_at(val, true, depth + 1);
            parts.push(format!("{k}:{t}"));
        }
        format!("{{{}}}", parts.join(","))
    }
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod conversion_tests;

#[cfg(test)]
#[path = "fn_source_tests.rs"]
mod fn_source_tests;

#[cfg(test)]
#[path = "feedback_slots_tests.rs"]
mod feedback_slots_tests;
#[cfg(test)]
#[path = "proto_ic_tests.rs"]
mod proto_ic_tests;
#[cfg(test)]
#[path = "regexp_rt_tests.rs"]
mod regexp_rt_tests;
#[cfg(test)]
#[path = "bi_string_regexp_tests.rs"]
mod bi_string_regexp_tests;
#[cfg(test)]
#[path = "bi_uri_tests.rs"]
mod bi_uri_tests;

#[cfg(test)]
#[path = "script_array_push_tests.rs"]
mod array_push_tests;

#[cfg(test)]
#[path = "reenter_depth_tests.rs"]
mod reenter_depth_tests;

#[cfg(test)]
#[path = "script_key_add_tests.rs"]
mod key_add_tests;

#[cfg(test)]
#[path = "script_foreach_step_tests.rs"]
mod foreach_step_tests;

#[cfg(test)]
#[path = "forin_shape_tests.rs"]
mod forin_shape_tests;

#[cfg(test)]
#[path = "bi_object_forin_snapshot_tests.rs"]
mod forin_snapshot_tests;

#[cfg(test)]
#[path = "regexp_test_fast_tests.rs"]
mod regexp_test_fast_tests;

#[cfg(test)]
#[path = "stub_records_tests.rs"]
mod stub_records_tests;

#[cfg(test)]
#[path = "lazy_entry_tests.rs"]
mod lazy_entry_tests;

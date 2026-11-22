//! Run a script: lex, parse, compile and interpret (plan steps S2.1 to S2.6, end to end). [`load`] makes a [`Program`] and a [`Vm`] for a
//! script; [`run`] also runs it. The top level of the script is compiled at once. A function inside it is a stub (`Op::Lazy`) that waits
//! for its first call ([`Lazy`]): the pre-parse of the whole text finds every early error at load, and the full parse and the compile of
//! one function happen at its first call. [`compile`] still compiles every function (the examples and the tests of the compiler use it).
//!
//! What a lazy function changes: a construct that the compiler does not build yet (direct `eval`, `with`, a generator or `async` function,
//! ...) stops the load when the top level code of the script holds it (a generator declared there counts). In the code of a function
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
use crate::bytecode::{CF_ARGS, CF_LINK, Code, Ins, Op};
use crate::codes::Codes;
use crate::compile::{Child, Compiled, Input, ModInfo, ScriptMode, compile_function, compile_module, compile_script, compile_script_mode};
use crate::facts::{ARGS, EVAL, Facts, FnFacts, FnId, INNER_EVAL, SUPER};
use crate::interp::{Exit, LazyCompile, Program, Vm, code_flags};
use crate::interp_rt::Realm;
use crate::parse::{Limits, parse_function, parse_module_lazy, parse_script_lazy};
use crate::shapes::A_E;
use crate::value::Value;
use std::borrow::Cow;
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
    c.noctor = matches!(f.kind, crate::ast::FuncKind::Arrow | crate::ast::FuncKind::Method | crate::ast::FuncKind::Getter | crate::ast::FuncKind::Setter);
    // `prototype` of a generator function inherits the generator prototype, and `new` on it throws: both are read before the first call.
    if f.mode == crate::ast::FuncMode::Generator {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_GEN, true);
    } else if f.mode == crate::ast::FuncMode::Async {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_ASYNC, true);
    } else if f.mode == crate::ast::FuncMode::AsyncGen {
        (c.gen_kind, c.noctor) = (crate::bytecode::GK_ASYNCGEN, true);
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
    source: Cow<'static, str>,
    facts: Facts,
    /// The text is event handler code (`ScriptMode::scoped`): its functions get `Input::scoped`.
    scoped: bool,
    /// The text is the prelude: its functions get `Input::pre`.
    pre: bool,
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
}

/// A function that waits at the code index `ci`, and the unit that holds its text.
struct Wait {
    ci: usize,
    unit: usize,
    child: Option<Child>,
}

impl Lazy {
    fn new(atoms: Arc<SharedAtoms>, limits: Limits) -> Lazy {
        Lazy { units: Vec::new(), atoms, limits, pending: Vec::new(), pre: None }
    }

    /// Add a text and the stubs that its loader made.
    fn add(&mut self, source: Cow<'static, str>, facts: Facts, pending: Vec<(usize, Child)>) {
        let u = self.units.len();
        self.units.push(Unit { source, facts, scoped: false, pre: false });
        self.wait(u, pending);
    }

    fn wait(&mut self, unit: usize, pending: Vec<(usize, Child)>) {
        self.pending.extend(pending.into_iter().map(|(ci, ch)| Wait { ci, unit, child: Some(ch) }));
        debug_assert!(self.pending.windows(2).all(|w| w[0].ci < w[1].ci), "the stubs are added in the order of their indexes");
    }
}

impl LazyCompile for Lazy {
    fn compile(&mut self, codes: &Codes, ci: usize) -> Result<(), String> {
        let at = self.pending.binary_search_by_key(&ci, |w| w.ci).map_err(|_| "no function waits at this code")?;
        let w = &mut self.pending[at];
        let (u, ch) = (w.unit, w.child.take().ok_or("this function was compiled already")?);
        let base = codes.len();
        let unit = &mut self.units[u];
        let (p, fid) = parse_function(&unit.source, &mut unit.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
        let sub = compile_function(&p, fid, ch.id, &unit.facts, &self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone(), scoped: unit.scoped, pre: unit.pre })
            .map_err(|e| format!("compile fn {}: {e:?}", ch.id))?;
        // The new codes wait in `new` (their indexes start at `base`) and go into the program only when all are made: a failure leaves it as it was.
        // The room is one code for each child and one more, for the code of `ci` (`Codes::set` takes room from the last chunk).
        let (mut new, mut pending) = (Vec::with_capacity(sub.children.len() + 1), Vec::new());
        let mut l = Loader { source: &unit.source, facts: &mut unit.facts, atoms: &self.atoms, limits: self.limits, base, codes: &mut new, lazy: Some(&mut pending), scoped: unit.scoped, pre: unit.pre };
        let mut sub = sub;
        l.children(&mut sub)?;
        let mut code = sub.code;
        code.noctor |= codes[ci].noctor;
        // The flags of the new codes: the children come after their parent, so the last come first (a stub has no `super`).
        let link = |new: &[Code], c: &Code| c.funcs.iter().any(|&k| k as usize >= base && new[k as usize - base].flags & CF_LINK != 0);
        for i in (0..new.len()).rev() {
            new[i].flags = code_flags(&new[i]) | if link(&new, &new[i]) { CF_LINK } else { 0 };
        }
        code.flags = code_flags(&code) | if link(&new, &code) { CF_LINK } else { 0 };
        debug_assert!(code.flags & CF_LINK == 0, "a lazy function must not use super");
        debug_assert_eq!(codes.len(), base, "nothing else adds codes during a compile");
        codes.push_all(new);
        codes.set(ci, code);
        self.wait(u, pending);
        Ok(())
    }

    /// A stub for the top-level function `top` of `prelude.js`, added at the end of `codes`. The unit of the prelude is made here, at the first
    /// call: a realm that uses no prelude function never reads the facts.
    fn prelude_fn(&mut self, codes: &Codes, top: &str, name: &str) -> Result<usize, String> {
        let u = match self.pre {
            Some(u) => u,
            None => {
                self.units.push(Unit { source: Cow::Borrowed(PRELUDE), facts: crate::prelude_facts::facts(), scoped: false, pre: true });
                self.pre = Some(self.units.len() - 1);
                self.units.len() - 1
            }
        };
        let id = crate::prelude_facts::TOP.iter().find(|(n, _)| *n == top).map(|&(_, id)| id).ok_or_else(|| format!("the prelude has no function {top}"))?;
        let mut code = stub(&self.atoms, self.units[u].facts.get(id), Some(name))?;
        // A built-in has no construct slot, even when its body is a function declaration in the prelude.
        code.noctor = true;
        code.flags = code_flags(&code);
        let ci = codes.push_all(vec![code]);
        self.pending.push(Wait { ci, unit: u, child: Some(Child { id, chain: None, self_name: false, name: Some(name.into()), ready: None }) });
        Ok(ci)
    }

    /// The top level of a script that joins a program that runs: its code is the last one that this adds, and the functions inside are
    /// stubs. The flags are as `compile` makes them. A failure adds nothing.
    fn script(&mut self, codes: &Codes, src: &str, mode: ScriptMode) -> Result<usize, String> {
        let mut facts = Facts::new();
        let prog = parse_script_lazy(src, self.limits, &mut facts).map_err(|e| format!("SyntaxError: {}", e.msg))?;
        // The text of an event handler is one function expression, so a body cannot close the function and go on (`compile_function`).
        if mode.scoped {
            let mut body = prog.list(prog.body);
            let one = matches!((body.next(), body.next()), (Some(s), None) if matches!(prog.stmt(s).kind, StmtKind::Expr(e) if matches!(prog.expr(e).kind, ExprKind::Function(_))));
            if !one {
                return Err("SyntaxError: Unexpected token '}'".into());
            }
        }
        let top = compile_script_mode(&prog, &facts, &self.atoms, mode).map_err(|e| format!("compile: {e:?}"))?;
        let (base, mut new, mut pending) = (codes.len(), Vec::new(), Vec::new());
        let at = Loader { source: src, facts: &mut facts, atoms: &self.atoms, limits: self.limits, base, codes: &mut new, lazy: Some(&mut pending), scoped: mode.scoped, pre: false }.add(top)?;
        let link = |new: &[Code], c: &Code| c.funcs.iter().any(|&k| k as usize >= base && new[k as usize - base].flags & CF_LINK != 0);
        for i in (0..new.len()).rev() {
            new[i].flags = code_flags(&new[i]) | if link(&new, &new[i]) { CF_LINK } else { 0 };
        }
        debug_assert_eq!(at, base);
        codes.push_all(new);
        let u = self.units.len();
        self.units.push(Unit { source: Cow::Owned(src.to_owned()), facts, scoped: mode.scoped, pre: false });
        self.wait(u, pending);
        Ok(at)
    }

    /// The top level of a module that joins a program that runs: its code is the last one that this adds. As `script`, with the facts of
    /// the module.
    fn module(&mut self, codes: &Codes, src: &str) -> Result<(usize, ModInfo), String> {
        let mut facts = Facts::new();
        let prog = parse_module_lazy(src, self.limits, &mut facts).map_err(|e| format!("SyntaxError: {}", e.msg))?;
        let (top, info) = compile_module(&prog, &facts, &self.atoms).map_err(|e| format!("compile: {e:?}"))?;
        let (base, mut new, mut pending) = (codes.len(), Vec::new(), Vec::new());
        let at = Loader { source: src, facts: &mut facts, atoms: &self.atoms, limits: self.limits, base, codes: &mut new, lazy: Some(&mut pending), scoped: false, pre: false }.add(top)?;
        let link = |new: &[Code], c: &Code| c.funcs.iter().any(|&k| k as usize >= base && new[k as usize - base].flags & CF_LINK != 0);
        for i in (0..new.len()).rev() {
            new[i].flags = code_flags(&new[i]) | if link(&new, &new[i]) { CF_LINK } else { 0 };
        }
        debug_assert_eq!(at, base);
        codes.push_all(new);
        let u = self.units.len();
        self.units.push(Unit { source: Cow::Owned(src.to_owned()), facts, scoped: false, pre: false });
        self.wait(u, pending);
        Ok((at, info))
    }
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
            c.code.funcs[i] = self.add(sub)? as u32;
        }
        Ok(())
    }
}

/// Compile `source` as a script after the codes that `codes` has. The top level is the code at index `codes.len()` before the call.
/// With `lazy` the functions inside the top level are stubs, and `lazy` gets what makes them.
fn compile_after(mut codes: Vec<Code>, source: &str, atoms: &SharedAtoms, limits: Limits, lazy: Option<&mut Vec<(usize, Child)>>) -> Result<(Vec<Code>, Facts), String> {
    let mut facts = Facts::new();
    let prog = parse_script_lazy(source, limits, &mut facts).map_err(|e| format!("SyntaxError: {}", e.msg))?;
    let script = compile_script(&prog, &facts, atoms).map_err(|e| format!("compile: {e:?}"))?;
    Loader { source, facts: &mut facts, atoms, limits, base: 0, codes: &mut codes, lazy, scoped: false, pre: false }.add(script)?;
    Ok((codes, facts))
}

/// Compile `source` as a script. The top level is `Program::codes[0]`.
pub fn compile(source: &str, atoms: &SharedAtoms, limits: Limits) -> Result<Program, String> {
    Ok(Program { codes: compile_after(Vec::new(), source, atoms, limits, None)?.0 })
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
    let shared = Arc::new(SharedAtoms::default());
    let mut lazy = Lazy::new(shared.clone(), Limits::default());
    let mut pending = Vec::new();
    let (codes, facts) = compile_after(Vec::new(), source, &shared, Limits::default(), Some(&mut pending))?;
    lazy.add(Cow::Owned(source.to_owned()), facts, pending);
    let at = codes.len();
    let mut prog = Program { codes };
    if !host.is_empty() {
        prog.codes = compile_after(prog.codes, host, &shared, Limits::default(), None).map_err(|e| format!("host script: {e}"))?.0;
    }
    let mut vm = Vm::new(prog, shared);
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

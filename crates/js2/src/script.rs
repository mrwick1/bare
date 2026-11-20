//! Run a script: lex, parse, compile and interpret (plan steps S2.1 to S2.6, end to end). [`load`] compiles a whole script (the top level
//! and every function inside it, eagerly) into a [`Program`] and makes a [`Vm`] for it; [`run`] also runs it. There is no lazy compile
//! yet: the function table of the interpreter is fixed when the [`Vm`] is made, so the driver compiles each function before the run.
//! A function that has an unbuilt construct (a generator, `async`, `with`, direct `eval`) stops the load with an error.
//!
//! A child with [`Child::ready`] (the initializers of the fields of a class, `static` blocks) takes its code from `Compiled::inline`
//! of its parent. `Code::funcs` of the compiler holds the id of the child in the facts; the driver replaces it by the index in the
//! program (the facts id of a synthetic function is not always unique).
use crate::atoms::SharedAtoms;
use crate::string::Chars;
use crate::bytecode::{CF_ARGS, CF_LINK, Code, Op};
use crate::compile::{Child, Compiled, Input, compile_function, compile_script};
use crate::facts::{Facts, SUPER};
use crate::interp::{Exit, LazyCompile, Program, Vm, code_flags};
use crate::interp_rt::Realm;
use crate::parse::{Limits, parse_function, parse_script_known, parse_script_lazy};
use crate::shapes::A_E;
use crate::value::Value;
use std::sync::Arc;

/// The stub of a function that waits for its first call: no parameters, all arguments in an array, and one instruction (`Op::Lazy`).
/// It has the `name` and the `length` of the function (`Realm::fn_props` reads them before the first call).
fn stub(atoms: &SharedAtoms, name: Option<&str>, length: u32) -> Result<Code, String> {
    let mut c = Code::new(0, 0);
    c.words = vec![Op::Lazy as u32];
    c.flags = CF_ARGS;
    c.fn_len = length.min(u16::MAX as u32) as u16;
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

/// The functions that wait for their first call: their stubs in `codes`, and what compiles them. The loader makes a stub for a function
/// that no `super` reaches (`CF_LINK` is not known before the compile), and [`LazyCompile::compile`] fills it at the first call.
pub struct Lazy {
    source: &'static str,
    facts: Facts,
    atoms: Arc<SharedAtoms>,
    limits: Limits,
    /// `(index of the stub, the function)`, sorted by the index.
    pending: Vec<(usize, Child)>,
}

impl LazyCompile for Lazy {
    fn compile(&mut self, codes: &mut Vec<Code>, ci: usize) -> Result<(), String> {
        let at = self.pending.binary_search_by_key(&ci, |p| p.0).map_err(|_| "no function waits at this code")?;
        let ch = self.pending.remove(at).1;
        let first = codes.len();
        let (p, fid) = parse_function(self.source, &mut self.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
        let sub = compile_function(&p, fid, ch.id, &self.facts, &self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone() })
            .map_err(|e| format!("compile fn {}: {e:?}", ch.id))?;
        let mut pending = Vec::new();
        let mut l = Loader { source: self.source, facts: &mut self.facts, atoms: &self.atoms, limits: self.limits, codes, lazy: Some(&mut pending) };
        l.fill(ci, sub)?;
        // The flags of the new codes: the children come after their parent, so the last come first (a stub has no `super`).
        for i in (first..codes.len()).rev().chain([ci]) {
            let f = code_flags(&codes[i]);
            let from_children = codes[i].funcs.iter().any(|&k| codes.get(k as usize).is_some_and(|c| c.flags & CF_LINK != 0));
            codes[i].flags = f | if from_children { CF_LINK } else { 0 };
        }
        debug_assert!(codes[ci].flags & CF_LINK == 0, "a lazy function must not use super");
        self.pending.extend(pending);
        Ok(())
    }
}

struct Loader<'a> {
    source: &'a str,
    facts: &'a mut Facts,
    atoms: &'a SharedAtoms,
    limits: Limits,
    codes: &'a mut Vec<Code>,
    /// Make a stub for each function that may wait (the `Child` goes here); `None`: compile every function now.
    lazy: Option<&'a mut Vec<(usize, Child)>>,
}

impl Loader<'_> {
    /// Add `c` and, below it, its children. Gives the index of `c` in the program.
    fn add(&mut self, c: Compiled) -> Result<usize, String> {
        let at = self.codes.len();
        self.codes.push(Code::new(0, 0));
        self.fill(at, c)?;
        Ok(at)
    }

    /// Put `c` at the index `at` (a placeholder or a stub) and add its children.
    fn fill(&mut self, at: usize, mut c: Compiled) -> Result<(), String> {
        if !c.unsupported.is_empty() {
            return Err(format!("not supported yet: {:?}", c.unsupported));
        }
        let mut inline: Vec<Option<Compiled>> = std::mem::take(&mut c.inline).into_iter().map(Some).collect();
        let children = std::mem::take(&mut c.children);
        for (i, ch) in children.iter().enumerate() {
            let sub = match ch.ready {
                Some(r) => inline[r as usize].take().ok_or("a ready child is used twice")?,
                None if self.lazy.is_some() && self.facts.get(ch.id).flags & SUPER == 0 => {
                    c.code.funcs[i] = self.codes.len() as u32;
                    self.lazy.as_mut().expect("lazy").push((self.codes.len(), ch.clone()));
                    let (name, length) = (ch.name.as_deref(), self.facts.get(ch.id).length);
                    self.codes.push(stub(self.atoms, name, length)?);
                    continue;
                }
                None => {
                    let (p, fid) = parse_function(self.source, self.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
                    compile_function(&p, fid, ch.id, self.facts, self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name, name: ch.name.clone() }).map_err(|e| format!("compile fn {}: {e:?}", ch.id))?
                }
            };
            c.code.funcs[i] = self.add(sub)? as u32;
        }
        self.codes[at] = c.code;
        Ok(())
    }
}

/// Compile `source` as a script after the codes that `codes` has. The top level is the code at index `codes.len()` before the call.
/// With `lazy` the functions inside the top level are stubs, and `lazy` gets what makes them. With `known` (the prelude) the facts of
/// the text are not made by a pre-parse: they are the generated ones (`prelude_facts.rs`, a test keeps them equal to a pre-parse).
fn compile_after(mut codes: Vec<Code>, source: &str, atoms: &SharedAtoms, limits: Limits, lazy: Option<&mut Vec<(usize, Child)>>, known: Option<Facts>) -> Result<(Vec<Code>, Facts), String> {
    let (mut facts, prog) = match known {
        Some(mut f) => {
            let p = parse_script_known(source, limits, &mut f).map_err(|e| format!("SyntaxError: {}", e.msg))?;
            (f, p)
        }
        None => {
            let mut f = Facts::new();
            let p = parse_script_lazy(source, limits, &mut f).map_err(|e| format!("SyntaxError: {}", e.msg))?;
            (f, p)
        }
    };
    let script = compile_script(&prog, &facts, atoms).map_err(|e| format!("compile: {e:?}"))?;
    Loader { source, facts: &mut facts, atoms, limits, codes: &mut codes, lazy }.add(script)?;
    Ok((codes, facts))
}

/// Compile `source` as a script. The top level is `Program::codes[0]`.
pub fn compile(source: &str, atoms: &SharedAtoms, limits: Limits) -> Result<Program, String> {
    Ok(Program { codes: compile_after(Vec::new(), source, atoms, limits, None, None)?.0 })
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

/// Compile `source` and make a [`Vm`] that can run it (`Vm::run_main(0)`). The prelude is compiled after the script, and it has run
/// when this returns, so `Array.prototype.forEach` and the others are there.
pub fn load(source: &str) -> Result<Vm, String> {
    load_with("", source)
}

/// [`load`] with a host script that has run when this returns, after the prelude and before `source` (a test harness: it sets globals,
/// and `source` keeps its own directive prologue).
pub fn load_with(host: &str, source: &str) -> Result<Vm, String> {
    let shared = Arc::new(SharedAtoms::default());
    let mut prog = compile(source, &shared, Limits::default())?;
    let pre = prog.codes.len();
    let mut pending = Vec::new();
    let (codes, facts) = compile_after(prog.codes, PRELUDE, &shared, Limits::default(), Some(&mut pending), Some(crate::prelude_facts::facts())).map_err(|e| format!("prelude: {e} (is prelude_facts.rs out of date? run `BLESS_PRELUDE_FACTS=1 cargo test -p bare-js2 --lib prelude_facts`)"))?;
    prog.codes = codes;
    let at = prog.codes.len();
    if !host.is_empty() {
        prog.codes = compile_after(prog.codes, host, &shared, Limits::default(), None, None).map_err(|e| format!("host script: {e}"))?.0;
    }
    let mut vm = Vm::new(prog, shared.clone());
    vm.realm.lazy = Some(Box::new(Lazy { source: PRELUDE, facts, atoms: shared, limits: Limits::default(), pending }));
    if let Err(e) = vm.run_main(pre) {
        panic!("the prelude failed: {e:?}");
    }
    // Hide the names of the prelude, do not delete them: a delete of a property that is not the last one puts the global object in dictionary
    // mode, and then no global access can be cached. A property is not enumerable; its value is `undefined`, so a script cannot reach the function.
    let g = vm.realm.global;
    for name in ["__def", "__helper", "__set"] {
        let k = vm.realm.intern(name);
        vm.realm.define(g, crate::interp_rt::Key::Name(k), Value::UNDEFINED).expect("define");
    }
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
        let entries: Vec<_> = r.shapes.entries(id).filter(|e| e.attrs() & A_E != 0).map(|e| (r.text(e.key()), o.slot(&r.shapes, e.slot()))).collect();
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

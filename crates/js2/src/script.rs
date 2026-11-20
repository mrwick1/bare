//! Run a script: lex, parse, compile and interpret (plan steps S2.1 to S2.6, end to end). [`load`] compiles a whole script (the top level
//! and every function inside it, eagerly) into a [`Program`] and makes a [`Vm`] for it; [`run`] also runs it. There is no lazy compile
//! yet: the function table of the interpreter is fixed when the [`Vm`] is made, so the driver compiles each function before the run.
//! A function that has an unbuilt construct (a generator, `async`, `with`, direct `eval`) stops the load with an error.
//!
//! A child with [`Child::ready`] (the initializers of the fields of a class, `static` blocks) takes its code from `Compiled::inline`
//! of its parent. `Code::funcs` of the compiler holds the id of the child in the facts; the driver replaces it by the index in the
//! program (the facts id of a synthetic function is not always unique).
use crate::atoms::SharedAtoms;
use crate::bytecode::Code;
use crate::compile::{Compiled, Input, compile_function, compile_script};
use crate::facts::Facts;
use crate::interp::{Exit, Program, Vm};
use crate::interp_rt::Realm;
use crate::parse::{Limits, parse_function, parse_script_lazy};
use crate::shapes::A_E;
use crate::value::Value;
use std::sync::Arc;

struct Loader<'a> {
    source: &'a str,
    facts: Facts,
    atoms: &'a SharedAtoms,
    limits: Limits,
    codes: Vec<Code>,
}

impl Loader<'_> {
    /// Add `c` and, below it, its children. Gives the index of `c` in the program.
    fn add(&mut self, mut c: Compiled) -> Result<usize, String> {
        if !c.unsupported.is_empty() {
            return Err(format!("not supported yet: {:?}", c.unsupported));
        }
        let at = self.codes.len();
        self.codes.push(Code::new(0, 0));
        let mut inline: Vec<Option<Compiled>> = std::mem::take(&mut c.inline).into_iter().map(Some).collect();
        let children = std::mem::take(&mut c.children);
        for (i, ch) in children.iter().enumerate() {
            let sub = match ch.ready {
                Some(r) => inline[r as usize].take().ok_or("a ready child is used twice")?,
                None => {
                    let (p, fid) = parse_function(self.source, &mut self.facts, ch.id, self.limits).map_err(|e| format!("parse fn {}: {}", ch.id, e.msg))?;
                    compile_function(&p, fid, ch.id, &self.facts, self.atoms, &Input { chain: ch.chain.clone(), self_name: ch.self_name }).map_err(|e| format!("compile fn {}: {e:?}", ch.id))?
                }
            };
            c.code.funcs[i] = self.add(sub)? as u32;
        }
        self.codes[at] = c.code;
        Ok(at)
    }
}

/// Compile `source` as a script after the codes that `codes` has. The top level is the code at index `codes.len()` before the call.
fn compile_after(codes: Vec<Code>, source: &str, atoms: &SharedAtoms, limits: Limits) -> Result<Vec<Code>, String> {
    let mut facts = Facts::new();
    let prog = parse_script_lazy(source, limits, &mut facts).map_err(|e| format!("SyntaxError: {}", e.msg))?;
    let script = compile_script(&prog, &facts, atoms).map_err(|e| format!("compile: {e:?}"))?;
    let mut l = Loader { source, facts, atoms, limits, codes };
    l.add(script)?;
    Ok(l.codes)
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
    prog.codes = compile_after(prog.codes, PRELUDE, &shared, Limits::default()).map_err(|e| format!("prelude: {e}"))?;
    let at = prog.codes.len();
    if !host.is_empty() {
        prog.codes = compile_after(prog.codes, host, &shared, Limits::default()).map_err(|e| format!("host script: {e}"))?;
    }
    let mut vm = Vm::new(prog, shared);
    if let Err(e) = vm.run_main(pre) {
        panic!("the prelude failed: {e:?}");
    }
    // Hide `__def`, do not delete it: a delete of a property that is not the last one puts the global object in dictionary mode, and
    // then no global access can be cached. The property is not enumerable; its value is `undefined`, so a script cannot reach the function.
    let k = vm.realm.intern("__def");
    let g = vm.realm.global;
    vm.realm.define(g, crate::interp_rt::Key::Name(k), Value::UNDEFINED).expect("define");
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

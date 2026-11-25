//! The script and iterator methods of `Engine for Realm` (`api.rs`; `docs/internal/design-js2-host-api.md` 3.8 and 10).
//!
//! **A script with a value.** `run_script` and `eval_global` compile the text with `ScriptMode::value`: the top level keeps a completion
//! register and returns it (see `compile_stmt.rs`: the value of the last expression statement that ran, reset to `undefined` by a
//! statement with a body; the old engine has the same rule, it is not the full UpdateEmpty). A script that nobody asks for a value
//! (`Realm::run_text`) is compiled as before.
//!
//! **Eval.** Call the intrinsic with a string to compile a script. Direct eval starts with the caller's lexical chain.
//! Sloppy eval declares vars and functions in the caller's variable environment, or at global scope for indirect eval.
//! Strict eval keeps its declarations local.
//!
//! **Event handlers.** `compile_function` compiles `(function NAME(PARAMS) {\nBODY\n})` as a script with `ScriptMode::scoped`: the
//! top level and every function inside it read, write and `typeof` a free name by `GetName`, `SetName` and `TypeofName`. Those look in
//! the objects of the running `call_scoped` (`ApiState::scope`, innermost first) after the global lexical bindings and before the
//! global object. A method found there is called with its holder as `this` (a bound function), as in the old engine. Code of other
//! scripts never sees the scope, and a function that outlives the call does not either. The parsed code is kept by text
//! (`ApiState::handlers`), so the same attribute on many elements compiles once.
//!
//! **Iterators.** An array and a string are stepped by index with the position in `Iter::state` (as `Op::IterOpen` does); any other
//! value goes through `Symbol.iterator`, `next` and `return`. `state[0]` is the kind, `state[1]` the position, `state[2]` is 1 when done.
use crate::bi_symbol::ITERATOR;
use crate::builtins::HIDDEN;
use crate::cell::Layout;
use crate::compile::ScriptMode;
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::Obj;
use crate::string::{Chars, Str};
use crate::value::Value;
use bare_js_api::{Abort, Engine, Error, Iter, Local};

const ARRAY: u64 = 1;
const STRING: u64 = 2;

/// The mode of the code of an event handler attribute.
const HANDLER: ScriptMode = ScriptMode { value: true, eval: false, scoped: true };

fn this_fn(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

/// `%eval%`: indirect eval.
pub(crate) fn eval_native(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.eval_code(args.first().copied().unwrap_or(Value::UNDEFINED), true)
}

impl Realm {
    /// Compile `src` as a script of this realm and add it to the program. Gives the code index of its top level. A syntax error is a
    /// thrown `SyntaxError`; a realm with no compiler stops with an abort.
    pub(crate) fn script_code(&mut self, src: &str, mode: ScriptMode) -> R<usize> {
        self.script_code_in(src, mode, None, false, false)
    }

    fn script_code_in(
        &mut self, src: &str, mode: ScriptMode,
        chain: Option<std::rc::Rc<crate::compile::Level>>, strict: bool, new_target: bool,
    ) -> R<usize> {
        let Some(l) = self.lazy.as_mut() else {
            self.api.abort = Some(Abort::Unsupported("a realm with no compiler"));
            return Err(STOP);
        };
        let ci = match l.script_in(&self.prog, src, mode, chain, strict, new_target) {
            Ok(ci) => ci,
            Err(m) => {
                let (kind, text) = match m.strip_prefix("SyntaxError: ") {
                    Some(t) => (ErrKind::Syntax, t),
                    None => (ErrKind::Plain, m.as_str()),
                };
                return Err(self.error(kind, text));
            }
        };
        self.fn_info.extend((self.fn_info.len()..self.prog.len()).map(|i| crate::interp::code_info(&self.prog[i])));
        Ok(ci)
    }

    /// Run the top level code `ci` of a script with the global object as `this`. `native`: a native on the Rust stack calls this, with
    /// no host call or nested call around it (`nest` is 0), so the run is a nested call that holds the collector. A run on a pooled
    /// machine would collect with the stack of the caller out of the roots.
    pub(crate) fn run_code(&mut self, ci: usize, native: bool) -> R<Value> {
        let (f, g) = (self.new_function(ci, Value::UNDEFINED), self.global.value());
        if native && self.stack_full() {
            return Err(self.nest_limit());
        }
        if native { self.call_function(f, g, &[]) } else { self.a_call(f, g, &[]) }
    }

    /// Indirect eval: a value that is not a string is the result; a string runs as a script in the global environment.
    pub(crate) fn eval_code(&mut self, v: Value, native: bool) -> R<Value> {
        let Some(s) = Realm::str_of(v) else { return Ok(v) };
        let src = self.text(s);
        let ci = self.script_code(&src, ScriptMode { value: true, eval: true, scoped: false })?;
        self.run_code(ci, native)
    }

    /// Run `source` as a classic script of the realm. Inside a host call (`importScripts` of a worker) the run is a nested call, as for
    /// `eval_global`.
    pub(crate) fn a_run_script<'s>(&mut self, source: &str) -> Result<Local<'s>, Error<'s>> {
        self.api.abort = None;
        let r = self.script_code(source, ScriptMode { value: true, ..ScriptMode::default() }).and_then(|ci| self.run_code(ci, false));
        self.a_ret(r)
    }

    pub(crate) fn a_compile_function<'s>(&mut self, name: &str, params: &str, body: &str) -> Result<Local<'s>, Error<'s>> {
        let src = format!("(function {name}({params}) {{\n{body}\n}})");
        let ci = match self.api.handlers.get(&src) {
            Some(&ci) => Ok(ci),
            None => self.script_code(&src, HANDLER).inspect(|&ci| {
                self.api.handlers.insert(src, ci);
            }),
        };
        let r = ci.and_then(|ci| self.run_code(ci, false));
        self.a_ret(r)
    }

    pub(crate) fn a_call_scoped<'s>(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>], scope: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        let (f, this, args) = (self.a_at(f), self.a_at(this), self.a_args(args));
        let mut objs = Vec::with_capacity(scope.len());
        for &l in scope {
            objs.push(self.a_obj(l)?);
        }
        let prev = std::mem::replace(&mut self.api.scope, objs);
        let r = self.a_call(f, this, &args);
        self.api.scope = prev;
        self.a_ret(r)
    }

    pub(crate) fn a_eval_global<'s>(&mut self, src: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let v = self.a_at(src);
        let r = self.eval_code(v, false);
        self.a_ret(r)
    }

    pub(crate) fn is_eval(&self, callee: Value) -> bool {
        self.shared_fn(self.global, "eval") == Some(callee)
    }

    pub(crate) fn a_eval_function<'s>(&mut self) -> Local<'s> {
        let k = self.intern("eval");
        self.lazy_prep(self.global, k);
        self.a_push(self.shared_fn(self.global, "eval").expect("the eval intrinsic"))
    }

    pub(crate) fn direct_eval(
        &mut self, v: Value, this: Value, ctx: Value,
        chain: Option<std::rc::Rc<crate::compile::Level>>, strict: bool, new_target: bool,
    ) -> R<Value> {
        let Some(s) = Realm::str_of(v) else { return Ok(v) };
        let src = self.text(s);
        let ci = self.script_code_in(&src, ScriptMode { value: true, eval: true, scoped: false }, chain, strict, new_target)?;
        let f = self.new_function(ci, ctx);
        self.call_function(f, this, &[])
    }

    // ---- the names of event handler code ----

    /// A bound function: `f` with `this` fixed (the native of `Engine::bind`).
    fn bound_to(&mut self, f: Value, this: Value) -> Value {
        let b = self.native_cell(self.bound_id as usize);
        b.set_slot(&self.shapes, 1, f);
        b.set_slot(&self.shapes, 2, this);
        b.set_slot(&self.shapes, 3, Value::UNDEFINED);
        b.value()
    }

    /// The first object of the running `call_scoped` that has the name `n`.
    fn scope_find(&mut self, n: Str) -> R<Option<Value>> {
        for i in 0..self.api.scope.len() {
            let o = self.api.scope[i].value();
            if self.has_prop(o, Key::Name(n))? {
                return Ok(Some(o));
            }
        }
        Ok(None)
    }

    /// `GetName` (`missing_ok` false) and `TypeofName` (true): a global lexical binding, then the scope, then the global object.
    pub(crate) fn get_name(&mut self, n: Str, missing_ok: bool) -> R<Value> {
        if let Some(v) = self.lex_get(n) {
            return v;
        }
        if let Some(o) = self.scope_find(n)? {
            let v = self.get_prop(o, Key::Name(n))?;
            return Ok(if Realm::obj(v).is_some_and(|f| f.is_callable()) { self.bound_to(v, o) } else { v });
        }
        let g = self.global.value();
        let v = self.get_prop(g, Key::Name(n))?;
        if v.is_undefined() && !missing_ok && !self.has_prop(g, Key::Name(n))? {
            let t = self.text(n);
            return Err(self.error(ErrKind::Reference, &format!("{t} is not defined")));
        }
        Ok(v)
    }

    /// `SetName`: the same search. A write that the property refuses is ignored (sloppy code).
    pub(crate) fn set_name(&mut self, n: Str, v: Value) -> R<()> {
        if let Some(r) = self.lex_put(n, v) {
            return r;
        }
        let o = match self.scope_find(n)? {
            Some(o) => o,
            None => self.global.value(),
        };
        match self.set_prop_g(o, Key::Name(n), v)? {
            crate::interp_rt::Put::Set(f) => self.call_function(f, o, &[v]).map(|_| ()),
            _ => Ok(()),
        }
    }

    // ---- iterators ----

    /// Make the iterator intrinsic before a host can read it through a shared borrow.
    pub fn init_host_intrinsics(&mut self) {
        self.iterator_prototype();
    }

    pub(crate) fn host_iterator_prototype(&self) -> Value {
        assert!(!self.api.iter_proto.is_undefined(), "initialize host intrinsics before reading them");
        self.api.iter_proto
    }

    /// `%IteratorPrototype%`: an object with `[Symbol.iterator]() { return this }`, made at the first ask.
    pub(crate) fn iterator_prototype(&mut self) -> Value {
        if self.api.iter_proto.is_undefined() {
            let o = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
            let f = self.new_native(this_fn, false, 0, "[Symbol.iterator]");
            let k = Key::Name(self.wk[ITERATOR]);
            self.define_attrs(o, k, HIDDEN, f.value()).expect("define");
            self.api.iter_proto = o.value();
        }
        self.api.iter_proto
    }

    pub(crate) fn a_iter_open<'s>(&mut self, v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>> {
        let x = self.a_at(v);
        if x.is_undefined() || x.is_null() {
            return Ok(None);
        }
        let array = Realm::obj(x).is_some_and(|o| self.shapes.get(o.shape(&self.shapes)).layout == Layout::Indexed);
        if array || Realm::str_of(x).is_some() {
            let a = self.a_push(x);
            return Ok(Some(Iter::from_parts(a, a, [if array { ARRAY } else { STRING }, 0, 0])));
        }
        let r = self.iter_record(x);
        match r {
            Ok(Some((it, next))) => {
                let (a, b) = (self.a_push(it), self.a_push(next));
                Ok(Some(Iter::from_parts(a, b, [0, 0, 0])))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(self.a_err(e)),
        }
    }

    /// `GetIterator(x)` for an object or a primitive that has `Symbol.iterator`: the iterator and its `next`. `None`: no `Symbol.iterator`.
    fn iter_record(&mut self, x: Value) -> R<Option<(Value, Value)>> {
        let m = self.get_prop(x, Key::Name(self.wk[ITERATOR]))?;
        if m.is_undefined() || m.is_null() {
            return Ok(None);
        }
        let it = self.a_call(m, x, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("the result of Symbol.iterator is not an object");
        }
        let k = self.intern("next");
        let next = self.get_prop(it, Key::Name(k))?;
        Ok(Some((it, next)))
    }

    pub(crate) fn a_iter_next<'s>(&mut self, it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>> {
        let (a, b, [kind, i, done]) = it.parts();
        if done != 0 {
            return Ok(None);
        }
        Engine::charge(self, 1)?;
        let r = match kind {
            ARRAY => Ok(self.array_step(a, i)),
            STRING => Ok(self.string_step(a, i)),
            _ => self.generic_step(a, b),
        };
        match r {
            Ok(Some((v, n))) => {
                it.set_state([kind, n, 0]);
                Ok(Some(self.a_push(v)))
            }
            Ok(None) => {
                it.set_state([kind, i, 1]);
                Ok(None)
            }
            Err(e) => {
                it.set_state([kind, i, 1]);
                Err(self.a_err(e))
            }
        }
    }

    /// The element `i` of the array `a` and the next position; the length is read again at each step.
    fn array_step(&mut self, a: Local<'_>, i: u64) -> Option<(Value, u64)> {
        let o = Realm::obj(self.a_at(a)).expect("an array");
        (i < o.len() as u64).then(|| (o.get_element(i as u32).unwrap_or(Value::UNDEFINED), i + 1))
    }

    /// The code point of the string `a` at the unit `i` as a string, and the next position.
    fn string_step(&mut self, a: Local<'_>, i: u64) -> Option<(Value, u64)> {
        let s = Realm::str_of(self.a_at(a)).expect("a string");
        let i = i as usize;
        let u = s.unit_at(i)?;
        let n = if (0xD800..0xDC00).contains(&u) && s.unit_at(i + 1).is_some_and(|l| (0xDC00..0xE000).contains(&l)) { 2 } else { 1 };
        let units: [u16; 2] = [u, s.unit_at(i + 1).unwrap_or(0)];
        let c = Str::new(&mut self.heap, Chars::Two(&units[..n])).expect("memory");
        Some((c.value(), (i + n) as u64))
    }

    /// One call of `next`: the value, or `None` when the result says `done`.
    fn generic_step(&mut self, a: Local<'_>, b: Local<'_>) -> R<Option<(Value, u64)>> {
        let (it, next) = (self.a_at(a), self.a_at(b));
        let r = self.a_call(next, it, &[])?;
        if Realm::obj(r).is_none() {
            return self.type_error("the result of next is not an object");
        }
        let k = self.intern("done");
        let done = self.get_prop(r, Key::Name(k))?;
        if Realm::truthy(done) {
            return Ok(None);
        }
        let k = self.intern("value");
        let v = self.get_prop(r, Key::Name(k))?;
        Ok(Some((v, 0)))
    }

    pub(crate) fn a_iter_fail<'s>(&mut self, it: &mut Iter<'s>, err: Error<'s>) -> Error<'s> {
        let (a, _, [kind, i, done]) = it.parts();
        it.set_state([kind, i, 1]);
        if kind != 0 || done != 0 || matches!(err, Error::Abort(_)) {
            return err;
        }
        // IteratorClose with a throw completion: `return` runs, and its result and its error are dropped.
        let o = self.a_at(a);
        let k = Key::Name(self.intern("return"));
        if let Ok(f) = self.get_prop(o, k)
            && Realm::obj(f).is_some_and(|f| f.is_callable())
        {
            let _ = self.a_call(f, o, &[]);
            if self.api.abort.is_some() {
                return Error::Abort(self.api.abort.take().expect("an abort"));
            }
        }
        err
    }

    pub(crate) fn a_symbol_description<'s>(&mut self, v: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        let Some(s) = Realm::sym_of(self.a_at(v)) else { return Err(self.a_throw(ErrKind::Type, "not a symbol")) };
        let d = self.symbol_description_value(s);
        Ok((!d.is_undefined()).then(|| self.a_push(d)))
    }
}

#[cfg(test)]
#[path = "api_script_tests.rs"]
mod tests;

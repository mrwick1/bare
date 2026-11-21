//! Generators: the suspended frame (`docs/design-js2-async.md` 5.1 and 6, plan step G1). Safe code over [`Realm`]; the loop of `interp.rs`
//! calls it from its cold arms.
//!
//! **The generator object** (the *activation*) is an object with the flag [`F_GEN`] and four hidden slots (names that start with `\0`, so
//! no script sees them): the state, the saved frame, the value that the last `next` sent, and the mode of the last resume.
//!
//! | Slot | Holds |
//! | --- | --- |
//! | 0 | the state: [`G_START`], [`G_SUSPENDED`], [`G_RUNNING`], [`G_DONE`] |
//! | 1 | the saved frame, a record (`Ctx`) that the activation owns; `undefined` when done |
//! | 2 | the sent value (`next(v)`, `throw(e)`, `return(v)`) |
//! | 3 | the mode: [`MODE_NEXT`], [`MODE_THROW`], [`MODE_RETURN`] |
//!
//! **The saved frame** is a `Slots` record of `FR_HDR` words and then the registers of the save list: the function, `this`, the current
//! context, the number of contexts that the function pushed, the arguments object, the `pc` where a resume goes on, the offset of the
//! save list in `Code::tables`, the code index, and the registers. It holds only the registers of the list, so it holds no stale pointer, and the marker
//! scans it like any record: no side table, no root map, no program. A generator that nobody reaches is freed with its frame.
//!
//! **A running generator** is an ordinary frame on the stack of whatever machine resumed it. Its activation is in the word of the frame
//! record that holds `new.target` for any other function (a generator is never constructed, and the verifier rejects `LoadNewTarget` in its
//! code), so the marker finds it, and `Yield`, `RetGen` and `unwind` find it. The frame has the flag [`GEN`] in its destination word, and
//! [`ITER`] when the caller wants the result object `{ value, done }` (a `next()` call; not a resume from the host).
//!
//! **Resume.** `next`, `return` and `throw` are natives. [`Realm::gen_request`] checks the state, writes the sent value and the mode, and
//! says what to do: an answer ([`Step::Result`]) or a frame to enter ([`Step::Enter`]). The native answers an `Enter` by asking for
//! [`Outcome::Enter`] through the tail channel (`Realm::tail` holds the generator and the hole as `this`, which no script can pass). The loop
//! then pushes the frame with [`Realm::gen_enter`]: the same `ensure!` and `reload!` as a call, so the register pointer of the loop is
//! made again. Nothing in the saved state names a stack, so a generator that suspended on one machine resumes on any other.
use crate::builtins::{NativeFn, arg};
use crate::bytecode::{Code, NO_REG};
use crate::codes::Codes;
use crate::interp::{GEN, MAX_STACK, REC};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::shapes::A_C;
use crate::object::{F_GEN, Obj};
use crate::shapes::ShapeId;
use crate::value::Value;

pub const G_START: i32 = 0;
pub const G_SUSPENDED: i32 = 1;
pub const G_RUNNING: i32 = 2;
pub const G_DONE: i32 = 3;

pub const MODE_NEXT: i32 = 0;
pub const MODE_THROW: i32 = 1;
pub const MODE_RETURN: i32 = 2;

const FR_CALLEE: usize = 0;
const FR_THIS: usize = 1;
const FR_CTX: usize = 2;
const FR_DEPTH: usize = 3;
const FR_ARGS: usize = 4;
const FR_PC: usize = 5;
const FR_LIST: usize = 6;
const FR_CI: usize = 7;
/// Words of a saved frame before its registers.
pub const FR_HDR: usize = 8;

/// What a request to a generator needs the caller to do.
pub enum Step {
    /// The request is answered: `(value, done)`.
    Result(Value, bool),
    /// The state is now running and the sent value and the mode are in the activation: push the frame ([`Realm::gen_enter`]).
    Enter,
}

/// The pieces of the realm that generators need (`Realm::genr`).
pub(crate) struct GenRealm {
    /// `%GeneratorPrototype%`: `next`, `return` and `throw`.
    pub proto: Obj,
    /// The shape of `{ value, done }`.
    pub s_iter: ShapeId,
    /// The names of the hidden slots: the first four are those of a generator, the fifth holds the result promise of an async function.
    pub keys: [crate::string::Str; 5],
    /// `%GeneratorFunction.prototype%`, made when the first generator function is (a rooted constant: `Realm::consts`).
    pub func_proto: Option<Obj>,
    /// `%AsyncFunction.prototype%`, made when the first async function is (a rooted constant).
    pub async_proto: Option<Obj>,
    /// The shape of the activation of an async function (no prototype, five hidden slots).
    pub async_shape: Option<ShapeId>,
}

impl Realm {
    fn activation(v: Value) -> Option<Obj> {
        Realm::obj(v).filter(|o| o.has_flag(F_GEN))
    }

    /// Is `v` a generator object? (`for-of` steps one itself: the intrinsic path of the iterator protocol, until G3.)
    pub(crate) fn is_generator(v: Value) -> bool {
        Realm::activation(v).is_some()
    }

    fn gen_state(a: Obj) -> i32 {
        a.inline_slot(0).as_int().expect("the state of a generator")
    }

    /// Has the generator finished (a return, a throw out of its body)?
    pub fn gen_done(&self, g: Value) -> bool {
        Realm::activation(g).is_some_and(|a| Realm::gen_state(a) == G_DONE)
    }

    /// `{ value, done }`.
    pub fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let o = Obj::new(&mut self.heap, &self.shapes, self.genr.s_iter).expect("memory");
        o.set_inline_slot(0, value);
        o.set_inline_slot(1, Value::bool(done));
        o.value()
    }

    /// `GenInit`: make the activation and its record for the frame `(stack, fp)`, save the registers of the list at `tbl`, and give the
    /// activation. The first resume goes on at `resume`. No collection runs here (the loop collected before).
    pub(crate) fn gen_init(&mut self, stack: &[Value], fp: usize, ci: usize, code: &Code, tbl: u32, resume: usize) -> R<Value> {
        let callee = stack[fp - REC];
        let proto = match Realm::obj(callee).map(|f| self.lookup(f, self.k_prototype)).transpose()?.flatten().and_then(Realm::obj) {
            Some(p) => p,
            None => self.genr.proto,
        };
        let mut shape = self.shapes.root(crate::cell::Layout::Object, Some(proto.cell()), 4);
        for k in self.genr.keys[..4].iter().copied() {
            shape = self.shapes.add(shape, k, 0).expect("shape");
        }
        let act = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        act.set_flag(F_GEN);
        let fr = crate::object::Ctx::new(&mut self.heap, FR_HDR + code.gen_regs as usize, Value::UNDEFINED).expect("memory");
        act.set_inline_slot(0, Value::int(G_START));
        act.set_inline_slot(1, fr.value());
        act.set_inline_slot(3, Value::int(MODE_NEXT));
        save(stack, fp, ci, code, tbl, resume, fr);
        Ok(act.value())
    }

    /// `Yield`: save the frame `(stack, fp)` into the record of its activation and say that the generator waits. Gives what the caller
    /// receives: the value, or `{ value, done: false }` when the frame has [`ITER`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gen_yield(&mut self, stack: &[Value], fp: usize, ci: usize, code: &Code, tbl: u32, resume: usize, v: Value, iter: bool) -> Value {
        let act = Realm::activation(stack[fp - REC + 8]).expect("the activation of a generator frame");
        let fr = Realm::ctx(act.inline_slot(1)).expect("the record of a generator");
        save(stack, fp, ci, code, tbl, resume, fr);
        act.set_inline_slot(0, Value::int(G_SUSPENDED));
        if iter { self.iter_result(v, false) } else { v }
    }

    /// `RetGen`: the generator is done. Gives the result of the call as [`Realm::gen_yield`] does, with `done: true`.
    pub(crate) fn gen_return(&mut self, stack: &[Value], fp: usize, v: Value, iter: bool) -> Value {
        let act = Realm::activation(stack[fp - REC + 8]).expect("the activation of a generator frame");
        finish(act);
        if iter { self.iter_result(v, true) } else { v }
    }

    /// `AsyncInit`: make the activation and the result promise of the async function whose frame is `(stack, fp)`, and keep the activation in
    /// the frame record (the word of `new.target`, which an async function never reads). The activation has no record yet: the first
    /// `Await` makes it. No collection runs here (the loop collected before).
    pub(crate) fn async_init(&mut self, stack: &mut [Value], fp: usize) {
        let shape = match self.genr.async_shape {
            Some(s) => s,
            None => {
                let mut s = self.shapes.root(crate::cell::Layout::Object, None, 5);
                for k in self.genr.keys {
                    s = self.shapes.add(s, k, 0).expect("shape");
                }
                self.genr.async_shape = Some(s);
                s
            }
        };
        let act = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        act.set_flag(F_GEN);
        let p = self.new_promise();
        act.set_inline_slot(0, Value::int(G_RUNNING));
        act.set_inline_slot(3, Value::int(MODE_NEXT));
        act.set_inline_slot(4, p.value());
        stack[fp - REC + 8] = act.value();
        let w = stack[fp - REC + 7].as_int().expect("the destination word");
        stack[fp - REC + 7] = Value::int(w | GEN);
    }

    /// `Await`: wait for `v`. The activation of the frame `(stack, fp)` becomes the handler of a reaction on the promise of `v`, and the frame
    /// is saved. Gives the result promise of the function, which the caller receives. Nothing changed when it throws.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn async_await(&mut self, stack: &[Value], fp: usize, ci: usize, code: &Code, tbl: u32, resume: usize, v: Value) -> R<Value> {
        let act = Realm::activation(stack[fp - REC + 8]).expect("the activation of an async frame");
        let p = self.promise_resolve_default(v)?;
        self.perform_then(p, act.value(), act.value(), Value::UNDEFINED)?;
        let fr = match Realm::ctx(act.inline_slot(1)) {
            Some(fr) => fr,
            None => {
                let fr = crate::object::Ctx::new(&mut self.heap, FR_HDR + code.gen_regs as usize, Value::UNDEFINED).expect("memory");
                act.set_inline_slot(1, fr.value());
                fr
            }
        };
        save(stack, fp, ci, code, tbl, resume, fr);
        act.set_inline_slot(0, Value::int(G_SUSPENDED));
        Ok(act.inline_slot(4))
    }

    /// An async function that cannot go on (a throw out of its frame): reject its promise with `e`.
    pub(crate) fn async_abort(&mut self, act: Value, e: Value) {
        let Some(a) = Realm::activation(act) else { return };
        let p = a.inline_slot(4);
        finish(a);
        if let Some(po) = Realm::promise_obj(p) {
            self.reject_promise(po, e);
        }
    }

    /// `RetAsync`: the async function is done. Resolve (or, with `reject`, reject) its promise with `v`, and give the promise.
    pub(crate) fn async_return(&mut self, stack: &[Value], fp: usize, v: Value, reject: bool) -> R<Value> {
        let act = Realm::activation(stack[fp - REC + 8]).expect("the activation of an async frame");
        let p = act.inline_slot(4);
        finish(act);
        let po = Realm::promise_obj(p).expect("the promise of an async function");
        if reject {
            self.reject_promise(po, v);
        } else {
            self.resolve_promise(po, v)?;
        }
        Ok(p)
    }

    /// A request to the generator `g` (`next`, `throw`, `return`; `v` is the argument). See [`Step`].
    pub(crate) fn gen_request(&mut self, g: Value, mode: i32, v: Value) -> R<Step> {
        let Some(act) = Realm::activation(g) else { return self.type_error("not a generator") };
        match Realm::gen_state(act) {
            G_RUNNING => return self.type_error("the generator is already running"),
            G_DONE => {
                return match mode {
                    MODE_NEXT => Ok(Step::Result(Value::UNDEFINED, true)),
                    MODE_RETURN => Ok(Step::Result(v, true)),
                    _ => Err(v),
                };
            }
            G_START if mode != MODE_NEXT => {
                // A generator that never ran ends at once: the body does not start.
                finish(act);
                return if mode == MODE_RETURN { Ok(Step::Result(v, true)) } else { Err(v) };
            }
            _ => {}
        }
        act.set_inline_slot(0, Value::int(G_RUNNING));
        act.set_inline_slot(2, v);
        act.set_inline_slot(3, Value::int(mode));
        Ok(Step::Enter)
    }

    /// Push the frame of the suspended generator `g` at stack index `nf` (the record starts at `nf - REC`). `caller` is the code index
    /// of the frame below, or `ENTRY` (-1); `dstw`, `callpc` and `next` are what a call writes in the record. Gives the code index and the
    /// `pc` to go on at. The caller makes its locals again (`reload!`): the stack may have grown.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn gen_enter(&mut self, stack: &mut Vec<Value>, prog: &Codes, nf: usize, caller: i32, dstw: i32, callpc: usize, next: usize, g: Value) -> R<(usize, usize)> {
        let act = Realm::activation(g).expect("a generator");
        let fr = Realm::ctx(act.inline_slot(1)).expect("the record of a generator");
        let int = |v: Value| v.as_int().expect("an integer word of a saved frame");
        let ci = int(fr.with(|w| w[FR_CI])) as usize;
        let code = &prog[ci];
        let need = nf + code.nregs as usize + 8;
        if need > stack.len() {
            if need > MAX_STACK {
                act.set_inline_slot(0, Value::int(G_SUSPENDED));
                return Err(self.error(ErrKind::Range, "maximum call stack size exceeded"));
            }
            stack.resize(need.max(256).max(2 * stack.len()).min(MAX_STACK + 8).max(need), Value::UNDEFINED);
        }
        let (sent, mode) = (act.inline_slot(2), act.inline_slot(3));
        let pc = fr.with(|w| {
            let list = code.save_list(int(w[FR_LIST]) as u32).expect("the save list of a suspension");
            let w = &w[..FR_HDR + list.regs.len()];
            let rec = [w[FR_CALLEE], w[FR_THIS], w[FR_CTX], Value::int(callpc as i32), Value::int(next as i32), Value::int(caller), w[FR_DEPTH], Value::int(dstw | GEN), act.value(), w[FR_ARGS]];
            stack[nf - REC..nf].copy_from_slice(&rec);
            let frame = &mut stack[nf..nf + code.nregs as usize];
            for (&r, &v) in list.regs.iter().zip(&w[FR_HDR..]) {
                frame[r as usize] = v;
            }
            if list.sent != NO_REG {
                frame[list.sent as usize] = sent;
            }
            if list.mode != NO_REG {
                frame[list.mode as usize] = mode;
            }
            int(w[FR_PC]) as usize
        });
        Ok((ci, pc))
    }

    /// `Closure` made the generator function `f`: it inherits `%GeneratorFunction.prototype%`, which is made now if it is not yet. That object
    /// holds `prototype` (`%GeneratorPrototype%`) and the tag; `%GeneratorPrototype%` holds `constructor` and its tag. The `GeneratorFunction`
    /// constructor is not built (js2 has no `Function` constructor).
    #[cold]
    pub(crate) fn make_generator_function(&mut self, f: Value) {
        if self.function(f).is_some_and(|(ci, _)| self.prog[ci].gen_kind == crate::bytecode::GK_ASYNC) {
            return self.make_async_function(f);
        }
        let fp = match self.genr.func_proto {
            Some(p) => p,
            None => {
                let shape = self.shapes.root(crate::cell::Layout::Object, Some(self.function_proto.cell()), 4);
                let p = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
                let gp = self.genr.proto;
                self.consts.push(p.value());
                self.genr.func_proto = Some(p);
                let tag = Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]);
                let (k_proto, k_ctor) = (self.k_prototype, self.k_constructor);
                let (t1, t2) = (self.string("GeneratorFunction"), self.string("Generator"));
                self.define_attrs(p, Key::Name(k_proto), A_C, gp.value()).expect("define");
                self.define_attrs(p, tag, A_C, t1).expect("define");
                self.define_attrs(gp, Key::Name(k_ctor), A_C, p.value()).expect("define");
                self.define_attrs(gp, tag, A_C, t2).expect("define");
                // `%IteratorPrototype%` (made for the host API too) is the prototype of `%GeneratorPrototype%`.
                let ip = Realm::obj(self.iterator_prototype()).expect("an object");
                gp.set_proto(&mut self.shapes, Some(ip.cell()));
                p
            }
        };
        let o = Realm::obj(f).expect("a function");
        o.set_proto(&mut self.shapes, Some(fp.cell()));
    }

    /// `Closure` made the async function `f`: it inherits `%AsyncFunction.prototype%`, which holds the tag `AsyncFunction`. The `AsyncFunction`
    /// constructor is not built (js2 has no `Function` constructor).
    #[cold]
    fn make_async_function(&mut self, f: Value) {
        let ap = match self.genr.async_proto {
            Some(p) => p,
            None => {
                let shape = self.shapes.root(crate::cell::Layout::Object, Some(self.function_proto.cell()), 4);
                let p = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
                self.consts.push(p.value());
                self.genr.async_proto = Some(p);
                let tag = Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]);
                let t = self.string("AsyncFunction");
                self.define_attrs(p, tag, A_C, t).expect("define");
                p
            }
        };
        let o = Realm::obj(f).expect("a function");
        o.set_proto(&mut self.shapes, Some(ap.cell()));
    }

    /// The prototype of generator objects and its three methods.
    pub(crate) fn install_generator(&mut self) {
        let p = self.genr.proto;
        for (n, f) in [("next", gen_next as NativeFn), ("return", gen_return), ("throw", gen_throw)] {
            self.method(p, n, 1, f);
        }
    }
}

/// Copy the record words and the registers of the list into `fr` (see the module comment).
fn save(stack: &[Value], fp: usize, ci: usize, code: &Code, tbl: u32, resume: usize, fr: crate::object::Ctx) {
    let rec = &stack[fp - REC..fp];
    let list = code.save_list(tbl).expect("the save list of a suspension");
    let regs = &stack[fp..fp + code.nregs as usize];
    fr.with(|w| {
        let w = &mut w[..FR_HDR + list.regs.len()];
        w[FR_CALLEE] = rec[0];
        w[FR_THIS] = rec[1];
        w[FR_CTX] = rec[2];
        w[FR_DEPTH] = rec[6];
        w[FR_ARGS] = rec[9];
        w[FR_PC] = Value::int(resume as i32);
        w[FR_LIST] = Value::int(tbl as i32);
        w[FR_CI] = Value::int(ci as i32);
        for (d, &r) in w[FR_HDR..].iter_mut().zip(list.regs) {
            *d = regs[r as usize];
        }
    });
}

/// The generator is done: its record is dropped.
pub(crate) fn finish(act: Obj) {
    act.set_inline_slot(0, Value::int(G_DONE));
    act.set_inline_slot(1, Value::UNDEFINED);
    act.set_inline_slot(2, Value::UNDEFINED);
}

/// A frame with the flag [`GEN`] is dropped by an exception: its generator is done. `act` is the word of the record that holds it.
pub(crate) fn abandon(act: Value) {
    if let Some(a) = Realm::activation(act) {
        finish(a);
    }
}

fn request(rt: &mut Realm, this: Value, mode: i32, v: Value) -> R<Value> {
    match rt.gen_request(this, mode, v)? {
        Step::Result(v, d) => Ok(rt.iter_result(v, d)),
        Step::Enter => {
            // The loop sees `Outcome::Enter` with this hole (`Realm::call_native`). The hole is no `this` that a script can pass.
            rt.tail = Some((this, Value::HOLE));
            Ok(Value::UNDEFINED)
        }
    }
}

fn gen_next(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    request(rt, this, MODE_NEXT, arg(args, 0))
}

fn gen_return(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    request(rt, this, MODE_RETURN, arg(args, 0))
}

fn gen_throw(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    request(rt, this, MODE_THROW, arg(args, 0))
}

#[cfg(test)]
#[path = "generator_tests.rs"]
mod tests;

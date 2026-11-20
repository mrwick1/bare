//! A native calls a script function and waits for the result (plan step S2.7 follow-up). `ToPrimitive`, `ToNumber`, `ToString`,
//! `ToPropertyKey`, a getter or a setter that a built-in reads or writes, and any other Rust code that has `&mut Realm` can run a
//! `valueOf`, a `toString` or an accessor that a script wrote: [`Realm::call_function`]. Safe code: it adds no `unsafe`.
//!
//! **The problem.** The loop of `Vm::run` owns the JS stack, the feedback vectors and the cached locals of the running function, and it
//! lends only `&mut Realm` to a native. A call from a native cannot use that stack (`&mut` twice), and a stack that is pushed to
//! behind the loop would also leave the `args` slice of the native (a window into the stack) dangling when the stack grows.
//!
//! **The design.** A nested call runs on a machine of its own: [`Machine`], a stack and a feedback vector. A machine is taken from
//! `Realm::pool` (or made when the pool is empty) and given back after the call, so a native that calls many times reuses one, and
//! a nested call inside a nested call takes the next. The program, the budget (`Realm::fuel`) and the table of default constructors
//! are fields of the realm too, so the new run needs nothing else from the loop that called the native. The loop itself changed in
//! three places only: its entry (`Vm::run` takes the pieces as arguments), the back edge (the budget is in the realm) and `unwind`
//! (a stop is not caught). No op of the hot path got a new instruction.
//!
//! **The collector is held during a nested call.** A native keeps cells in Rust locals (a string that it converted, the receiver)
//! and calls on, and a collection in the nested run would free them: a root map names registers, not Rust locals. So
//! `Heap::hold_task` makes `Realm::due` answer false until the outermost nested call returns (the next `gate!` then collects). The
//! heap grows meanwhile: a `valueOf` that allocates more than the slack (8 MiB) without returning holds that memory until it
//! returns, and the budget is the only bound. Known gap, and the design to lift it is: a stack of rooted temporaries in the realm
//! for the natives, and the frames of the outer machines in `walk_frames` (a frame with the pc of its call, as for any caller).
//!
//! **Errors.** A thrown value comes back as `Err(value)`. When the budget runs out in the nested run, `Err(STOP)` comes back: the
//! native passes it on with `?`, the loop throws it at the op that called the native, and `unwind` skips every handler for it. The
//! run that the host started then returns `Exit::Fuel`.
//!
//! **State that a nested call must not change.** A native reads `callee` and `nt` (its own function and `new.target`) and may have
//! staged a tail call; a nested native overwrites them, so `call_function` saves them and puts them back.
use crate::builtins::Outcome;
use crate::interp::{Exit, STOP, Vm};
use crate::interp_rt::{ErrKind, Feedback, R, Realm};
use crate::value::Value;

/// A JS stack and a feedback vector for one nested call.
pub(crate) struct Machine {
    pub(crate) stack: Vec<Value>,
    pub(crate) fbs: Vec<Feedback>,
}

/// The most nested calls. Each one is a frame of `Vm::run` and of the native above it on the Rust stack.
const MAX_NEST: u32 = 100;

impl Realm {
    /// Call `f` (a script function or a built-in) with `this` and `args`. A thrown value is `Err(value)`.
    ///
    /// The result is a Rust local like any other: no collection runs until the outermost nested call is over, so it stays valid
    /// while the native goes on. Call it only from code that the loop called (a native, or an operator that the loop runs).
    pub fn call_function(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        if self.nest >= MAX_NEST {
            return Err(self.error(ErrKind::Range, "maximum call stack size exceeded"));
        }
        let saved = (self.callee, self.nt, self.tail.take(), std::mem::take(&mut self.tail_args));
        self.nest += 1;
        let held = self.heap.hold_task();
        let r = self.call_held(f, this, args);
        self.heap.release_task(held);
        self.nest -= 1;
        (self.callee, self.nt, self.tail, self.tail_args) = saved;
        r
    }

    pub(crate) fn call_held(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        if self.native_id(f).is_none() {
            return self.call_script(f, this, args);
        }
        match self.call_native(f, this, args)? {
            Outcome::Val(v) => Ok(v),
            // A native that ends in a call (`call`, `apply`, a bound function): the target is a script function. With the hole as `this`:
            // `next`, `return` or `throw` of a generator that a native called.
            Outcome::Enter { f, this } if this.is_hole() => self.resume_script(f),
            Outcome::Enter { f, this } => {
                let a = std::mem::take(&mut self.tail_args);
                self.call_script(f, this, &a)
            }
        }
    }

    fn resume_script(&mut self, g: Value) -> R<Value> {
        let mut m = self.pool.pop().unwrap_or_else(|| Machine { stack: Vec::new(), fbs: Vec::new() });
        let r = Vm::resume_start(self, &mut m.stack, &mut m.fbs, g);
        self.pool.push(m);
        match r {
            Ok(v) => {
                let done = self.gen_done(g);
                Ok(self.iter_result(v, done))
            }
            Err(Exit::Throw(e)) => Err(e),
            Err(Exit::Fuel) => Err(STOP),
        }
    }

    fn call_script(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        if self.function(f).is_none() {
            return if self.function_new(f).is_some() { self.type_error("class constructor cannot be invoked without 'new'") } else { self.type_error("not a function") };
        }
        let mut m = self.pool.pop().unwrap_or_else(|| Machine { stack: Vec::new(), fbs: Vec::new() });
        let r = Vm::start(self, &mut m.stack, &mut m.fbs, f, this, args);
        self.pool.push(m);
        match r {
            Ok(v) => Ok(v),
            Err(Exit::Throw(e)) => Err(e),
            Err(Exit::Fuel) => Err(STOP),
        }
    }
}

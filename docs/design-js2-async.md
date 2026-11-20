# Design: Promise, microtasks, generators and async functions in `bare-js2`

Status: design for owner review. No engine code changed. Date: 2026-11-20. Base: `origin/main` `b0a0df89`. It also reads PR #785 (`crates/js2/src/reenter.rs`, head `7a3e3df9`, not merged), because that PR changes how a native calls script.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "frame" is the record of `REC` words and the registers of one running function on the JS stack (`interp.rs`).
- "suspend" is to leave a frame and keep its state. "resume" is to enter it again.
- "activation" is the heap object of one suspended generator or async function.
- "job" is one entry of the microtask queue.
- "tick" is one job that runs between two steps of a script.
- "instructions" are user-mode instructions (`instructions:u`) of `perf stat`.

## 1. Question and answer

`bare-js2` has no `Promise`, no job queue, no generators and no async functions. The compiler records `await` and `yield` as "not supported yet" (`compile_expr.rs:459`). The adapter answers `Abort::Unsupported` for every promise and microtask method (`design-js2-host-api.md` 3.8). The browser slice (plan S2.8 and S5) needs promises and microtasks first: `fetch`, `queueMicrotask`, `Host::promise_rejections` and every `.then` chain depend on them. The census of the daily journeys (`docs/census/2026-11-18.tsv`, 50 workloads) shows `await` in 21 workloads (1,032 uses), async functions and arrows in 18 and 15, and `yield` in 5 (532 uses).

This page answers three questions:

1. How does a suspended frame live in the register VM of js2, and what does a `yield` or an `await` cost?
2. What is the layout of a promise, a reaction and the job queue, and who runs the jobs?
3. In which order do we build it?

Recommendation (details in sections 7 to 10):

- Suspended frames: option A. On suspend, copy the live registers of the frame into a heap record that the activation owns. On resume, push a normal frame on the stack and copy them back. A new frame kind in `Vm::run` is not needed. The hot loop gets no new instruction.
- Promise: objects with two internal slots. A reaction is a small heap record. The queue is an intrusive list of those records in the realm. An activation is its own reaction, so an `await` allocates nothing. An `await` of a native promise or of a plain value uses one tick and makes no extra promise.
- The realm owns the queue. The host runs it: `checkpoint` after each task and `drain_microtasks` at the top, as in the old engine. The engine never runs jobs by itself.
- Order: promises first, resumable frames with generators second, async functions third. The first two can be built in parallel.
- Expected result (hypothesis, section 8): 400 to 700 instructions for a `yield` and its resume, 450 to 550 for an `await`, against 2,400 to 3,950 and 2,900 to 3,380 in the old engine. About 144 bytes for a suspended generator, against about 530.

## 2. Facts about js2 (checked in the tree)

1. One `Vec<Value>` holds all frames of a run. A frame is `REC` = 10 words (callee, `this`, context, call pc, return pc, caller code index, pushed-context count, destination register and flags, `new.target`, arguments object) and then `nregs` registers (`interp.rs` module comment). The frame record is read by index: `record(stack, fp, k)`, `int_at(stack, fp, k)`. The new frame goes to `fp + nregs + REC`. A return computes the caller frame as `fp - REC - nregs(caller)`. Counted: 34 places in `interp.rs` read or compute frames this way.
2. After #773 (merged), `Vm::run` caches `regs`, a pointer to register 0 of the frame. `reload!` and `ensure!` set it again after any change of `fp` or any growth of the stack. The registers are read with `r!`, which has no bounds check. The verifier proves that a register is written before it is read, so a call does not fill the registers (`verify.rs`, 6.10 item 4).
3. A collection inside a task walks the frames in place (`walk_frames`). For each frame it marks the record words and the registers that the root map of the safepoint names. `Code::roots` is keyed by the `pc` of the safepoint. A register that is not in the map may hold the pointer of a freed cell (`roots.rs` module comment).
4. Cells have the layouts `Object`, `Indexed`, `Slots`, `Elements`, `WeakTable` and the string layouts (`cell.rs`). `Slots` and `Elements` are records of values: the marker marks every word of such a record (`gc.rs`, `record`). Cells do not move. A native function cell has internal slots 1 to 3 that the function may use (`builtins.rs`).
5. A native is `fn(&mut Realm, this, &[Value]) -> R<Value>`. It can end in a tail call: `Outcome::Enter { f, this }` with the arguments in `Realm::tail_args`. The loop turns it into a frame in the same `run` (`native_call!`). A native cannot reach the stack.
6. After #785, `Realm::call_function` calls script from any native. It runs on a `Machine` from `Realm::pool` (its own stack and feedback) and holds the collector until the outermost nested call returns (`Heap::hold_task`). A budget stop is `Err(STOP)`.
7. `Op::Lazy` (#777, merged) compiles a function at its first call into the same code index, then runs it from pc 0 with the same frame. Code indexes never change after that. Nothing flushes bytecode yet.
8. `try`/`finally` is compiled with the `finally` block copied at each exit (`compile_stmt.rs`, `fin_pieces`). A handler is a pc range of the code with a target, a register for the thrown value and a context depth (`Handler`). No dynamic state names the active `try`.
9. The parser parses `yield`, `await`, `async`, generator and async methods, and `for await` into the AST (`ast.rs`). The compiler stops at them (`compile_expr.rs:459,460,846,849`; `compile_stmt` for `for await`).
10. js2 has no `Symbol`, so no `Symbol.iterator`. `IterOpen` accepts an array or a string only; anything else throws (`interp.rs`, `Op::IterOpen`). Generators need a way to iterate a generator object. Section 9, step G, handles this.
11. The cost of ops in js2, measured on archMachine (`op_cost`, release, `perf stat -e instructions:u`, 100,000 turns of 10 copies of the op less the empty loop, 3 runs, identical):

| Op                                              | Instructions |
| ----------------------------------------------- | -----------: |
| `Mov` (register to register)                    |           23 |
| `GetCtx` or `SetCtx` (a context slot)           |           58 |
| `GetField` (cache hit)                          |           64 |
| `CallU` (a script call and return, no argument) |          316 |
| `NewObj`                                        |          115 |
| `NewObjLit` (a literal with fields)             |          179 |
| `Closure`                                       |          210 |

## 3. The old engine (`crates/js`)

### 3.1 Promise and the job queue (`eval_promise.rs`)

- A promise is an ordinary heap object. Its state is in a side table: `promise.map: FxMap<ObjId, PromiseData>`. `PromiseData` holds a sequence number, the state (`Pending`, `Fulfilled(v)`, `Rejected(v)`), `reactions: Vec<Reaction>` and the `handled` flag. A promise therefore costs a 104-byte object, a map entry and, while pending, a `Vec`.
- A reaction is `{ on_ok, on_err, derived, act }`: two handler values, the derived promise and an optional `ActRef` (an `await`).
- The resolving functions are native function objects with a `PFn::Settle { promise, pair, reject }` payload. `pair` is a counter that is never reused, so the "already resolved" flag cannot attach to a recycled slot.
- The queue is a `VecDeque<Job>`. A job is `React`, `Thenable`, `Callback` (`queueMicrotask`) or `Resume` (an `await`).
- Nothing runs by itself. The host calls `host_checkpoint` after a task (it does nothing unless the call depth is 1: the HTML rule "do nothing when the script stack is not empty") or `drain_microtasks` (`eval_promise.rs:227,245`). The drain loop calls `Host::between_jobs` before each job and once when the queue is empty, then runs the job. A failed job (a limit, an `Unsupported`) stores a sticky error, so later drains fail until the host calls `clear_failed_job`.
- Unhandled rejections (HTML "HostPromiseRejectionTracker"): `settle` puts a promise that is rejected with no handler on `rejections`. At the end of a checkpoint `notify_rejections` gives the host the still-unhandled promises and the reported ones that got a handler since. The lists are bounded (`MAX_PENDING_REJECTIONS` = 1,024 and 256), and the overflow is counted.
- Limits: `max_queue` 2^18, `max_promises` 2^20, `max_reactions` 2^20, `max_jobs_per_drain` 2^20. Each one is checked before the thing grows and ends the call with a distinct limit error.

### 3.2 Generators and async functions (`eval_async.rs`, `eval_generator.rs`)

- The bytecode lane refuses them: `bc_gen_expr.rs:199` returns `Err("await or yield")` and `bc_gen_stmt.rs:203` refuses `for await`. A function that holds one runs on the tree walker. A preflight marks each AST node that contains an `await` or a `yield`.
- Marked nodes run on an explicit machine. The machine keeps no Rust call stack across a suspension. Its state is a `Run`: a `Vec<Fr>` of continuation frames (ids and counters; 45 variants such as `Op`, `List`, `ForT`, `TryB`, `Fin`) and a `Vec<Slot>` of values. Nodes that hold no `await` run on the ordinary evaluator.
- A suspended generator is `Gen::Yield(Box<Run>)` in `aw.gens: FxMap<ObjId, Gen>`. A suspended async function is an `ActSlot { era, run }` in `aw.acts`, and a promise reaction or a queued `Resume` job holds its `ActRef`.
- GC: `collect` traces the run when it reaches the generator object (`gen_trace`) and drops the entries of dead objects (`gen_prune`). An activation that nobody reaches is dropped.
- `await v`: `PromiseResolve(%Promise%, v)`. For a value that is not an object, no promise is made and a `Resume` job is queued at once. Otherwise the activation is stored as a reaction on the promise. Each resume re-enters the machine and walks the continuation frames.
- Caps: 65,536 suspended activations, 2^20 frames and 2^20 slots over all runs.
- Unsupported: `await` in a parameter, a catch parameter, a for-in or for-of head pattern, `delete`, `++`/`--`, a tagged template, a class heritage or computed key, `super(...)` with an `await`. Top-level await.

### 3.3 How the browser drives microtasks

- The engine owns the promise state and the queue. The host holds a promise as a `Local`, a `Persistent` or a `Traced`, and settles it with `resolve_promise` or `reject_promise` (`bare-js-api`, `design-host-microtasks.md`).
- Each task of the browser ends with a checkpoint: 23 `host_checkpoint` call sites (timers, events, frames, fetch, XHR, navigation, idle, observers). `Browser::drain` and `browser_custom.rs:769` call `drain_microtasks` at the top. `page.rs:671,734` read `pending_jobs` to decide whether the page is idle.
- `Host::between_jobs` (`browser.rs:1002`) runs the queued inline scripts, so a promise continuation of the inserting code sees what they define. `Host::promise_rejections` builds `PromiseRejectionEvent`. `Host::report_exception` reports a throw of a `queueMicrotask` callback.
- The checkpoint does nothing when a script function is on the stack (a host call below it). That is the HTML rule.

### 3.4 Measurements

Machine: archMachine (Ryzen 9 5900HX, load average 4 to 10 during the runs; only instruction counts are used). Build: release, `bare-js` at `b0a0df89`. The harness is a scratch example (`async_cost_tmp`, not in the repository; a copy is in `/data/bare-tmp/js2-async-tmp/`). It runs one case per process, with a counting global allocator. Instructions: `perf stat -e instructions:u`, 3 runs at n = 20,000 and 3 at n = 40,000. The cost of one step is (instructions at 40,000 less instructions at 20,000) / 20,000. The spread over the 3 runs is below 0.01% for every case (each count is within 1,200 instructions of the others, over 20 to 230 million). `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`, as `tools/js/instr.sh` does. The modes are `BARE_JS_EXEC=tree` (default) and `auto` (bytecode lane for the code that it accepts; generators and async bodies are not accepted, so only the loops around them change).

The programs (n is the scale; `reader` returns `out` after `drain_microtasks`, and the checksum is checked):

| Case            | Program                                                                                 |
| --------------- | --------------------------------------------------------------------------------------- |
| `loop`          | `function(n){var s=0; for(var i=0;i<n;i++) s+=i; out=s}` (baseline: no suspension)      |
| `await_prim`    | `async function f(n){var s=0; for(var i=0;i<n;i++) s+=await i; return s}`               |
| `await_prom`    | the same with `await p`, `p = Promise.resolve(1)`                                       |
| `async_noawait` | `async function f(i){return i}` called n times                                          |
| `then`          | `p = Promise.resolve(0); for(...) p = p.then(function(x){return x+1})`                  |
| `gen_next`      | `function* g(n){for(var i=0;i<n;i++) yield i}`, `while(!(r=it.next()).done) s+=r.value` |
| `gen_forof`     | the same generator, `for (var x of g(n)) s+=x`                                          |

Instructions per step (gross: the loop around it is in the number):

| Case            | `tree` | `auto` | What one step is                                               |
| --------------- | -----: | -----: | -------------------------------------------------------------- |
| `loop`          |  1,085 |    186 | one loop turn                                                  |
| `await_prim`    |  2,900 |  2,900 | one `await` of a number, one job, no promise                   |
| `await_prom`    |  3,344 |  3,377 | one `await` of a native promise                                |
| `async_noawait` |  2,672 |  2,087 | one call of an async function that returns at once             |
| `then`          |  5,734 |  4,418 | one `then` on a pending promise, with its closure (210 in js2) |
| `gen_next`      |  5,305 |  3,951 | one `next()` call, result object, one `yield`                  |
| `gen_forof`     |  3,001 |  2,423 | one `yield` read by `for-of` (no result object)                |

Reading the table:

- An `await` costs about 2,900 instructions in both modes. The machine, not the loop, sets the cost. Net of a bytecode loop turn (186) it is 2,714. A `yield` read by `for-of` costs about 2,240 (`auto`) net of one loop turn.
- Both numbers are about 7 to 9 times the cost of a js2 script call (316).

Memory per suspended frame (a counting allocator; live malloc bytes after the script, slope between n = 10,000 and n = 20,000, less the same loop that keeps `{}` objects, which costs 216 bytes per object; heap slabs and tables included; 3 runs, identical):

| Case                                              | Bytes per item | What the item is                                                                                           |
| ------------------------------------------------- | -------------: | ---------------------------------------------------------------------------------------------------------- |
| not started generator                             |            317 | generator object, a boxed `Run` with empty vectors, an entry of `aw.gens`                                  |
| generator at a `yield` inside `for` and `try`     |            731 | the same, 5 continuation frames, and the garbage result object of the first `next()` (about 200)           |
| async function at an `await` of a pending promise |            781 | `Run` in `aw.acts`, 5 frames, the result promise with its table entry, the reaction on the awaited promise |
| settled promise (async function with no `await`)  |            269 | the promise object and its table entry (n = 1,000, one run)                                                |
| pending `then` step                               |            672 | closure, derived promise, table entry, reaction (n = 1,000, one run)                                       |

So a suspended generator costs about 530 bytes with the result object left out (731 less 200), and an async activation with its result promise costs about 780.

Not measured: wall time (the machine was loaded), peak RSS, the cost of a collection with many suspended runs, the bytecode lane with `byte` mode, and the cost split inside the 2,900 (a callgrind run would give it). The `then` row includes the `Closure` and the loop turn.

## 4. How other engines solve it

Sources: V8 blog `https://v8.dev/blog/fast-async`, read 2026-11-20. QuickJS: `quickjs.c` of the tarball `quickjs-2026-06-04` (`async_func_init`, `async_func_resume`, `js_async_function_resume`, `OP_await`, `OP_yield`), read 2026-11-20. The other rows are from the engines' published design and my memory of their source. They are marked "not re-read". Do not use a detail of those rows without checking it first.

| Engine         | Suspended frame                                                                                                                                                                                                                                                                                                   | `await`                                                                                                                                                                                                                                                                   |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V8 (Ignition)  | Not re-read: a generator object holds the context, the receiver, a bytecode offset and a `FixedArray` of registers. `SuspendGenerator` copies a register range into it and returns. `ResumeGenerator` copies it back and a switch on the resume state jumps in. A resume mode picks next, throw or return.        | Read (blog): `PromiseResolve` returns a native promise as is, so `await` takes 1 tick, not 3. The throwaway promise is gone unless `async_hooks` is on (V8 7.2, Chrome 72). An async function has an implicit promise that it returns to the caller at the first `await`. |
| SpiderMonkey   | Not re-read: an `AbstractGeneratorObject` holds the callee, the environment chain, the arguments object, the resume index and an array that gets the expression stack on `Yield` and `Await`. A resume pushes a new frame and copies the stack back. Locals live in the frame or in the environment.              | Not re-read: `Await` suspends the generator-like object and attaches reactions; the frame is resumed by a reaction job.                                                                                                                                                   |
| JavaScriptCore | Not re-read: the bytecode generator emits `op_yield` and a "generatorification" pass rewrites the bytecode into a state machine. A liveness analysis finds the registers that are live across a suspension and saves only those into the generator's storage. A switch on the state at entry resumes.             | Not re-read: async functions are compiled like generators and a promise-based driver resumes them.                                                                                                                                                                        |
| QuickJS        | Read: the whole frame is heap-allocated at the call (`async_func_init` mallocs the state, the arguments, the variables and the operand stack in one block). `JS_CallInternal` runs on that frame with `JS_CALL_FLAG_GENERATOR`. `OP_yield` and `OP_await` store `pc` and `sp` and leave. A resume calls it again. | Read: each `await` calls `js_promise_resolve` (no extra promise for a native one), then makes two new function objects (resolve and reject) that point to the state, and `perform_promise_then`. So one `await` allocates two function objects.                           |
| Hermes         | Not re-read: async functions are compiled into generators and a helper drives them. A generator body is a separate inner function whose frame is saved into the generator object and restored on resume.                                                                                                          | Not re-read: the driver calls `then` on the awaited value.                                                                                                                                                                                                                |

What we take from them:

- V8, SpiderMonkey, JavaScriptCore and Hermes copy the registers out and in. QuickJS keeps the frame in the heap. Each design fits its engine: QuickJS frames are pointer-based, while js2 frames are indexes into one `Vec`.
- JavaScriptCore saves only the registers that are live across the suspension. js2 has this information already (the root maps), and the collector needs a record with no stale pointers (section 5).
- V8 shows that `await` needs 1 tick and no extra promise. QuickJS shows the cost of a closure pair for each `await`. The design avoids both.
- V8 resumes through a switch on a resume mode at each suspension point (next, throw, return). js2 can do the same in bytecode (section 6.4).

## 5. Options for suspended frames

All three keep the semantics of the spec. The questions are where the state lives while the function is suspended, and what the hot loop must change.

### 5.1 A. Copy the live registers into a heap record on suspend, and back on resume

Data structures. An activation is an object with internal slots: state (start, yield, running, done), the resume index, the saved-frame record, the link of the job queue, the result promise (async only) and the pending sent value and mode. The saved frame is a `Slots` record: the callee, `this`, the current context, the pushed-context count, `new.target`, the arguments object, and then the live registers. The compiler gives each suspension point a save list: the registers that are both live (the root map of that point) and written. The list goes in `Code::tables`; the `Yield` or `Await` op names it.

Algorithm.

- Suspend (`Yield`, `Await`): write the live registers of the list into the record in list order, write the resume index and the record words, pop the frame like a return, and put the value (a yielded value or the result promise) into the register of the caller.
- Resume (`next` and the jobs): check the state, push a frame at `fp + nregs + REC` as `enter!` does, write the record words, copy the live registers back, call `reload!`, write the sent value and the mode into two registers, jump to the resume pc.
- The record is allocated once, at the first suspension, with room for the largest list of the function (a field of `Code`). Later suspensions do no allocation, so `Yield` is not a safepoint.

Cost per `yield` and resume, from the numbers of 2.11 (hypothesis, not built): about 320 for the frame push and pop that a call makes, plus 8 for each saved register (4 to save, 4 to restore), plus about 40 for state and mode. With 5 live registers: about 400. A `next()` call through the native adds a native call (348 for `isNaN` today) and the result object (179). About 700.

Memory per suspended frame (hypothesis; size classes of S1.3 not checked): the activation 48 to 64 bytes (header and 3 to 6 slots) and the record 8 + 8 x (6 + n) bytes. For 5 live registers: 48 + 96 = 144 bytes for a generator; 64 + 96 + 32 (result promise) = 192 bytes for an async function.

GC. A suspended frame is reachable only through its activation object. The marker marks the `Slots` record like any record. The record holds only live registers (the others are not in it), so it holds no stale pointer, and the collector needs no root map and no program access to trace it. A generator that nobody reaches is freed with its frame, and its `finally` blocks do not run (the spec says so). No side table, no prune step.

A running activation is an ordinary frame. Its safepoints have root maps as any frame. The ops that suspend first do the allocation (`gate!`, the record, the promise), then copy with no allocation between, so no collection can see a half-written record.

#773 `regs`. A resume goes through the same `ensure!` and `reload!` that a call does, so `regs` is set after the stack may have grown. A suspend ends with the same frame pop as `ret!`. No new place keeps a `regs` pointer.

#785 nested runs. The saved state does not name a stack. A generator that suspends on a pooled `Machine` resumes on any other machine or on the main stack. A resume that a native starts (for example `gen.next()` through `call_function`) takes a machine, builds the frame there and runs. The collector stays held in that nested call, as for any nested call. A resume that script starts goes through `Outcome::Resume` (below) and stays in the same `run`: no nested call, no hold, no Rust recursion.

`Op::Lazy`. A generator function has a stub like any function. `Lazy` compiles it into the same code index and runs it from pc 0, and the first suspension is in the real code. The code index in the record stays valid because the table does not move. Bytecode flushing (not built) must pin the code of a function that has a suspended activation. Section 11 lists it.

`try`/`finally`, `return()` and `throw()`. A handler is a pc range, so a resume that jumps into the middle of a `try` block is covered by the same handlers. The pushed-context count in the record keeps the handler depths right. For the three resume modes the compiler emits, after each suspension op, a short dispatch: mode next continues; mode throw runs `Throw` at that pc (the existing handlers catch it); mode return runs the code of a `return` statement at that place, which already copies the `finally` blocks of the enclosing `try` statements and closes the iterators of enclosing `for-of` loops (fact 8). V8 does the same. The code size grows by one `return` path for each suspension point inside a `try` with `finally` (hypothesis: small; S3-G measures it).

### 5.2 B. Allocate the frame in the heap from the start (QuickJS)

Data structures. A generator call allocates a cell with the frame record and all `nregs` registers. The run executes on that cell: `regs` points into the cell. Suspend and resume copy nothing. Cost per `yield`: about 320 less the copy (hypothesis: 330 to 360 in total).

Memory per frame: 8 + 8 x (10 + nregs) bytes, plus the activation. For `nregs` = 14: about 200 + 48 = 250 bytes, because dead registers are kept.

Why it does not fit js2:

- The frame record is read by index into the stack: `record(stack, fp, k)`, `int_at`, `ret!`, `enter!`, `unwind`, `walk_frames` (34 places in `interp.rs`, fact 1). A frame in a cell has no index in the stack. Every one of these places needs a second form or a branch. A call from the heap frame needs a stack top, which js2 derives from `fp + nregs` today. The change is in the hot paths of calls and returns, which the plan protects (the cost rows must not move). I did not measure it; the change is large, so the risk is high.
- The collector must trace a cell that holds dead registers. Either it marks all registers (stale pointers: unsafe, because the register may name a freed cell), or it finds the root map of the saved pc (the marker then needs the program: `Marker::trace` has none), or the frame is cleared at the call and at each suspension (a cost on every call).
- A frame in a cell and nested machines (#785) need a rule for the stack of a call made from the heap frame.

B is the right design for QuickJS and for an engine whose frames are linked records. It is not the right one for js2.

### 5.3 C. Transform the function into a state machine in the compiler

Data structures. Each variable that is live across a `yield` or an `await` goes into a heap context (js2 already has contexts and `GetCtx`, `SetCtx`). The function is compiled as a step function: a switch on the state in the context at the entry, then the body, with each suspension storing the state and returning. Temporaries that live across a suspension (`a + (yield b)`) go to the context too. A resume is a normal call of the step function.

Cost per `yield` and resume (hypothesis): one `SetCtx` for the state (58), a return and a call (316), a switch (about 40), and 35 more instructions for each access of a hoisted variable (`GetCtx` is 58, `Mov` is 23). A loop `for (i...) yield i` reads or writes `i` three times for each turn: +105. About 520 in total. The cost grows with the work between two yields.

Memory per frame: the context (8 + 8 x live variables) and the activation: about 100 bytes for 5 variables (hypothesis).

GC. The context is an ordinary object. No new rule.

#773, #785, `Lazy`. A resume is a call: no new frame kind, no change to `run`. This is the biggest gain of C. `try`/`finally` need care: a jump into a `try` block works with pc-range handlers, as in A. The `return()` resume has the same dispatch as in A.

Why it is not the first choice:

- Every variable that is live across a suspension is slower for the whole life of the function. A generator that runs a long loop between yields (a parser, a state machine, a scheduler loop) pays +35 instructions at each access. Register allocation is the main strength of the VM.
- It needs a liveness pass of its own in the compiler. A already has the data (the root maps).
- It makes the compiler larger than A does: variable hoisting rules, temporaries, the `finally` state, and `arguments`.

C is the design of regenerator and of C# and TypeScript. It fits an engine with no frame control. Here A has the data and the control.

### 5.4 Comparison

All numbers are hypotheses until step G0 (section 9) measures them. "Hot loop" is a change in `Vm::run` for ops that are not generator ops.

| Design                 | Instructions per `yield` and resume (for-of) | Bytes per suspended generator (5 live registers) | GC tracing                                  | Hot loop change                              | Compiler work                     |
| ---------------------- | -------------------------------------------: | -----------------------------------------------: | ------------------------------------------- | -------------------------------------------- | --------------------------------- |
| Old engine (measured)  |                               2,240 to 3,000 |                                              530 | side table, `gen_trace`, `gen_prune`        | none (tree walker)                           | none                              |
| A. copy live registers |                                    about 400 |                                              144 | a `Slots` record, live registers only       | none                                         | save lists, resume code           |
| B. heap frame          |                                   330 to 360 |                                              250 | needs root maps of saved pcs or stale marks | 34 places of frame access, calls and returns | small                             |
| C. state machine       |                                    about 520 |                                              100 | ordinary context                            | none                                         | liveness, hoisting, state machine |

The difference between A and B is about 40 to 70 instructions for each yield (10 to 15%). The difference between A and the old engine is about 6 times. A is chosen because B puts risk in the hot paths for a small gain, and C slows the code between suspensions.

## 6. Recommended design of resumable frames (option A)

### 6.1 Code

- New ops: `GenInit` (the initial suspend of a generator or an async function that must return its object), `Yield`, `Await`, `ResumeMode` (reads the mode that a resume wrote) and an async return `RetAsync`. Each has `cold_path()` as its first statement (the rule of #785 notes: a new arm of a rare op starts with it). The ops are not on the path of an ordinary loop.
- `Code` gets `gen_kind` (none, generator, async, async generator), `gen_regs` (the largest save list) and the save lists in `tables`.
- The verifier treats a suspension op as a point where only the registers of the save list are written afterwards. This keeps the proof "written before read" true after a resume. A save list must be a subset of the registers that are written at that point, and it must contain every register that is read after the resume before it is written.

### 6.2 Calls and resumes

- Generator function call. The call binds the parameters and evaluates the defaults, as the spec says (a throw is a throw at the call). `GenInit` then allocates the activation and the record, saves the frame, and returns the activation to the caller. The body runs at the first `next()`.
- Async function call. The body runs at once, up to the first `await`. The activation and the result promise are made at the first suspension, not at the call. A call that returns before its first `await` allocates one promise (settled) and no activation. A throw in the parameters or before the first `await` becomes a rejected promise: a handler around the whole body ends in a reject.
- `Generator.prototype.next`, `return` and `throw` are natives. They check the state (running: a `TypeError`), write the sent value and the mode into the activation, and end in `Outcome::Resume { gen }` (a new variant beside `Outcome::Enter`). `native_call!` handles it with `cold_path()` and does what `enter!` does, with the frame built from the record. The yield returns a value to the register of the call. The result object `{ value, done }` is made by the frame itself: `native_call!` sets a flag bit in the destination word of the frame (as bit 24 marks a construct call and bit 25 marks `super()`), and `Yield` and the return of that frame build the object into the register of the caller. A resume that `for-of` starts sets no bit, so it makes no object. A `for-of` over a generator whose `next` is the intrinsic skips the result object (the old engine does the same, `iter_native_step`).
- A resume from a job (async) or from the host enters at the top with `Vm::resume`, a sibling of `Vm::call`. The caller word of the frame is `ENTRY`.

### 6.3 Layout (hypothesis; to check in S3-G1)

| Cell             | Layout   | Words                                                                                                              |
| ---------------- | -------- | ------------------------------------------------------------------------------------------------------------------ |
| Generator object | `Object` | prototype from `F.prototype`; internal slots: `state`, `resume` (the index of the resume point), `frame` (record)  |
| Async activation | `Object` | `state`, `resume`, `frame`, `next` (job queue link), `promise` (result), `arg` and `mode` (the pending sent value) |
| Saved frame      | `Slots`  | callee, `this`, context, pushed-context count, `new.target`, arguments, the live registers                         |

How the internal slots of an object are hidden from `Object.keys` is the same question as for the function cells of `builtins.rs` (slots 1 to 3). To verify in S3-G1. If the hiding costs, the activation is a `Slots`-layout cell that a thin generator object points to.

An optimization for later: one cell, with the save list as inline slots of a per-function shape. It saves one allocation (115 instructions) and 8 bytes. S3-G4 measures it.

### 6.4 `yield*`, `for-of` and `for await`

- `yield*` compiles to a loop in bytecode: call `next`, `throw` or `return` of the inner iterator with the received value, and `Yield` the inner result. V8 does the same. It needs the generic iterator protocol (fact 10).
- A `for-of` over a generator uses the iterator protocol in the generic path and an intrinsic fast path when the object is a generator whose `next` is the built-in. The fast path needs no `Symbol` (it tests the prototype).
- `for await` and async generators are built last (section 9, step 5): an async generator is a generator plus an `Await` plus a request queue. The census shows 25 uses of async generator functions in 7 workloads. It does not split `for await` from `for-of` (`stmt.ForOf`, 1,113 uses), so the number of `for await` is not known; the census decides the order.

## 7. Recommended design of Promise and microtasks

### 7.1 Objects

| Cell                | Layout               | Words                                                                                                                                                                                         |
| ------------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Promise             | `Object`             | prototype `Promise.prototype`; internal slots `state` (an int: pending, fulfilled, rejected, with the `handled` bit) and `result` (the value, or the head of the reaction list while pending) |
| Reaction            | `Slots`              | `kind`, `on_ok`, `on_err`, `derived` (a promise, or a capability record), `next` (the list or queue link), `arg`                                                                              |
| Resolving functions | native function cell | slots 1 to 3: the promise and the shared "already resolved" record                                                                                                                            |
| Async activation    | as 6.3               | is its own reaction (section 7.3)                                                                                                                                                             |

A promise is about 32 bytes (a header and two slots, hypothesis). The old engine needs a 104-byte object and a table entry. There is no side table: the state is in the object, so the collector needs no `promise.map`, no prune and no `max_promises` count of a table. `max_promises` becomes a count of live cells that the collector keeps, or goes away. Question 5 asks.

Pending reactions are a singly linked list on the promise (`result` holds the head). Settling reverses it to keep the order.

### 7.2 The queue

An intrusive FIFO in the realm: `jobs_head` and `jobs_tail`. A job is a cell that has a `next` slot: a reaction that was settled, an activation that waits to resume, or a job record for the two rare kinds (a thenable job and `queueMicrotask`). Settling a promise sets `arg` and `kind` of each reaction and appends it to the queue. Nothing is allocated for a job that was a reaction. V8 converts a reaction into a job in place for the same reason (not re-read).

Roots: `Realm::collect` marks `jobs_head`. The tail is reachable from the head. A count of queued jobs gives `pending_jobs` and the `max_queue` limit.

Alternative: a ring buffer of `(kind, handler, arg, derived)` that the realm owns. It avoids the link slot but copies four words for each job and needs its own tracing and growth. Not chosen. The intrusive queue has no growth and no copies.

### 7.3 `await`

The activation is its own reaction: it has the `next` slot, and a suspended activation waits for one thing only.

1. The `Await` op reads the value `v`.
2. If `v` is a native promise whose `constructor` is `%Promise%` (a property read with a cache hit, 64 instructions, or a protector bit later), it is `PromiseResolve(%Promise%, v) = v`. No new promise.
3. If `v` is pending: link the activation into the reactions of `v` (set `kind` to resume). If `v` is settled: set `arg` and `mode` and append the activation to the queue.
4. If `v` is not an object: append the activation to the queue at once, with `arg` = `v`. No promise is made: it would not be visible (the old engine does the same).
5. If `v` is another object: call the slow path out of line. It makes a promise, resolves it with `v` (a thenable runs through a job that calls `v.then`, as the spec says), and goes on as in 3.

Steps 2, 3 and 4 allocate nothing after the first suspension. The tick count is the spec's after ES2019: 1 for a native promise or a plain value. A resume job sets the register of the awaited value (fulfilled) or runs the throw path of 6.1 (rejected).

### 7.4 Rejections

The `handled` bit is set when a reaction is added. A rejection of an unhandled promise appends it to `rejections` (a `Vec` of values in the realm, a root; at most `MAX_PENDING_REJECTIONS` = 1,024, with a `dropped` count). `reported` is a weak list: the collector drops dead entries in `Roots::weak`. The checkpoint ends with `Host::promise_rejections(unhandled, handled, dropped)`, with the same rules as the old engine (`design-host-microtasks.md`). This is the contract that the browser uses today.

### 7.5 The `bare-js-api` contract: who runs jobs, and when

- The realm owns the state and the queue. The host never reads them. The host settles a promise with `resolve_promise` and `reject_promise` and reads it with `promise_status`.
- Nothing runs a job except the host: `Engine::checkpoint` (after a task; a no-op when `api.win` is not null or `nest > 0`, which is the HTML rule) and `Engine::drain_microtasks` (at the top). `Vm::call` and the end of a script do not drain. This keeps the behavior of the browser as it is.
- Each job is a separate entry at the top: a pooled machine, with no `nest` increment and no hold (`design-js2-host-api.md` 3.4, rule 2). The collector can run inside a job at `gate!`. The job queue and the handle stack are roots. A job does not hold the collector.
- Before each job and once when the queue is empty, the drain calls `Host::between_jobs`. At the end it calls `promise_rejections`. A throw of a `queueMicrotask` callback goes to `Host::report_exception` and the queue goes on. A limit stops the drain with a sticky error until `clear_failed_job`.
- `Engine::task` scopes and `Abort::Limit`: a job that runs out of budget ends the drain with `Err(STOP)` as in #785.
- Limits: `max_queue`, `max_reactions` and `max_jobs_per_drain` stay (a counter that is checked on the slow path of a push, a settle and a drain turn). The activation caps of the old engine (65,536 activations, 2^20 frames and slots) go away: the heap limit and the budget bound them, because a suspended frame is a cell.

### 7.6 Where the code lives

`promise.rs` in js2 (state, resolve, `then`, reactions, queue, rejections) in Rust. The static functions `all`, `allSettled`, `race`, `any`, `withResolvers`, and `finally` are rarely hot: they can go in `prelude.js` (compiled at first call, `design-js2-prelude.md`) and call the Rust core. Question 4 asks. The constructor, `then`, `catch`, the resolving functions and `await` are Rust.

## 8. Expected numbers (hypotheses)

These numbers come from the op costs of section 2.11 and the structure of section 6 and 7. They are not measured. Step G0 and P0 of section 9 measure them before the build goes on.

| Step                                                       |   Old engine (`auto`, measured) | js2, A (hypothesis) | Basis                                                                                                                                 |
| ---------------------------------------------------------- | ------------------------------: | ------------------: | ------------------------------------------------------------------------------------------------------------------------------------- |
| `yield` read by `for-of`, with resume                      |                           2,423 |           about 400 | call and return 316, 5 live registers x 8, state 40                                                                                   |
| `next()` call with result object                           |                           3,951 |           about 700 | native call 348, resume 100, result object 179, `yield` 70                                                                            |
| `await` of a number (job)                                  |                           2,900 |           about 450 | suspend 70, enqueue 20, job entry and restore 200, return 100                                                                         |
| `await` of a native promise                                |                           3,377 |           about 550 | the same, a link and a settle conversion                                                                                              |
| async call up to its first `await` (activation, record and result promise made) |                               - |           about 700 | call 316, activation and record (2 x 115), result promise 115, suspend 70; the `await` itself is the row above (450) and is not in this number |
| `then` on a pending promise, with its settle and job       | 4,418 (with a `Closure` of 210) |         about 1,300 | derived promise 115, reaction 115, native call 348, settle 60, job run 316, resolve of the derived promise 150                        |
| bytes per suspended generator (5 live regs)                |                             530 |                 144 | activation 48 (header and 5 words), record 96 (header and 11 words)                                                                                                                |
| bytes per suspended async function with its result promise |                             780 |                 192 | 64 + 96 + 32                                                                                                                          |

The old engine's `await` cost does not depend on the loop (it is the same in `tree` and `auto`). It is the cost of the continuation machine. The gain of A is the removal of that machine: a resume costs the same as a call and a small copy.

## 9. Build plan

Order: promises first, resumable frames with generators second, async functions third. Steps P and G are independent, so two agents can build them in parallel. Each step is one PR of at most about 400 changed lines of code (plan rule), unless noted. Each step records its numbers in `docs/progress.md`.

Why this order:

- The browser slice (S2.8, S5) fails at the first `.then`. It needs Promise, the queue and the host contract before it needs `await`. Many page scripts use promises without `async` syntax.
- Promise needs no change of `Vm::run`, the compiler or the verifier. The risky part (the new frame kind of the loop and the verifier) is in step G. G starts at once, in parallel, so we see its cost early.
- `await` needs both. Async functions then become a small step: an `Await` op, an activation, and the job.
- Generators come before async functions because they test the frame mechanism with no promise.

Checks in every step:

- test262 per feature directory with `tools/js/test262_js2.sh -j 4 -g REGEX`, and the test-by-test table `tools/fixtures/js/test262-js2-baseline.txt`: no test that passes may fail; the new passes are listed. The skip file `test262-js2-skip.txt` loses the entries of the feature.
- The differential runner (`crates/js-diff`): the same script on the old engine and on js2, with the old engine as the reference, in `tree` mode.
- The micro rows of js2 must not move: `interp_load` (`loopfn`, `calls`, `fib`, `closures`, `objects`) and the `op_cost` rows, at least 3 runs, the 0.5% rule of the plan. A new frame kind or a new op must not slow the hot loop. Every new arm of `run` starts with `std::hint::cold_path()`, as the notes of #785 say. Each PR that touches `interp.rs` also runs `objects` and `calls` at `codegen-units=1` on both trees.
- `cargo test --locked -p bare-js2 --lib` and, for steps that touch the API, `-p bare-js-api`. Miri on the new `unsafe` code, if there is any.
- Constant GC: every test of the step also runs with the trigger at 1 KiB (as `script_tests.rs` does) and with a forced collection at each suspension point.

Steps:

| Id  | Builds                                                                                                                                                                                                                                                                                                     | Check                                                                                                                                                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P0  | A scratch spike, not merged: the promise object, reaction and queue in Rust, 200 lines, with a hand-written bytecode loop that calls `then` and drains. It measures section 8.                                                                                                                             | `then` step at or below 1,500 instructions and the promise at or below 40 bytes. If not, the design of 7.1 and 7.2 is checked again.                                                                                                                                                                        |
| P1  | `promise.rs`: promise cells, reactions, resolving functions, `then`, `catch`, thenable job, the queue, `queueMicrotask`, rejection tracking. `Promise` constructor and prototype.                                                                                                                          | test262 `built-ins/Promise` (732 files) except the combinators; the order tests (`then` order, thenable ticks); a GC test with 100,000 pending reactions under constant collection; `pending_jobs`; the limits.                                                                                             |
| P2  | `bare-js-api` methods in `api.rs`: `new_promise`, `resolve_promise`, `reject_promise`, `promise_then`, `promise_react`, `promise_status`, `queue_microtask`, `checkpoint`, `drain_microtasks`, `pending_jobs`, `clear_failed_job`, and the hooks `between_jobs`, `report_exception`, `promise_rejections`. | The shared suite of `bare-js-api` (`design-js2-host-api.md` 5) on both engines, with promise tests added; the unsupported-method tests lose their entries; checkpoint inside a host call is a no-op; a settled-in-call promise and a promise that the host settles later both survive collections.          |
| P3  | The static functions: `resolve`, `reject`, `all`, `allSettled`, `race`, `any`, `withResolvers`, `finally`; `Symbol.species` (needs `Symbol`; until then the constructor is read as `%Promise%` only). Prelude or Rust by the answer to question 4.                                                         | test262 `built-ins/Promise` complete; cost rows of the combinators against the old engine (`promise_all_load`).                                                                                                                                                                                             |
| G0  | A scratch spike, not merged: hand-written bytecode with `GenInit`, `Yield` and a resume in `Vm::run`, 5 live registers, loop of 100,000 yields (the `interp_load` harness). It measures sections 5 and 8 and the micro rows.                                                                               | Per `yield` at or below 600 instructions; no micro row moves by more than 0.5% (the new arms are cold); kill criterion: if a micro row moves, move the bodies into `#[cold]` functions (the #785 note) and measure again; if it still moves, take C.                                                        |
| G1  | The core: the new ops and `Code` fields, the verifier rule, the activation and record layout, GC tracing, `Outcome::Resume`, `Vm::resume`, `next`, `return`, `throw`, the generator prototype objects.                                                                                                     | The verifier tests (a register written before a suspension and read after it; one that is not in the list is rejected); GC tests with a forced collection at each suspension and resume; a generator that suspends on one machine and resumes on a pooled machine (#785); `Op::Lazy` before the first call. |
| G2  | The compiler: `function*`, `yield`, the resume dispatch (next, throw, return), `try`/`finally` and `for-of` close with a `return()` resume. No `yield*`.                                                                                                                                                   | test262 `language/statements/generators` (266), `language/expressions/generators` (290), `language/expressions/yield` (63), `built-ins/GeneratorPrototype` (61), `language/statements/try`; the differential runner.                                                                                        |
| G3  | The iterator protocol in `IterOpen` and `IterNext` (a generic object, with the intrinsic fast path for a generator), spread, destructuring and `yield*`. Needs `Symbol.iterator` (a separate step; the fast path of generators comes first).                                                               | test262 `language/statements/for-of`, `language/expressions/yield` with `yield*`, destructuring and spread tests; `gen_forof` row.                                                                                                                                                                          |
| A1  | Async functions: `Await`, the lazy activation and result promise, `RetAsync`, the job of an activation, the rejection path, async arrows and methods. Needs P1 and G1.                                                                                                                                     | test262 `language/statements/async-function` (74), `language/expressions/async-function` (93), `language/expressions/await` (22), `built-ins/AsyncFunction` (18), the `Promise` order tests; the `await_prim` and `await_prom` rows against the old engine; the differential runner.                        |
| A2  | Async generators, `for await`, the async-from-sync iterator, `yield*` in an async generator. Only if the census asks (it shows 25 uses in 7 workloads).                                                                                                                                                    | test262 `language/statements/async-generator` (301), `language/expressions/async-generator` (623), `built-ins/AsyncGeneratorPrototype` (48), `for-await-of` (1,234).                                                                                                                                        |
| A3  | Browser: `bare-js-api` on `Realm` with fetch, timers and `queueMicrotask` through promises; the browser slice runs a page with `fetch` and an async login flow.                                                                                                                                            | The S2.8 slice rows (the typing page and the frozen staging scripts) against `auto`.                                                                                                                                                                                                                        |
| G4  | Optional: one cell for the generator (6.3), a protector bit for `constructor` reads, and any item that the rows ask for.                                                                                                                                                                                   | A/B on the `yield` and `await` rows.                                                                                                                                                                                                                                                                        |

Benchmark rows to add (in `crates/js2/examples`, with the old-engine program text as `interp_load --source` does): `await_prim`, `await_prom`, `then`, `gen_next`, `gen_forof`, `async_noawait` and the suspended-frame memory cases, with the programs of section 3.4. They run on both engines with `tools/js/instr.sh`.

## 10. Risks

- A new frame kind in `Vm::run`. Option A adds none, but the new ops and the new `Outcome` variant are new arms of the loop. LLVM can move the hot rows when a rare arm changes (the notes of #785). Control: `cold_path()` first, the micro rows in every PR, the G0 spike before the build.
- The verifier. The rule "a register is written before it is read" must hold after a resume. A wrong save list reads a register that nobody restored: a stale value, which is memory corruption. Control: the verifier checks each list; a forced collection at each suspension; tests where a register is written, a suspension happens, and the register is read.
- Code growth. The `return()` resume path is a copy of the `finally` blocks at each suspension inside a `try`. Control: G2 counts the words of the generated code on `test262` and on a real bundle.
- Bytecode flushing (not built): it must not drop the code of a function that has a suspended activation, and a recompile must give the same pcs. Control: pin a code that has a live activation (a counter in the code, kept by the activation), or never flush `gen_kind` codes.
- The collector between `await` steps. A job runs at the top, so the collector may run. The activation and the queue are roots. Control: forced collection at each job.
- Promise ordering. The tick counts are observable (test262 and real pages depend on them). Control: the order tests of P1 and A1, and the differential runner against the old engine, which is exact for the order (`eval_promise.rs` header).
- The adapter holds raw values. The jobs run at the top, so no `Local` of the host is live between jobs. A `Host::between_jobs` that runs a script is a re-entry at the top. Control: the same rule as `design-js2-host-api.md` 3.4.
- `Symbol` is missing. The generic iterator protocol and `Symbol.species` wait for it. The generator fast path and `await` do not.

## 11. Not measured

- The cost of option A, B and C. Every js2 number in sections 5 and 8 is derived from the op costs of 2.11. G0 measures it.
- The size classes of the allocator for the cells of 6.3.
- Wall time, peak RSS and the cost of a collection with many suspended frames, on either engine.
- The split of the 2,900 instructions of an old-engine `await` (a callgrind run).
- A real page: the numbers are micro scripts. The census counts syntax, not how often a suspension runs.
- How many live registers a typical generator or async function has at a suspension. I used 5.
- V8, SpiderMonkey, JavaScriptCore and Hermes details (section 4): not re-read.
- Windows, macOS, arm64.

## 12. Questions for the owner

Each has a recommended answer.

1. May the build start with promises (P1 to P3) and resumable frames (G0 to G3) in parallel, with async functions (A1) after both? Recommended: yes. Promises unblock the browser slice, and G0 shows the cost of the frame design early.
2. Option A (copy the live registers into a heap record) for suspended frames? Recommended: yes. G0 has a kill criterion: if a micro row moves by more than 0.5% after the cold-path work, or a `yield` costs more than 600 instructions, we take option C.
3. May the verifier and `Code` get the save lists and the new ops (a new arm of `Vm::run` for each of `GenInit`, `Yield`, `Await`, `ResumeMode`, `RetAsync`, plus `Outcome::Resume`), all behind `cold_path()`? Recommended: yes.
4. Combinators (`all`, `allSettled`, `race`, `any`, `finally`, `withResolvers`) in `prelude.js` or in Rust? Recommended: the prelude, because they are rarely hot and compile at the first call. The core (`then`, resolve, `await`, the queue) stays in Rust. P3 measures `promise_all_load` against the old engine and moves a function to Rust if it is slower.
5. May the table caps of the old engine go (65,536 activations, 2^20 frames and slots, 2^20 promises), and the heap limit and the budget be the bounds, while `max_queue`, `max_reactions`, `max_jobs_per_drain` stay as counters? Recommended: yes. A suspended frame is a cell, so the heap limit bounds it. Keep the three queue counters because a loop of `queueMicrotask` is not bounded by the heap in time.
6. May the activation be its own reaction (no allocation for an `await`), with one waiting thing at a time? Recommended: yes. It is the largest part of the expected gain for an `await`. The cost is that the activation has a `next` slot that two lists cannot share. A frame awaits one thing at a time, so it never needs two.
7. Does the engine keep the rule "the host runs jobs; `Vm::call` never drains"? Recommended: yes. It is the browser's behavior today (23 checkpoint sites), and the HTML rule.
8. May `Symbol` (at least `iterator`, `asyncIterator`, `species`, `toStringTag`) be scheduled as its own step before G3 and P3? Recommended: yes, a small step first. Until then generators work with `next()` and the intrinsic `for-of` fast path, and `Promise` reads `constructor` as `%Promise%` only.
9. Is `for await` and the async generator (A2) after the browser slice? Recommended: yes. The census shows 25 uses of async generator functions in 7 workloads and the journeys can wait. Re-run the census (P0.1) when a journey is added.
10. Do we pin the code of a function that has a suspended activation, when bytecode flushing comes? Recommended: yes: never flush a `gen_kind` code while a counter on it is above 0.

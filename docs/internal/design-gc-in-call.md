# Design: collecting garbage inside a long call (R2, 2026-11-15)

Status: measured, not built. The owner decides. This document changes no product code.

Owner decision (2026-11-14): write a design for collecting inside a long call. The cause is in `docs/design-craigslist.md` (PR #544): on the ES5 path of `craigslist-home` one `load` listener is one call chain of 45 G instructions. It makes 3.15 M objects (432 MiB accounted, peak RSS 749 MiB). One collect runs at the end and frees 433 MiB. `gc_due()` needs call depth 0, so no trigger at depth 0 can fire.

## 1. Summary

- **The collector can run inside a call. The roots are the problem.** The mark phase is not the limit: `Interp::roots()` already says it is "complete at any time" for interpreter state. The limit is the Rust locals that hold a `Value` or an `ObjId` across a call that can allocate. I counted about 25 such sites in the evaluator, 2 in the array natives that call back, and an unknown number among the 1,997 binding functions of `crates/bare`. A stale handle gives `HeapError::Stale`, not memory unsafety, so a missed root gives a wrong result or a panic. Section 3 gives the evidence.
- **A missed root fails silently.** In my experiment (section 4) a collect inside the call killed the `load` chain, and the journey still ended `ok:true` with an empty console. A page can lose its work and the driver does not see it. Any build needs a stress mode that collects at every safe point (slice 0).
- **Where the 144 B per object goes** (section 5): the slab record is 117 B per object. An array adds 76 to 80 B for its element buffer. 88% of the objects of the call are arrays. 72% of those have one element. The average accounted cost is 144 B and the RSS cost about 234 B, because malloc and `Vec` growth add 1.6 times.
- **The live set is small.** In a fork of the process, a collect with exact active roots left 3 MiB (7,391 objects) from 48 MiB to 384 MiB of use, and 9 MiB at the end. So collection inside the call keeps the heap near the trigger size, not near 432 MiB.
- **The work does not go away, it moves.** The sweep costs about 256 instructions per freed object plus about 14 M fixed per collect. The objects are freed once either way. A trigger at 32 MiB adds 14 collects and about 0.2 G instructions (+0.4% of 47 G); 8 MiB adds about 0.76 G (+1.6%).
- **Recommendation: design A** (a shadow root stack, a pin counter that blocks the collect for every frame that is not yet audited, and a safe point that costs nothing on the hot path). Build it in 5 slices. Expected gain on `craigslist-home` ES5: peak RSS 749 MiB to about 80 to 100 MiB (hypothesis; my one cut-short run measured 152 MiB). Expected gain on the 47 journeys after the `SharedWorker` stub: about -130 MiB of 1,831 MiB (7%, hypothesis), because `craigslist-home` then no longer has the long call. Instructions: +0 to +1.6% on one journey, 0 on the others. Cost matrix: not measured for the final form; a first probe cost +0.79% on average, so slice 2 must remove that cost or stop.
- **Honest ranking:** after the `SharedWorker` stub, GC inside a call is a robustness feature (long scripts, the 1 GiB `max_bytes` limit), not a lever on the benchmark. It is worth building only if the owner wants pages with long synchronous scripts to be safe. A cut of the bytes per object (design D) is cheaper, independent, and lowers the peak of every page.

## 2. How this was measured

- Machine: archMachine, shared with other agents. Instruction counts (`perf stat -e instructions:u`) are used under load. Wall time is not used.
- Journey: `craigslist-home`, ES5 path (corpus `/data/bare-tmp/corpus/virtual-load`, which was filled before the `SharedWorker` change), `BARE_SEED=1`, `BARE_CLOCK=virtual`, `BARE_NET_REPLAY=...`. Base: main `4a27b7a9` (equal to #544).
- Temporary instrumentation, not in the repository (saved in `/data/bare-tmp/r2/gc-in-call/`: `instrument.patch`, `live-final.patch`, `ic-final.rs`, `overhead-probe.patch`): a heap probe every 48 MiB of used heap, a byte-weighted allocation sampler (64 KiB per sample, top call frame), a fork-and-collect probe (the child process collects with exact active roots and reports the live set and the instruction cost, the parent continues), and an in-parent unsafe collect (`BARE_CL_LIVE=<MiB>`).
- Symbol build: `/data/bare-tmp/target-gc-in-call`, `-p bare-drive`, line tables only.
- Micro cases: 500,000 garbage objects in a top-level loop, 3 runs each, RSS from `wait4` `ru_maxrss`, divided by the iteration count. Page served from a local `http.server` (the driver refuses `file://`). Files: `/data/bare-tmp/r2/gc-in-call/micro/`.
- All 47 journeys: one run each on `virtual-load`. Per journey: the peak of the used heap and the largest growth inside one outermost call (`all47.tsv`).
- Cost probe for the hot path: a first, unoptimized version of design A (shadow stack pushes at Binary, Call and `eval_args`, a pin around `call_native`), run with `tools/js/cost_matrix.sh -a -g` on 60 rows (`eval_load`, `arrobj_load`, `promise_load`) against `/data/bare-tmp/base/4a27b7a988/target`, both allocators.

## 3. How the collector finds roots today, and why it needs depth 0

### 3.1 Today

`Interp::collect` (`crates/js/src/eval.rs`) refuses to run when `depth != 0 || stmt_frames != 0` with the error "collect during a call". `gc_due()` has the same two tests, and then `used_bytes > max(GC_MIN_BYTES = 8 MiB, gc_live / 4 * 7)` or an external-byte test. The driver asks `gc_due()` between tasks.

The mark phase starts from:

- the global object and the prototype table,
- the return and completion values,
- `ext_roots`,
- the host roots and `Host::trace`,
- the tables for promises, binary data, structured clone, weak refs, dates, bigints, `Intl`, async functions, generators, proxies and classes,
- `tpl_objs` and modules,
- the environment chains that a live closure reaches (`Callee::Script { env }`).

Then it frees the environments that nothing reached (`free_env`), prunes the side tables of freed objects, and calls `Heap::collect(roots)`. `Interp::roots()` is the same set plus, for the interpreter, all live environments and all objects in `funcs`. It over-approximates. That is safe.

Complexity: mark is O(live objects + live edges). Sweep is O(slab size). Environment free is O(environments). A collect has a fixed part of about 14 M instructions (measured) and a part of about 256 instructions per freed object.

### 3.2 Why depth 0 is required

The rule that makes depth 0 safe is written in `eval_host.rs`: "Nothing is collected during a call, so a host may hold handles in Rust locals while it runs." Everything below depends on it. The holders are Rust locals:

| Kind | Where | What is held |
| --- | --- | --- |
| Operands | `eval_expr_inner` Binary: `let a = self.eval_expr(left)?; let b = self.eval_expr(right)?;` | `a` while the right side runs |
| Call | `link_inner` Call, `eval_args` | the callee, `this`, and the `out` vector of arguments (from `arg_pool`) |
| Literals | `eval_array`, `eval_object`, `object_shaped` | the array or object under construction; `small: [Value; 3]` and `big: Vec<Value>` |
| Member access | Index | the base object while the key runs |
| Assignment | `eval_assign` `Ref` | the base and the key while the right side runs |
| Callee setup | `call_fn_inner` `D::B(.., Vec<Value>)`, `D::N` | cloned arguments, `this`, native arguments |
| Natives that call back | `ao_iterate`, `ao_reduce` in `eval_array_object.rs` | `this`, the callback, the result array `out`, the element `v`, the accumulator `acc` |
| Statement machine | `eval_statements.rs` `SFrame` | `saved_ret: Value` and environment ids (a local `Vec` per run) |
| Host bindings | `crates/bare`: 1,997 functions that take `it: &mut Interp<'_>`, in 83 files | any `ObjId` from `it.get_value(..)` (111 sites) and across `it.call(..)` (29 sites) |
| Event dispatch | `browser_events.rs` `dispatch`, `dispatch_inner` | `ev`, `l.cb`, `cur` in locals; the event table entry |

Call-site counts that the audit must read (grep, `crates/js`): `eval_expr(` 49, `operand(` 21, `link(` 7, `eval_args(` 4, `call_fn(` 91, `get_prop(` 158 to 166, `coerce*` about 190.

The handles are generational (`ObjId { index: u32, gn: u32 }`). A freed slot gets a new `gn`, so a use after free is `HeapError::Stale`. It is not undefined behavior. The failures I saw (section 4.3) were a `Stale` error in a native, and a panic in `browser_events.rs:221` (`events.get_mut(&ev).expect("event is registered")`) because `Host::prune` had dropped the event entry of an object that the dispatch loop still held.

A statement-level loop at depth 0 has the same problem: `stmt_frames != 0` while a top-level `for` runs. A micro case with a 500,000 iteration loop confirmed that the loop is not collectable either.

## 4. Where the objects of the call come from

### 4.1 The mix

Heap probe at 48 MiB of used heap (336,800 objects):

| Item | Count | Share |
| --- | ---: | ---: |
| arrays | 294,930 | 87.6% of objects |
| arrays with elements | 273,919 | |
| arrays of length 1 | 213,295 | 72% of arrays with elements |

Byte-weighted allocation sampler, top frame, at 48 MiB:

| Rank | Function | Share of bytes | What it is |
| ---: | --- | ---: | --- |
| 1 | `z` | 43.6% | the locale-matcher rule test: `n.split("-")` |
| 2 | `E` | 30.5% | the `__spreadArray` helper: `arguments`, `slice`, `concat` |
| 3 | `R` | 15.9% | the rule scan loop |
| 4 | the `reduce` callback | 3.1% | |
| 5 | all others | under 6% together | |

Inclusive, `F` and `R` account for about 97%. This is the matcher of `design-craigslist.md`: a loop that splits a short locale tag and spreads arrays for each rule.

### 4.2 The live set inside the call

A fork-collect probe (the child collects, counts and exits; the parent goes on). With exact active roots (the frame-stack bindings and the environment chains of each active call):

| Used heap at the probe | Live after collect |
| --- | ---: |
| 48 MiB to 384 MiB | 3 MiB (7,391 objects), constant |
| 432 MiB (the app factory has run) | 9 MiB (40,391 objects) |

With the over-approximate `roots()` (all environments, all functions) the live set looked like 6 to 24 MiB and then 39 MiB. The first fork probe used it and gave a wrong picture; the exact mode corrected it.

Cost of one collect in the child (precise-active mode, one run each):

| Objects at the collect | Instructions |
| ---: | ---: |
| 336,800 | 100.1 M |
| 688,262 | 190.0 M |
| 1.40 M | 371 M |
| 2.10 M | 546.3 M |
| 3.15 M | 858.4 M |

The fit is about 256 instructions per freed object plus about 14 M fixed. A real in-parent collect (32 MiB to 3 MiB, 210,720 objects) cost 67.8 to 69.8 M instructions. Arithmetic (not measured end to end): a 32 MiB trigger gives about 14 collects, +0.2 G (+0.4% of 47 G). A 16 MiB trigger gives about 27, +0.38 G (+0.8%). An 8 MiB trigger gives about 54, +0.76 G (+1.6%).

### 4.3 The in-parent experiment, and why it is not a clean number

I made the parent collect inside the call (trigger 32 MiB, polled at the entry of a script call every 4,096 steps) and rooted holders one class at a time:

1. Unrooted: panic at `browser_events.rs:221` (event table pruned).
2. Dispatch values rooted: the run ended `ok:true` at 4.0 G instructions with an empty console. The first collect had killed the `load` chain. The `Stale` came from `eval_array` (an array under construction) under `ao_iterate`.
3. A shadow stack for Binary, Call, `eval_args`, `eval_array`, `eval_object`, Index and the assignment `Ref`: `Stale` in `ao_iterate` (the result array).
4. Native `this` and arguments in `call_fn_inner` and `out` in `ao_iterate` rooted: the run reached 44.1 G instructions, 13 mid-call collects, peak RSS 152,160 KiB, `ok:true`, empty console. A `Stale` remains at `eval_array_object.rs:500` (`ao_reduce`). So the chain may still end early.

I stopped after this (common-brief rule: two fix attempts at one problem). **The 152 MiB and the 44.1 G are not valid results**: the run is cut short by a `Stale`, and the build carries the probe code. They show only that (a) the collect runs inside the call, (b) 13 collects keep the peak far below 749 MiB, (c) the audit is needed and the failures are silent. The console was empty in the cut-short runs; I did not check whether the base run prints anything, so the console cannot tell a good run from a cut one. The stress mode of slice 0 must compare the DOM or a checksum instead.

## 5. The 234 B per object

Micro cases, 500,000 garbage objects, RSS per iteration (3 runs each):

| Case | RSS per object |
| --- | ---: |
| empty array | 121.7 B |
| empty object | 121.8 B |
| `{a, b}` | 122.1 B |
| 5-property object | 213.9 B |
| array literal with 1 element | 198.1 B |
| array literal with 3 elements | 197.7 B |
| `"a-b".split("-")` | 197.5 to 197.6 B |
| `slice` of 1 element | 197.7 B |
| `concat` | 314.8 B |
| closure | 185.7 B |
| `(function(){ return arguments })(i)` | 384.2 B |
| string `'x' + i` | 154.9 B |

Where the bytes are:

- **Slab: 117 B per object.** The dense record is 104 B (`proto`, `shape`, flags, `elems: Vec<Option<Value>>` header, `repr: In([Value; 3]) | Heap(Vec<Value>)`, `n`, `dict: Option<Box<PropMap>>`). Plus `meta` 8 B (`gn`, `dense`), `back` 4 B, and a mark byte 1 B. An empty object costs about 121 B of RSS: the 117 B and 4 B of `live` growth slack.
- **Elements: 76 to 80 B for an array with 1 to 4 elements.** `Vec` has a minimum capacity of 4: 4 times 16 B = 64 B, plus the malloc chunk header, a 80 B chunk. `Value` is 16 B and an element is `Option<Value>`, also 16 B. Probe: at 48 MiB used the `elems` capacity was 23 MiB for 9 MiB of used elements (capacity over use 2.6).
- **Allocator and growth: about 1.7 B of RSS per accounted byte,** plus a floor of about 15 MiB. At 48 MiB used the RSS was 97 MiB; at 432 MiB it was 703 MiB.
- The mix for this call: 144 B accounted per object on average (432 MiB over 3.15 M), 234 B in RSS by the figure of the brief.

Can it be cut? Yes, without any GC change, but by 20 to 40%, not by 10 times:

- Exact-fit `elems` (capacity equal to length for literals and `split`/`slice` results): a 1-element array goes from about 198 B to about 154 B (-22%). Estimated -10 MiB of 82 MiB at the 48 MiB point (hypothesis, from the capacity probe).
- A smaller record (design 2a of `docs/design-js-memory.md`): fewer than 104 B. See that document for its own estimate.
- Not creating the object: the `arguments` object of `__spreadArray` is 384 B per call and is read only for `.length` (hypothesis: a parse-time check "arguments used only as `.length`" removes it). `split` results of one element are a case for an inline-one-element array. These are per-pattern gains; I did not measure them.

## 6. What real engines do

I read these sources on 2026-11-14 and 2026-11-15. Where I did not read the source, the line says so.

| Engine | How it finds roots in the middle of a call | Trigger | Source |
| --- | --- | --- | --- |
| QuickJS | Reference counts make every local exact. The cycle collector runs after that. | `js_trigger_gc` in `JS_NewObjectFromShape`: collect when `malloc_size + size > malloc_gc_threshold`. The threshold starts at 256 KiB and is `malloc_size * 1.5` after a collect. | `quickjs.c` |
| Ladybird LibJS (LibGC) | Conservative scan of the native stack, with `setjmp` to spill registers; `MarkedVector` and root vectors for heap buffers; a `DeferGC` counter to defer the collect. | `Heap::will_allocate`: collect when bytes allocated since the last collect plus the request exceed the threshold. `GC_MIN_BYTES_THRESHOLD` is 32 MiB. | `Heap.cpp` |
| Boa (`boa_gc`) | `Gc<T>` handles; rooting is counted by RAII on the Rust stack (`trace_non_roots`). | In `Gc::new` when `bytes_allocated > threshold`; the first threshold is 1 MiB. | `boa_gc` |
| V8 | Handles on a handle stack; a `HandleScope` frees the handles of the scope; compiled code uses precise stack maps. An object is garbage if it is unreachable and no handle refers to it. | Allocation failure in a space; external memory soft limit (`UpdateExternalMemory`). | embedder guide; I did not read the stack-map code |
| SpiderMonkey | Exact rooting: `Rooted<T>`, `Handle<T>`, `MutableHandle<T>`. A moving GC needs every pointer. | Allocation thresholds from the live size; malloc threshold. | `RootingAPI.h` (SMDOC Stack Rooting) |
| JavaScriptCore | Conservative scan of the stack and registers, because C++ cannot show its locals. This avoids a rooting API. | Allocation budget (Riptide). | WebKit blog; I did not read the source |

What carries over:

- Every engine collects in the middle of a call. All of them trigger on bytes allocated, not on a call depth. All of them have an opt-out for native code that is not safe (`DeferGC`, `AutoSuppressGC`). None of them needs the host to be correct everywhere on day one.
- The exact designs (SpiderMonkey, V8, Boa) pay a rooting cost in every function that holds a pointer. SpiderMonkey and V8 pay it in the whole embedder too. The conservative designs (JSC, Ladybird) pay nothing per function and need the stack scan.
- A tree walker has the same shape as Boa's evaluator: Rust recursion, a `Value` in a Rust local. Boa chose RAII roots. Ladybird chose a conservative scan.
- A bytecode VM keeps its operands on its own stack, so its roots are exact at no cost. bare stopped the bytecode lane at the R1 gate (2.49x against 3x needed, `docs/engine-plan.md`); the tree walker is the only executor.

## 7. The options

Each option lists: data structures, algorithm, complexity, memory layout, safety risk, code to change, and the gain on three measures: `craigslist-home` ES5 peak RSS, the 47 journeys (instructions, PSS), and the cost matrix. All gains are hypotheses unless a measurement is named.

### Option 0: the current design (no collect in a call)

- Roots: exact, at depth 0 only. Nothing to maintain. Host code may hold handles in locals.
- Complexity: none per allocation, none per call.
- Gain: none. Peak RSS 749 MiB on the ES5 page. A page that makes more than 1 GiB of garbage in one call fails with `Limit::Bytes` (hypothesis: not tested; the limit is checked in `Heap::charge`).

### Option A: a shadow root stack with a pin counter (recommended)

**Data structures.**
- `Interp::tmp: Vec<Value>`, a shadow stack of values held by the evaluator. `roots()` and `collect` mark it.
- `Interp::pin: u32`, a counter. While `pin > 0` no collect runs. Every native frame and every host call increments it by default. A native or a host entry that has been audited opts out (`call_rooted`, or an allow-list flag on the native).
- `Interp::next_poll: u64`, a step count. Instead of a new test in the hot path, the existing `tick()` compares `steps > step_limit`. Set the compared limit to `min(step_limit, next_poll)`. When it trips, `tick` checks the real limit, then calls `poll()`, then sets `next_poll = steps + 4096`. This costs nothing on the path that does not trip (hypothesis; the experiment polled this way with a 4,096 step period, but I did not measure the poll's cost separately).
- A truncate mark: each `call_script` records `tmp.len()` at entry and truncates at exit. An error return (`?`) then needs no guard at each push, because the frame that catches it truncates.

**Algorithm.**
1. `poll()` runs at the entry of a script call and at a statement start (so a loop that makes garbage in one frame is covered). It tests `pin == 0 && used_bytes > threshold`. The threshold is `max(GC_MIN_BYTES, gc_live * 1.75)` as today. A native-byte test shares the same counters as `gc_due()` (section 9).
2. At a holder site, push the value before the call that can allocate and pop after: `tmp.push(a); let b = eval_expr(right)?; let a = tmp.pop();`. The site is skipped when the right side is a literal or an identifier (static elision: an identifier read or a literal cannot allocate, so `a` cannot be freed).
3. A native that calls back (`ao_iterate`, `ao_reduce`, the other array callbacks, `Function.prototype.call`, `apply`, `bind`, `Reflect.apply`) roots its own `this`, callback, result and accumulator, and opts out of the pin. All other natives keep the pin.
4. A host entry that runs a long chain (event dispatch, timer, promise job, `run_script`, module evaluation) calls `it.call_rooted(f, this, args, &[roots])`. It lists its own locals and opts out of the pin. Every other `it.call(..)` pins.
5. The error stack of the statement machine: `SFrame` already lives in a local `Vec`. Push `saved_ret` and the environment ids into `tmp` at machine entry, truncate at exit.
6. After a failed `charge` with `Limit::Bytes`, if `pin == 0`, collect and retry once. This lets a call pass the 1 GiB limit.

**Complexity.** Push and pop are O(1). The poll is O(1) when it does not trip. A collect is O(live + slab). The extra work is the extra collects: about 14 M fixed each (section 4.2).

**Memory layout.** `tmp` is a `Vec<Value>` of 16 B entries. Its depth is bounded by the call depth times the sites per frame; with `max_depth` 2048, a few tens of KiB. Nothing per object changes.

**Safety risk.**
- A missed holder gives `HeapError::Stale` or a wrong value, not undefined behavior. The failures are silent (section 4.3), so the build needs the stress mode first.
- The pin default makes every unaudited path behave as today. The risk is bounded by what has been opted out. A native that opts out wrongly is the failure case.
- `Host::prune` drops side tables of freed objects. A binding that holds a table key across a call is a bug (the panic of section 3.2). Hosts stay pinned unless they opt in.
- An error path that skips a pop is handled by the truncate at the call boundary. A `catch` inside the same frame must truncate to its own mark (to verify: a `try` statement has to record `tmp.len()`).

**Code to change.** The evaluator: about 25 sites in `eval.rs`, `eval_statements.rs`, `eval_array_object.rs`. The pin in `call_native` and `call_host`. A new `call_rooted` API. Opt-in in `browser_events.rs` dispatch, the timer loop, the promise job loop, the script runner (a handful of sites). No change to the 1,997 binding functions: they stay pinned. Estimate (hypothesis): a few hundred lines. The rooting in my experiment was incomplete, so it gives no size.

**Gain.**
- `craigslist-home` ES5 peak RSS: hypothesis 80 to 100 MiB from 749 MiB (-87% to -89%), at a 32 MiB trigger. Not measured on a clean run. The cut-short in-parent run measured 152 MiB.
- Instructions: +0.2 G (+0.4%) at a 32 MiB trigger, +0.76 G (+1.6%) at 8 MiB, on this journey only. On the other 46 journeys the growth in one call is 14 MiB or less except `rust-book-search` (75 MiB), `substack-home` (40 MiB) and `nodejs-org-home` (14 MiB); one extra collect each, about 70 M instructions (hypothesis from the per-collect cost, not measured).
- PSS: hypothesis, `rust-book-search` about -45 MiB accounted (about -70 MiB RSS), `substack-home` about -40 MiB accounted. Across the 47 peaks about -130 MiB of 1,831 MiB after the `SharedWorker` stub, 7%. Not measured.
- Cost matrix: a first, unoptimized probe (pushes in Binary, Call and `eval_args`, pin in `call_native`, no elision, no poll) measured +0.79% mean on 60 rows (glibc), +1.06% (jemalloc); 35 rows over +0.5% under both allocators. `fib` +3.1% to +3.6%, `eval_load calls` +2.75%, `loop` +1.8%, `closures` +1.2%, `mapfilter` +1.1% to +1.2%, `objects` +0.7%. That breaks the 0.5% rule. The probe covered 3 of the 25 sites, so the final cost with all sites and no elision would be higher. Static elision and the poll through `tick` are the two things that must remove it. Hypothesis: elision removes most of the pushes (an operand that is an identifier or literal is the common case in `fib`). Slice 2 has a gate on this (section 8).

### Option B: a conservative scan of the native stack (JSC, Ladybird)

**Data structures.** The stack bounds (per thread), a registry of `Vec<Value>` buffers (Ladybird's `MarkedVector`), a candidate test: "is this word a valid `ObjId`?"

**Algorithm.** At a collect, spill the registers (`setjmp`), scan the stack word by word from the current frame to the stack top, treat each word as a possible handle, and mark it. Mark the registered buffers.

**Complexity.** O(stack depth in words) per collect, nothing per push or call. Zero cost on the hot path.

**Safety risk (hypotheses; none tested).**
- `ObjId` is two `u32` fields. The compiler can split them and keep them in different registers or stack slots (scalar replacement). A candidate test on one 8-byte word would then miss a handle. A scan that tests each `u32` pair at 4 byte alignment needs a table of live indexes with `gn`.
- `Vec<Value>` buffers hold the large share of the holders (`eval_args` `out`, `big: Vec<Value>`, `elems`). They live on the malloc heap, so a stack scan does not see them. They need the same registry as option A. The audit of the Vec holders is the same work as in option A.
- Reading stack bytes that were never written is undefined behavior in the Rust abstract machine. It works in practice with C code; with Rust and LLVM there is no guarantee (to verify).
- Registers need arch-specific assembly (x86-64, aarch64, and the Windows ABI). The stack bounds differ on Linux, macOS and Windows. `stacker::grow` makes extra stack segments (`eval.rs` header), so one thread can have more than one stack range.
- A conservative scan keeps garbage alive by accident, so the live set grows by an unknown amount.

**Code to change.** Small in the evaluator (none), plus a unsafe module per OS and a buffer registry at every `Vec<Value>` holder. Estimate: fewer lines than A, more unsafe code.

**Gain.** Same as A on peak RSS and instructions if it worked. Cost matrix: 0 on the hot path, a stack scan per collect (small). The unsafe code, and the per-OS code that the OS-independent core should not have, are why I do not recommend it.

### Option C: rooted handles with RAII (Boa, SpiderMonkey `Rooted`)

**Data structures.** A `Rooted<'a>` guard that pushes its value to a root list in `new` and pops in `Drop`. `eval_expr` returns a `Rooted` instead of a `Value`. Natives take `Handle`s.

**Algorithm.** The same as A, but every live `Value` local is a guard. `Drop` makes the pop automatic on every error path.

**Complexity.** One push and one pop for each value that is held across a call. No static elision, because the type system roots every value.

**Memory layout.** The same `Vec<Value>` of roots.

**Safety risk.** The lowest: a `Value` local that is not rooted does not type-check. The cost is the change to 1,997 binding functions and every native: the signatures change. This is the SpiderMonkey cost for the whole embedder.

**Code to change.** The most: all of `crates/js` and `crates/bare`. Estimate: thousands of lines, in 83 files of `crates/bare` alone.

**Gain.** Same as A. Cost matrix: worse than the probe of A (every `Value` is a guard, no elision). Not recommended: the pin default of option A gives the same safety for unaudited code at a fraction of the change.

### Option D: cut the bytes per object (independent of the collector)

**Data structures.** Exact-fit element buffers; an inline small array (the 1 to 3 element case in `repr`); a smaller `Object` record (design 2a of `design-js-memory.md`).

**Algorithm.** Allocate `Vec::with_capacity(len)` for literals, `split`, `slice` and `concat` results. Grow by doubling only on `push`.

**Complexity.** No change per operation.

**Memory layout.** A 1-element array: about 198 B to about 154 B (-22%). A smaller record lowers all 117 B.

**Safety risk.** Low. No rooting change.

**Code to change.** Small for exact fit (the sites that build arrays from a known length). Larger for the record (design 2a).

**Gain.** `craigslist-home` ES5: about -22% on the array part (hypothesis); with design 2a more; not 10 times. The 47 journeys: every page with arrays, instructions slightly lower (less memory to touch), PSS lower. Cost matrix: hypothesis, 0 to slightly negative. D does not make the long call safe from the 1 GiB limit.

### Option E: allocate less (remove the work)

The cheapest garbage is the garbage that is never made. Targets from section 4.1:
- Do not create an `arguments` object when a function reads it only as `arguments.length` (the `__spreadArray` helper). 384 B per call.
- A one-element array from `split` or `slice` that does not escape.
- Gains are per pattern and per page. I did not measure them. They do not replace a collector: a page with one loop that builds a big structure and drops it still needs one.

### Rejected

- **Statement-window pinning** (pin the values made in the last N statements): a native that loops (`reduce` over 100,000 items) defeats any window.
- **Generational collection**: needs a write barrier at every property store, array store and binding write. That is the whole object model and more than option A. The live set is small, so a generation would help, but a barrier costs on every store (not measured).
- **A bytecode VM with exact operand roots**: stopped at R1 (2.49x), not available.
- **Collect at allocation, anywhere** (QuickJS style): needs every holder exact at every allocation, which is the SpiderMonkey cost (option C).
- **The glibc `M_MMAP_THRESHOLD`, `malloc_trim` and other allocator settings:** excluded by the owner decision of 2026-11-14 (`design-peak-memory.md`).

## 8. Recommendation and build plan

Build **A** in slices, each a PR that can stop. Build **D (exact-fit element buffers)** in parallel as an independent PR. Do not build B or C.

Why A: it is the only option that is safe by default (the pin gives today's behavior to code that has not been audited), has no unsafe code, and has a bounded hot path cost if the two tricks (elision, poll through `tick`) work. Why not B: unsound for Rust and not portable. Why not C: the same safety as A for the cost of rewriting 83 files.

**Slice 0: a stress mode and a check.** `BARE_GC_STRESS=<n>` (a test-only knob): `poll()` collects every n safe points. No pin opt-outs yet, so no behavior change. Add a stale-handle check that turns `HeapError::Stale` in a native into a test failure and not a JS error. Gate: the harness finds the known holders of section 3.2 when they are opted in on purpose. Cost: 0 (test-only).

**Slice 1: the pin and the poll, nothing opted out.** Add `pin`, `tmp`, the poll through `tick`, `call_rooted`, and the truncate mark. Every native and host call pins, so no collect runs inside a call yet. Gate: cost matrix (803 rows, both allocators) within +0.5% on every row, and the journeys' instructions equal.

**Slice 2: root the evaluator.** Add the pushes at the 25 sites with static elision. Run the stress mode on `crates/js` tests, test262 and `exec_diff`. Gate: the cost matrix, no row over +0.5%. If it fails after the second try, stop and record why (the poll through `tick` alone is useless without roots).

**Slice 3: opt out the array callbacks and `call`, `apply`, `bind`, `Reflect.apply`.** Root `this`, callback, result and accumulator in `ao_iterate`, `ao_reduce` and the others. Stress mode again. Gate: the same.

**Slice 4: opt in the host entries** (event dispatch, timers, promise jobs, `run_script`, modules). This is where `craigslist-home` starts to collect. Gate: `craigslist-home` ES5 peak RSS on a clean build with the old corpus, the page's DOM equal with and without stress (a checksum, not the console), instructions +0.4% to +1.6%.

**Slice 5: the trigger.** Replace `gc_due()`'s depth test by `pin == 0`; share the byte counters with the DOM-arena trigger; retry once after `Limit::Bytes`. Choose the trigger size by measurement (32 MiB, 16 MiB, 8 MiB), not by the estimate of section 4.2.

Limits that point at a design problem: the 8 MiB `GC_MIN_BYTES` and the 1.75 factor protect a collect that is expensive when the heap is large and live data small (a sweep is O(slab)). The cost is the fixed 14 M, not the sweep. A cheaper collect walk (C1 of `design-peak-memory.md`) would allow a lower trigger. The 1 GiB `max_bytes` is a limit that a collect inside the call removes for garbage-heavy calls (slice 5).

## 9. Where this touches the work of the other agents

- **Lazy freeing of DOM nodes (DOM arena, GC trigger).** Slice 5 changes the test in `gc_due()` and reuses `add_external` / `ext_since`. It must read the same counters and not add a second trigger. The reclaim of dead trees runs from `Host::prune` inside `collect`. With a collect inside a call, `prune` can free a tree while a binding holds a `NodeId` from it. A binding that is pinned (the default) is safe. A binding that calls back into JS (`dispatchEvent`, `MutationObserver`, `NodeList.forEach`) and is opted in must root the wrapper of its node. This is the main hazard to review against their PR.
- **SharedWorker stub (worker bindings, corpus refill).** The stub removes the long call of `craigslist-home` (47,008 M to 2,682 M by `design-craigslist.md`). After it, the old corpus (`virtual-settled`, or the page with `SharedWorker` hidden) is the only place where this design can be measured on that page. Keep a copy of that corpus for slice 4. The worker bindings stay pinned. No other overlap.
- **Class atoms (style).** None. This design is in `crates/js` and the event loop of `crates/bare`.

## 10. What I did not measure

- A clean in-parent run with every holder rooted (section 4.3). The 152 MiB and 44.1 G are not valid. The peak RSS with collection inside the call is a hypothesis.
- The cost matrix for the final form of A (with all 25 sites, elision and the poll). Only the unoptimized probe on 60 rows.
- Test262 and `exec_diff` under the stress mode.
- The 47 journeys with a collect inside the call: per-journey growth only, one run each.
- Wall time, idle-machine timing, Windows and macOS.
- The size of the audit in `crates/bare` (the number of bindings that hold a handle across a call that can allocate). I counted `it.get_value(` and `it.call(` sites only.
- A `try`/`catch` inside a frame and the truncate mark (section 7, option A).
- The 1 GiB `max_bytes` failure of a long call, and the retry after `Limit::Bytes`.
- Exact-fit element buffers and the `arguments` elision: sizes are from the micro cases, not from a build.
- The malloc side: why 32 MiB of use gives more than 3 times that in RSS in the cut-short run (hypothesis: freed chunks stay in the arena and the probe code adds memory).

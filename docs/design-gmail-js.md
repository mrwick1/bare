# Design: cut the JS cost of the Gmail load

Status: design only. Nothing is built. Owner decision of 2026-11-15: design how to cut the JS cost of Gmail, no build yet.

Base: main `2211c276` (after #569). Task R2-J.

## 1. Summary

After the loader fixes (#565, #567, #569) the Gmail inbox load needs about 17.3 G instructions to the first row and about 104 G to settle. Chrome needs 12 to 19.6 G for the whole cgroup, with a JIT.

The JS interpreter takes 70% of the first-row instructions and 77% of the settle instructions. Parse takes 13.8% of the first row. Nothing else is above 5%.

The cost has three parts:

1. **Tree-walk dispatch.** 50.75 M steps at 268 instructions per step inside functions. The dispatch functions (`link_inner`, `eval_expr_inner`, `exec_simple`, `exec_list`, `machine`, `test_true`) take 30% of self time. Only a tier below the tree walker removes this part.
2. **Calls.** 3.06 M calls. A call costs 653 instructions (frame path) or 839 (env path) before the body runs. That is 10.5% of the thread.
3. **Parse and first runs.** 2.59 G (13.2%). The page thread does this work while the network is idle for 0.46 s.

Recommendation, in order:

| # | Design | Gain on the first row (hypothesis where marked) | Work |
| --- | --- | --- | --- |
| 1 | P1. Parse a script on arrival, on a worker thread | page thread -1.4 G (-8%); time to first row about -0.3 s; total instructions unchanged | S to M |
| 2 | P2. Dictionary receiver reads: position hint, then a shared stub cache | -0.3 to -0.6 G (2 to 3.5%) (hypothesis) | S |
| 3 | P3. A call path with a smaller frame and no `select` | -0.8 to -1.1 G (5 to 6%) (hypothesis) | M |
| 4 | P4. Three small removals (`ErrorCtor`, `Paren` nodes, `str::contains`) | -0.1 to -0.4 G (hypothesis) | XS |
| 5 | P5. Disk cache of parse results | repeat visit only: -0.9 G after P1 (-2.0 G alone) (hypothesis) | L |
| 6 | P6. A tier for hot functions (finish the bytecode lane) | -3 to -4 G (-17 to -24%) (hypothesis) | L to XL |

P1 to P4 together: about -3 G on the page thread (-17%), about -0.7 s on the first row. P6 adds about -3 to -4 G. Together the first row is still about 2.6 to 2.9 s against Chrome at 1.2 to 1.5 s (hypothesis). **Only a JIT closes most of the gap. No interpreter design does.** Section 8 gives the number.

## 2. Measurement

### 2.1 Method

- Page: the Gmail inbox, bare saved session, read only. No click, no input, no login. Each run opened the inbox and waited until the page settled, then closed the tab.
- Machine: archMachine. Load average 3 to 7 during the runs. Instruction counts do not depend on load. Wall time does, so this doc gives no wall time from these runs.
- Build: main `2211c276`, release, frame pointers, symbols. Own target dirs under `/data/bare-tmp/r2/gmail-js/`. Raw output stays there (it holds session data).
- Four tools:
  1. `perf stat -e instructions:u -I 100` on the whole process, 3 runs: the totals.
  2. `perf record -e instructions:u -c 4000000 -g --user-callchains`, 1 run: the split by subsystem and the leaf functions.
  3. A counting build (`KINDS`): exact counts of node kinds, calls, IC events. 1 run. Counts do not vary between runs (steps at the first row: 50.75 M in each of 3 instrumented runs).
  4. An instruction build: a user-space instruction counter (`perf_event_open`, read by syscall) around each call, with self and children kept apart. 3 runs. Each mark pair costs 111 instructions (49 inside the measured window). The page thread reads 19.62 G in this build and 17.3 G in the plain build (+13%). Shares in this doc are shares of the instrumented thread. They are about 13% too low in absolute size.
- First row: the probe `document.querySelectorAll('tr.zA').length>0`, as in #565.
- Script names: scripts have a source length in bytes. This doc uses the length and the offset. It never prints content.

### 2.2 Totals (plain symbol build)

| Run | Instructions to settle (all threads) |
| --- | --- |
| 1 | 115.6 G |
| 2 | 103.2 G |
| 3 | 104.1 G |
| mean | 107.6 G (range 103.2 to 115.6; run 1 was higher; the cause is not known) |
| perf record run | 103.4 G (17.3 G to the first row) |

The 3 instrumented runs agree on the first row to 0.3%: steps 50.75, 50.88, 50.86 M; page thread 19.62, 19.63, 19.62 G; function self 11.72, 11.71, 11.71 G.

### 2.3 Split by subsystem (perf record, 1 run)

| Subsystem | To first row (17.3 G) | After first row (86.1 G) | Whole run (103.4 G) |
| --- | --- | --- | --- |
| JS interpreter | 69.8% | 78.5% | 77.0% |
| JS parse (incl. lazy load) | 13.8% | 2.9% | 4.7% |
| allocator | 4.7% | 4.4% | 4.4% |
| bindings | 2.1% | 5.2% | 4.7% |
| style | 2.0% | 3.7% | 3.4% |
| JS GC | 1.8% | 1.5% | 1.5% |
| decompress | 1.6% | 0.4% | 0.6% |
| regexp | 1.5% | 0.3% | 0.5% |

### 2.4 What takes the steps

Counts to the first row (1 counting run; 50.75 M steps).

| Item | Count |
| --- | --- |
| Expression evaluations: Call 3.34 M, Binary 2.61 M, Number 2.53 M, Member 2.46 M, Ident 2.21 M, Assign 1.69 M, This 1.51 M, Paren 1.13 M, Unary 0.81 M, Index 0.66 M, Logical 0.65 M, Cond 0.62 M, Seq 0.56 M | 21.7 M in all kinds |
| Statements: Return 2.31 M, Expr 2.19 M, Var 0.72 M | 5.2 M |
| Binary ops: Add 594 K, BitOr 485 K, Ne 307 K, Eq 246 K, Sub 207 K, BitAnd 205 K, Lt 142 K, Shl 122 K, StrictEq 90 K, Instanceof 69 K | |
| Script calls | 2.925 M (frame path 2.104 M, env path 0.821 M, first call of a lazy function 11.4 K, constructors 13.7 K) |
| Native calls | 588.7 K |
| Host (DOM) calls | 12.2 K |
| Bound calls / promise functions | 70 / 995 |
| Closures created (`Function` expressions evaluated) | 66.5 K |

Object member reads: 5.49 M.

| Read result | Count | Share |
| --- | --- | --- |
| Own hit (`ic_own`) | 2.075 M | 37.8% |
| Chain hit (`get_ic`) | 1.013 M | 18.4% |
| Refill (own 2.125 M, proto 108 K, absent 18 K) | 2.251 M | 41.0% |
| First run (no entry) | 141 K | 2.6% |
| Refused receiver | 35 K | 0.6% |

Sites that refill (1 counting run):

| Refills per site | Sites | Refill events |
| --- | --- | --- |
| 1 | 11,556 | 11.6 K |
| 2 | 4,544 | 9.1 K |
| 3 to 4 | 2,967 | 9.6 K |
| 5 to 16 | 3,155 | 27.2 K |
| 17 to 256 | 4,787 | 298.9 K |
| 257 and more | 919 | 1.927 M (84% of all) |

919 sites make 84% of the refills. These sites do not fit a one-entry cache.

Not counted as a separate counter in this round: arguments objects, scope-depth lookups, string operations other than the natives below. `get_name` self time is 2.1% (first row).

### 2.5 What a call costs

Instruction build, 1 run shown (3 runs agree to 0.5%). Per call, to the first row. Each part carries up to 49 instructions of mark cost.

| Part | Frame path (2.104 M calls) | Env path (0.821 M calls) |
| --- | --- | --- |
| dispatch (`call_fn_inner`: `funcs` hash lookup, `D` match, `in_frames`, `low_stack`) | 168 | 168 |
| `select` (swap 11 per-script fields) | 127 | 137 |
| setup (`call_frame`, or `new_env` + `bind_call`) | 221 | 358 |
| teardown | 82 | 121 |
| return | 55 | 55 |
| **sum** | **653** | **839** |
| total | 1.374 G | 0.689 G |

Calls cost 2.06 G, which is 10.5% of the instrumented thread. At settle: 16.6 G of 142 G (11.7%).

A function of one step costs 702 instructions per call (script `1,464,865` bytes, offset 91,916: 483,783 calls). The call overhead is the whole cost of such a function. This function costs 2.9 G at settle (3.2% of function self).

### 2.6 First runs against hot loops

| Calls per function | Functions | Calls | Self instructions | First-call instructions |
| --- | --- | --- | --- | --- |
| 1 | 5,021 | 5.0 K | 0.549 G | 0.549 G |
| 2 to 9 | 4,324 | 17.1 K | 0.186 G | 0.076 G |
| 10 to 99 | 2,578 | 88.8 K | 0.822 G | 0.038 G |
| 100 to 999 | 1,087 | 341.3 K | 2.161 G | 0.018 G |
| 1,000 to 9,999 | 334 | 947.3 K | 4.410 G | 0.003 G |
| 10,000 and more | 31 | 1.662 M | 3.590 G | 0.0002 G |
| all | 13,375 | 3.061 M | 11.72 G | |

- Hot (1,000 and more calls): 365 functions, 8.0 G = 68% of function self = 41% of the thread.
- Cold (fewer than 10 calls): 9,345 functions, 0.74 G of self.
- Parse: `parse_top` 1.498 G (29 scripts), `lazy_load` 1.094 G (5,841 lazy functions, run inside `select`). Together 2.59 G = 13.2% of the thread.
- Cold cost (parse and first run) is 3.3 G (17%). Hot loops are 8 to 10 G (up to 52%).

By script (first row, source bytes): `1,464,865`: 3,897 functions, 2.79 M calls, 9.54 G self (81% of function self, 84% of steps; hypothesis: this is `m=b`). `2,034,627`: 5,645 functions, 1.71 G. `1,415,088`: 2,360 functions, 0.30 G.

### 2.7 Bindings, natives

Top natives (first row; 3.06 M calls are script calls; natives are small): `ErrorCtor` 29 calls 111.5 M (3.8 M per call), `Array.push` 36 K calls 105.9 M, `charCodeAt` 149 K calls 66.9 M, `JSON.parse` 38 calls 60.0 M, `RegExp` constructor 144 calls 54.5 M, `String.replace` 11.6 K calls 47.3 M. The top 60 natives sum to 0.78 G (4%).

Hot DOM calls (first row): `get clientWidth` 13 calls 457 M, `set innerHTML` 69 calls 299.5 M, `get response` 1 call 179 M, `getBoundingClientRect` 11 calls 141.4 M. Host calls sum to 1.18 G (6%). Most of this is forced layout (layout work, not JS). At settle `get clientWidth` costs 4.79 G (91 calls).

### 2.8 Top 20 bare functions (perf record, leaf self, to the first row)

| # | Share | Function |
| --- | --- | --- |
| 1 | 9.18% | `Interp::link_inner` |
| 2 | 8.23% | `Interp::eval_expr_inner` |
| 3 | 3.56% | `Interp::exec_simple` |
| 4 | 2.46% | `[unknown]` |
| 5 | 2.42% | `Interp::call_fn_inner` |
| 6 | 2.33% | `Interp::machine` |
| 7 | 2.29% | `Interp::exec_list` |
| 8 | 2.22% | `Lexer::scan_at` |
| 9 | 2.07% | `Interp::get_name` |
| 10 | 1.89% | `Interp::member_ic` |
| 11 | 1.74% | `Heap::get_dict` |
| 12 | 1.68% | `Lexer::next` |
| 13 | 1.68% | `Interp::link_base` |
| 14 | 1.67% | `Interp::call_script` |
| 15 | 1.45% | `Heap::lookup` |
| 16 | 1.39% | `Interp::test_true` |
| 17 | 1.33% | `PropMap::find` |
| 18 | 1.30% | `Interp::eval_assign` |
| 19 | 1.20% | `Interp::select` |
| 20 | 1.18% | `Interp::call_frame` |

The interpreter control functions (1, 2, 3, 6, 7, 13, 16, 18) are 30% of the thread. The lexer (8, 12) is 3.9%. `get_dict`, `lookup` and `PropMap::find` (11, 15, 17) are 4.5%. After the first row `<&str as Pattern>::is_contained_in` is 2.5% (a `str::contains` in the bindings; style path; not JS).

### 2.9 Top 20 JS functions by script and offset (first row, 1 instruction run)

Script is the source length in bytes. Offset is the byte offset of the function start. Self instructions include no children. No content is shown.

| # | Script | Offset | Calls | Self (M) | Per call | Steps per call | Share of function self |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1,464,865 | 473,673 | 255,012 | 670 | 2,628 | 11.0 | 5.7% |
| 2 | 1,464,865 | 473,195 | 9,685 | 524 | 54,101 | 293 | 4.5% |
| 3 | 1,464,865 | 129,956 | 65,307 | 481 | 7,368 | 50.3 | 4.1% |
| 4 | 1,464,865 | 472,585 | 124,080 | 365 | 2,941 | 9.0 | 3.1% |
| 5 | 1,464,865 | 91,916 | 483,783 | 340 | 702 | 1.0 | 2.9% |
| 6 | 1,464,865 | 92,001 | 120,299 | 215 | 1,785 | 5.0 | 1.8% |
| 7 | 2,034,627 | 251,030 | 5,848 | 196 | 33,464 | 168 | 1.7% |
| 8 | 1,464,865 | 51 | 1 | 168 | 167.8 M | 133,448 | 1.4% |
| 9 | 1,464,865 | 475,019 | 43,295 | 158 | 3,648 | 14.0 | 1.3% |
| 10 | 1,464,865 | 337,636 | 34,693 | 142 | 4,102 | 16.9 | 1.2% |
| 11 | 1,464,865 | 110,868 | 370 | 132 | 357,324 | 1,713 | 1.1% |
| 12 | 1,464,865 | 454,240 | 23,381 | 118 | 5,056 | 21.0 | 1.0% |
| 13 | 1,464,865 | 107,909 | 370 | 115 | 311,396 | 1,339 | 1.0% |
| 14 | 2,034,627 | 51 | 1 | 114 | 113.5 M | 137,489 | 1.0% |
| 15 | 1,464,865 | 454,181 | 47,787 | 90 | 1,877 | 7.0 | 0.8% |
| 16 | 1,464,865 | 127,733 | 5,939 | 84 | 14,136 | 77.2 | 0.7% |
| 17 | 1,464,865 | 434,942 | 7,233 | 79 | 10,978 | 41.7 | 0.7% |
| 18 | 1,464,865 | 125,819 | 1,230 | 77 | 62,482 | 372 | 0.7% |
| 19 | 2,034,627 | 247,446 | 56 | 67 | 1.20 M | 4,128 | 0.6% |
| 20 | 1,464,865 | 451,066 | 20,303 | 65 | 3,184 | 10.0 | 0.6% |

The top 20 are 38% of function self. The cost is flat: no single function is above 6%. Hot spots are not the way. Four functions at offsets 472,585 to 475,019 (one region of 2.4 KB) take 1,717 M (14.7% of function self). Hypothesis: one library routine group (an event or iterator helper).

At settle the top rows are: script `128,763` offset 6,145 (328.5 K calls, 17,451 per call, 6.3%); script `1,464,865` offset 402,373 (1.65 M calls, 4.3%); offset 473,673 (1.20 M calls, 3.5%). At settle the profile is again flat.

## 3. The current design

| Part | Structure | Cost model |
| --- | --- | --- |
| Parse | Lexer and recursive-descent parser build an arena AST (`Program`, `ExprId` indices). Function bodies are skipped on the first parse (pre-parse, `lazy_load`). | O(source) for the top parse. O(function) on the first call of each lazy function (a second parse of that range). Both on the page thread. |
| Eval | Tree walk (`eval_expr_inner`, `link_inner`, `exec_simple`). One `tick()` per step (the step counter). | 268 instructions per step inside functions, 386 per step for the whole thread. |
| Scope | Frame slots (`FR_ON` path) for functions that pass `fr_gate`. Env objects with name lookup for the rest. | O(1) by slot index for frame functions. O(depth) hash lookups for env functions. |
| Property read | Shapes. One inline cache entry per site (`Ic`: shape, slot, kind, depth, epoch). `IC_DICT` for dictionary receivers. | Hit about 60 instructions (hypothesis). Refill = `get_ic` or `get_dict` + `Heap::lookup` + `PropMap::find` (hypothesis: about 350). |
| Call | `call_fn_inner`: `FxMap funcs.get(&o)`, `D` enum match, `call_script`, `select` (swap 11 fields), `call_frame` or `new_env + bind_call`, `run_body`. | 653 (frame) or 839 (env) instructions plus the body. |
| Byte lane | Register VM (`bc*.rs`), `BARE_JS_EXEC=tree\|auto\|byte`, default tree. | R1: 2.49x on its benchmark set. Lane stopped 2026-11-08. |

`IC_DICT` hits only when the receiver is the same object as the entry (`o == ic.first`; `heap_shape.rs:1119`). A site that reads many different dictionary objects refills each time. `get_dict` is `#[cold]` and `inline(never)`. It still takes 1.74% self at the first row. Therefore dictionary receivers are a large part of the refills (hypothesis: the share is not counted; P2 step 1 counts it).

## 4. How other engines cut the cost of startup-heavy code

Read 2026-11-15 from the engine authors' posts and docs. This doc gives the design, not the authors' numbers, unless marked.

| Engine | Design | What it gives over a tree walker on startup-heavy code |
| --- | --- | --- |
| V8 | Lazy parse with a pre-parser (skips inner functions, keeps scope data). Ignition: register bytecode. ICs with a feedback vector per function. Sparkplug: a baseline compiler that makes machine code from the bytecode in one linear pass, with the interpreter frame layout. TurboFan for hot code. Code cache: bytecode of the functions that ran is stored on disk after the script is used (hot load). | Bytecode: one compile per function, then a dense dispatch. Sparkplug removes dispatch for code that runs often, with a compile cost near a memcpy. The code cache removes parse and compile on a repeat visit. |
| SpiderMonkey | Baseline Interpreter (runs the same bytecode with the same ICs as the Baseline JIT), Baseline JIT after a threshold, Ion for hot code. Stencils: the parse result is a compact, relocatable form that can be cached and parsed off thread. | A cheap middle tier that shares the data of the JIT tier (no second compile). Off-thread parse into stencils. |
| JavaScriptCore | One bytecode. LLInt (an interpreter in generated assembly), Baseline JIT, DFG, FTL. Tier up by counters. | The interpreter is fast because the dispatch is assembly, not Rust. |
| QuickJS | Compile to stack bytecode. No JIT. Reference counting. | Small and fast to start. Typically slower than a JIT. |
| Hermes | Register bytecode compiled ahead of time. Bytecode file is mapped from disk. Lazy compile of functions. Static scope resolution. | No parse at startup at all, for a known script. Needs an offline step. |
| Boa | Bytecode VM in Rust. | The repo's own note: bare won 6 of 8 rows against it. |

What this says about bare:

1. The engines that win on startup do two things bare does not: **they compile once to a dense form** and **they keep the result (cache or stencil)**. Bare parses twice (top parse, then a second parse per lazy function) and walks a tree.
2. Parse off the main thread is standard (SpiderMonkey stencils, V8 streaming). Bare does it on the page thread, while it waits for the network.
3. A middle tier (SpiderMonkey Baseline Interpreter) works because it shares data with the next tier. Bare's byte lane does not share the IC data and refuses lazy functions.
4. All the fast engines use inline caches with more than one entry (polymorphic ICs) and a global stub cache for megamorphic sites. Bare has one entry per site.

## 5. Designs

All numbers are for the first row of Gmail unless marked. "Hypothesis" means not measured on a build.

### P1. Parse a script on arrival, on a worker thread

Built 2026-11-15 on main `ca67571d` (after the preload rule #570). Code: `crates/js/src/parse_ahead.rs`, the hook in `parse_cache::parse`, `Net::start_with` in `browser_loader.rs`.

**Problem.** The page thread parses the top level of 29 scripts for 1.4 to 1.5 G (about 8% of the thread). Before the preload rule it waited 0.46 s for the bodies. After #570 the bodies arrive earlier.

**Re-check of the idle window on main `ca67571d`** (5 runs of the #570 branch, trace of `BARE_NET_TRACE`). The three big scripts (1.46, 1.41 and 2.03 MB of source) are complete 0.61 to 0.67 s after their request, which is at the first piece of the document. The page thread runs the first one at the end of the document: 0.62 to 0.75 s later (5 runs: 0.63, 0.69, 0.62, 0.65, 0.61 s; the document end is at 1.26 to 2.00 s). The window is larger than the 0.46 s of #569, not smaller. The fourth big script (`m=ebdd`, 2.2 MB) is requested at the end of the document and has no window. About 4.9 of about 7 MB of source have the window.

**Data structures.** One `Ahead` for each page, owned by `Net` (made in `Net::new`, also for the scan before the page exists). One worker thread, started on the first job, ends after 2 s with no job. A registry `HashMap<(hash, Kind), Arc<Slot>>` and a queue. A slot is `Mutex<St>` and a `Condvar`. `St` is `Queued(Rc<str>)`, `Running`, `Done(result)`, `Gone`.

**Algorithm.**
1. The download thread has the text of a classic script (`Net::start_with(.., classic = true)`: scan scripts and `as=script` preload hints, targets that no module element names, dynamic scripts that are not modules). If the text has 16 KB or more (`MIN_AHEAD` = `LAZY_MIN_SOURCE`), it hashes it with the hash of the parse cache, makes the one `Rc<str>` copy that the tree keeps, registers the slot and queues it.
2. The worker pops a slot. `Queued` becomes `Running`. It runs `parse_raw` (the code of `parse_cache`: same parser, same lazy rule, `Limits::default()`) over the `Rc<str>` and swaps the borrow for the owner (`with_shared_source`). It stores the result (`Ok(Rc<Program>)` or `Err`) and the node count, and wakes the page thread.
3. `parse_cache::parse` is the only reader. On a cache miss it asks `parse_ahead::take(kind, hash, source)` first: no slot gives `None`; `Queued` is cancelled (`Gone`) and `None` (the page thread parses; the same text is never parsed twice); `Running` waits on the condvar; `Done` is taken after a byte compare with the source. The result then goes through the same insert code as a fresh parse.
4. `lazy_load` stays on the page thread. It uses `Src::Shared`, which the worker makes.

**Node limits.** The worker parses with the default room (16 M nodes). The page uses the result when its room holds the nodes (`max_nodes >= used`), and for a `Limit(Nodes)` result when its room is not larger than the worker's. Else it parses again. So the result equals the fresh parse with the realm's own room (test: `a_node_room_that_is_too_small_gives_the_error_of_a_fresh_parse`).

**Order and errors.** The worker only fills a slot. It never runs code and reports nothing. A syntax error is stored as the result. The realm gets it at `run_script` or `module_parse`, which is where it got it before (same message, same place; the script that follows still runs after it). Scripts run in the order that the loader chooses: nothing in that order changed.

**Soundness (`Send`).** `Program` holds `Rc`. The module `Hand<T>` has `unsafe impl Send`. The condition: every `Rc` of the tree (the source, the marks, the lazy lists) is owned by that tree, and every other handle (the submitting thread's, the worker's `view`) is dropped before the hand-over. The hand-over goes through a mutex. A `debug_assert` checks that the source `Rc` has one owner at the hand-over (test: `the_tree_from_the_worker_is_the_only_owner_of_its_text`). The `thread_local` parse cache is not touched on the worker.

**Memory.** One copy of the text, as before: the `Rc<str>` that the tree keeps. The loader's `String` is dropped after the script runs, as before. After the hand-over the registry holds nothing. The copy and the tree are made earlier than before (at arrival, not at the turn of the script). A tree that waits for its script is bounded by the same cap as the text that waits (`done`, `max_total_bytes`), and by `WINDOW`. A navigation away drops them: `Net` drops `Ahead`, which closes it, skips the slots that have not begun and drops the finished trees. A parse that runs is not interrupted (the parser has no check): its tree is dropped when it ends.

**Determinism under the virtual clock.** The worker is not a `parallel::Task` and does not read the clock of `bare_net`. The page thread waits for a slot in real time, on a condition variable, and no virtual time passes. The tree is a pure function of (text, kind, limits), so the page sees the same steps, the same errors and the same node counts whether the worker or the page thread made it. Which of the two did it can change from run to run. That changes only the instructions of the page thread, not the replay.

**Measured** (Gmail, saved session, read only, 5 alternating pairs of main `ca67571d` and this branch, plain release build with a probe, load 4 to 8): page-thread instructions to the first row 16.42 to 16.45 G on main and 14.88 to 14.99 G on the branch (-1.5 G, -9%). All threads: 17.0 to 17.1 G and 16.8 to 17.0 G. Time from the end of the document to the first row: median 2.37 s on main, 2.15 s on the branch (-0.2 s). See `progress.md`.

**Size.** About 280 lines with the tests in `parse_ahead.rs`, 25 in `browser_loader.rs`, 15 in `parse_cache.rs`.

### P2. Dictionary receiver reads: position hint, then a shared stub cache

**Problem.** 41% of the member reads refill the cache. 919 sites make 84% of the refills. `get_dict` + `Heap::lookup` + `PropMap::find` cost 4.5% (0.78 G) at the first row and 5.2% at settle. The `IC_DICT` entry names one object (`first`), so a site that reads many objects of one kind (many dictionary-mode records of the same layout) never hits.

**Step 1 (measure, 10 lines).** Count the refills by kind (shape entry, dictionary, chain). Gate: build P2 only if dictionary refills are more than half of the 2.25 M.

**Data structure and algorithm (step 2: position hint).** When `ic.kind == IC_DICT` and `o != ic.first`, read the dictionary of `o` at position `ic.depth`. If the key at that position is `s` (one compare), the value is the answer, and the entry keeps its position. Objects built by the same code path insert keys in the same order, so the position is the same. Cost: about 20 instructions against about 350 for a refill (hypothesis). No new memory.

**Step 3 (only if step 2 leaves refills): a shared stub cache.** One table of 4,096 entries of 16 bytes (64 KB): `shape id (u32)`, `name atom (u32)`, `slot and kind (u32)`, `epoch (u32)`. Index: `(shape.id * 0x9E37_79B1) ^ atom) & 4095`. A site miss probes the table before `Heap::lookup`. The same epoch counter (459 bumps at the first row) invalidates it. Complexity O(1). This is the V8 stub cache.

**Expected.** Refills fall from 2.25 M to under 0.9 M (hypothesis). Saving 0.3 to 0.6 G (1.7 to 3.5%) at the first row; 3 to 5 G at settle (hypothesis: the same share, 3 to 5%).

**47 journeys.** All JS journeys, 1 to 3% each. The journeys with few property reads gain nothing.

**Cost matrix risk.** Low. A wrong hint is caught by the key compare. The stub cache is the risky part (stale slot after a shape change). The epoch protects it, as it protects the site entries now.

**Size.** S: about 60 lines for step 2, 120 more for step 3.

**Result of step 1 (2026-11-15, built and measured; `progress.md`, "Gmail P2").** A probe build counted every member read that left the inline hit, by cause (main `ca67571d`, Gmail first row, 2 runs agree to 0.3%).

| Cause | First row | Share of the misses |
| --- | --- | --- |
| Slow reads (all that leave the inline hit) | 3.41 M | |
| Chain hits (a method on the prototype, an absent key) | 0.99 M | not misses |
| Misses on a dictionary receiver | 2.06 M | 91% |
| Misses on a shaped receiver | 0.22 M | 9% |

The gate is met: dictionary receivers are most of the refills. But the cause is not the one that this section guessed. 99.4% of the dictionary misses read a key at position 256 or higher, and 99% read the same object as the last read at that site. The entry held the position in a `u8`, so a dictionary of 256 or more keys never got an entry, and every read took `get_dict` and a hash lookup. Hypothesis (not checked): the object is a namespace object of a few thousand keys (Gmail code reads helpers as `_.name`). The position hint for another object of the same layout would hit 4.4 K reads (0.2%): not built.

Of the 0.22 M shaped misses, 0.14 M are a site that has seen a second shape (polymorphic). A simulated 4-shape entry per site or a 4,096-entry stub table would hit 0.13 M of them (about 45 M instructions, 0.3% of the first row). That does not pay for the memory per site. Neither is built.

**Other engines (from the authors' public docs, not re-read for this change: to verify).** V8 keeps a site mono (1 map), poly (up to 4 maps) or mega (a global stub cache keyed by map and name). A dictionary-mode receiver has no stable map, so V8 does the name-dictionary probe in the handler: it caches nothing about the position. SpiderMonkey's CacheIR guards the shape. A dictionary-mode object has its own shape, so a stub for it holds only while the same object is read, as the bare entry does. So the bare entry (the object and the position, checked by the key) is already the shape of the answer in both; what bare lacked was the range of the position.

**Built.** `Ic::depth` is a `u16` (it shares the 4 bytes of `kind`, `depth` and `misses`, so `Ic` stays 24 bytes). An `IC_DICT` entry holds a position up to 65,535. A read of a dictionary of 256 or more keys now hits inline (`ic_dict_own`: the same object, the same key at that position).

### P3. A call path with a smaller frame and no `select`

**Problem.** A call costs 653 or 839 instructions before the body. 2.06 G (10.5%) at the first row. A one-step function costs 702 per call.

**Changes (each removes work).**
1. **No hash lookup.** `call_fn_inner` does `funcs.get(&o)` on an `FxMap`. Store the callee index in the function object (`ObjId` to a `Vec<FnInfo>` index, set in `make_closure`). Cost 168 to about 60.
2. **One pointer instead of 11 fields.** `select` copies 11 per-script fields into `Interp`. Put them in one `ScriptCtx` and swap one `&ScriptCtx`. Skip the swap when the callee has the same script as the caller (most calls in one big script: 84% of steps are in one script). Cost 127 to about 15 for a same-script call.
3. **Arguments straight into the slots.** `eval_args` fills an `arg_pool` vector, then `call_frame` copies it into the slots. Evaluate each argument straight into the frame slot of the callee when the callee is a known frame function. Setup 221 to about 130 (hypothesis).
4. **One stack check.** `in_frames` and `low_stack` are two checks per call. A single depth counter, with the low-stack probe every 64 calls, removes about 25.
5. **Env path.** Set up cost 358 is `new_env` + `bind_call`. Route more functions to the frame path (widen `fr_gate`), which costs 221. Check what `fr_gate` refuses on Gmail (not measured: 820 K calls, 28% of calls, take the env path).

**Expected.** Frame path 653 to about 330 (hypothesis); env path 839 to about 520 or to the frame path. Saving 2.104 M x 320 + 0.821 M x 320 = 0.94 G (5.4%) without changing `fr_gate`; up to 1.1 G with it. At settle about 5 G (hypothesis: calls are 11.7% there).

**47 journeys.** All JS journeys. Call-heavy ones gain 5 to 8%. Pages with little JS gain nothing.

**Cost matrix risk.** Medium. The call path touches `arguments`, `this`, generators, async functions, classes, `super`, proxies, stack-depth errors and the stack trace. `eval_stack_tests`, `eval_recursion_tests` and `eval_depth_tests` cover the edge cases. Run all of the cost matrix.

**Size.** M: about 400 lines changed in `eval.rs`, `eval_scope.rs`, `eval_scripts.rs`.

### P4. Three small removals

| Item | Evidence | Change | Gain (hypothesis) |
| --- | --- | --- | --- |
| `ErrorCtor` | 29 calls = 111.5 M (3.8 M instructions per call); at settle 333 calls = 309 M | Find why one error costs 3.8 M instructions (hypothesis: the stack capture scans source for line and column). Capture the position without a source scan. | -0.11 G first row (0.6%); -0.3 G settle |
| `Paren` nodes | 1.13 M `Paren` evaluations (about 2% of the steps) | The parser returns the inner expression. It keeps a flag only where `(a, b)` and `(a.b)()` need it. | up to -0.1 to -0.2 G |
| `str::contains` in bindings | `is_contained_in` 2.5% after the first row (about 2.2 G at settle) | Out of scope for JS. Report to the owner of the style path. | none in this design |

**Cost matrix risk.** Low for `ErrorCtor` (stack text must stay equal: test). Medium for `Paren` (call `this` binding of `(a.b)()`).

**Size.** XS each.

**Result (2026-11-15, built and measured; `progress.md`, "Gmail P4").**

*`ErrorCtor`.* The hypothesis was right in kind and wrong in detail. An error does not scan the source for each frame. The first line and column lookup of a script builds the location marks of the whole script (`lex::location_marks`, one pass over every byte at 19 instructions per byte). The three big Gmail scripts are 4.9 MB, so the first error in each of them costs 25 to 40 M. Later errors cost 54 K each in a 1.5 MB script (a scan of at most 4 KiB) and 20 K in a small one. The 3.8 M per call is the mean of a few calls of the first kind and many of the second. The scan now reads blocks of 32 bytes with no CR and no E2 byte in one vector pass: 2 instructions per byte, 9.6 times fewer. Gmail first row: 23 to 28 calls cost 109.5 M before and 14.0 M after (-87%). The stack text is the same: the old and the new scan are compared with a character reference at every offset of random texts.

Not built: a lazy `stack` (format the text on the first read, as V8 does). After this change the work of an error is 20 K instructions in a small script, and the marks are 14 M for the whole first row (0.09%). The lazy design needs a side table, a property hook and GC pruning for that. Also not built: marks made only up to the offset that is asked. It would save about half of the 14 M. The step charge of a first lookup (one step for 32 bytes of the script) was set for the old scan and is higher than the work now; it is a limit, and it is left as it is.

*`Paren`.* The node is gone (`ExprKind::Paren` is removed from the tree). The parser keeps a sorted list of the ids of the expressions that had parentheses (4 bytes each, only while parsing, cut back with the arenas when a lazy function body is dropped). Its questions: is a target valid (`(a) = 1`, `[(a.b)] = x` yes; `({a}) = 1`, `[(a = 1)] = x`, `((a)) => 1` no), is the expression an arrow (`(x => x) ? a : b`), a directive (`("use strict")` is not), a label (`(a): 1` is not), `**` after a unary (`(-a) ** 2`), `??` mixed with `&&` or `||`, `for ((async) of x)`, and `export default (function () {})` (an expression, not a hoisted declaration: `StmtKind::ExportDefault` has a flag for it). The rest needed no flag: `(a?.b).c` is a `Chain` as the object of a member, `(a.b)()` keeps its `this` because the callee is the member, `delete (x)` is `delete x`, `(a, b)` is a `Seq`, and a function's `toString` reads its own span. 1.13 M evaluations of a node are gone from the Gmail first row; a loop with 9 parentheses in each pass takes 7.8% fewer instructions (about 64 per parenthesis).

Measured on the Gmail first row (page-thread instructions, probe builds of main `5b0b76b1` and the branch, 7 alternating pairs, load average 1 to 4): median 15.509 G before, 15.344 G after (-0.165 G, -1.1%); the 7 values are 15.153 to 15.675 before and 14.969 to 15.505 after. The error probe gives -95.5 M of that; the rest (-70 M) fits 1.13 M evaluations at 64 instructions (hypothesis: not counted on its own in Gmail). The design hypothesis was -0.1 to -0.4 G.

### P5. Disk cache of parse results

**Problem.** Parse and lazy load cost 2.59 G (13.2% at the first row). A repeat visit parses the same bytes again.

**Data structure.** A file per script under the profile dir, named by a hash of `(source bytes, parser version, limits)`. Contents: the `Program` arenas (`Vec<Expr>`, `Vec<Stmt>`, names) as flat arrays, the lazy function ranges, and a list of functions that were first-called in the last visit (the `lazy_load` set).

**Algorithm.** On script load, hash the source (a fast 64-bit hash: about 1 instruction per byte, 5 M for 5 MB). On a hit, map the file and rebuild `Program` by reading the arrays. On a miss, parse and write the file on a worker thread after the first execution (so it holds the lazy ranges that ran, as V8 does). The first-call list lets a worker parse exactly the lazy functions that will run, before they run (this joins P1).

**Memory layout.** The arenas must be plain data (no `Rc`, no `OnceCell`). Today `ast.rs` holds `Rc<str>`, `Rc<OnceCell<..>>` and `OnceCell<Rc<Program>>`. The work is to make the arenas relocatable or to write a serializer that rebuilds them.

**Expected.** Repeat visit: -2.0 G at the first row (-12%) if load costs 20% of the parse (hypothesis). After P1 the top parse is already off the thread, so the extra gain is the lazy load, about -0.9 G (-5%). First visit: 0, and a small write cost on a worker. Disk: about 2 to 4 MB per MB of source (hypothesis).

**47 journeys.** Journeys run in a fresh browser most of the time. A cache helps only where the profile dir persists. The benefit is for a user who logs in to the same site again.

**Cost matrix risk.** Medium. A stale or damaged file must never run (key by content hash; check a trailer checksum). Disk use needs a size limit and an eviction rule.

**Size.** L. The AST change is the biggest part.

### P6. A tier for hot functions (finish the bytecode lane)

**State.** The lane runs about 5% of Gmail calls. The lazy functions are refused ("no scope") and Gmail is lazy. With `BARE_JS_EXEC=byte` the first row is 18.3 G against 17.7 G in tree mode (1 run each, #565 numbers; this round's byte run reached settle at 108.5 G against 103 to 116 G for the tree). The lane gives nothing on Gmail today.

**Design.**
1. Count calls per function (the `funcs` entry has room for a `u16` counter).
2. At 100 calls compile the function to byte code (one pass, no IR). Hot functions only: 365 functions of 13,375 (2.7%) take 68% of function self.
3. The lane must accept lazy functions (resolve the scope from the lazy slot as `call_script` does) and use the same IC entries and the same frames as the tree walker (the SpiderMonkey Baseline Interpreter model: one data set for both tiers).
4. Calls from the lane to the lane skip `call_fn_inner` (the same saving as P3).

**Expected (hypothesis).** The hot code is 8.0 G of the 19.6 G instrumented thread (41%). Scaled to the plain build it is about 7.1 G of 17.3 G. The lane gives 2.49x on its benchmark (R1). Gmail code is more property and call bound than R1 code, so the factor is lower.

| Factor on hot code | Hot code | Saving at the first row |
| --- | --- | --- |
| 1.5x | 4.7 G | -2.4 G (-14%) |
| 2.0x | 3.5 G | -3.5 G (-20%) |
| 2.49x (R1) | 2.9 G | -4.2 G (-24%) |
| 3.0x | 2.4 G | -4.7 G (-27%) |

Compile cost: 365 functions x about 30 K instructions = 11 M (negligible; the 30 K is a hypothesis). Memory: byte code of 365 functions, about 1 to 2 MB (hypothesis).

**47 journeys.** Every journey with a loop or hot function gains. The gain is large only when code runs 100 times or more.

**Cost matrix risk.** High. A second engine path must give the same result for the whole language, errors, stack traces, generators and async. The lane has its own tests. The owner stopped it at R1 on 2026-11-08 because of this cost.

**Size.** L to XL.

## 6. Ranking

Rank by gain for work, and prefer removing work.

| Rank | Design | Gain, first row | Gain, repeat visit | Settle | Risk | Work | Why this rank |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | P1 parse on arrival | page thread -1.4 G (-8%); about -0.3 s | same | -1.5 to -3.6 G page thread | low to medium | S to M | Moves work into idle time. No semantic change. |
| 2 | P4 small removals | -0.1 to -0.4 G | same | -0.3 G+ | low | XS | Removes work; almost free. |
| 3 | P2 dictionary hint, stub cache | -0.3 to -0.6 G (2 to 3.5%) | same | 3 to 5 G | low | S | Removes a repeated lookup. Measure first. |
| 4 | P3 call path | -0.8 to -1.1 G (5 to 6%) | same | about 5 G | medium | M | Biggest single JS cost that is not dispatch. |
| 5 | P5 disk cache | 0 | -2.0 G (-12%); -0.9 G after P1 | -2 to -4 G | medium | L | Repeat visits only. Needs a relocatable AST. |
| 6 | P6 hot tier | -3.5 to -4.2 G (-20 to -24%) | same | -25 to -30 G (hypothesis) | high | L to XL | Largest gain. The only one that cuts dispatch. High risk and work. |

Order of build: P1 and P4 now, P2 step 1 beside them, P3 next, P6 decision after P3 (P3 is a part of P6: both need a cheap call).

## 7. Effect together

| Stack | Page thread, first row | Time to first row (hypothesis: 1 G = 0.22 s of page thread; P1 is limited by the 0.46 s window) |
| --- | --- | --- |
| now | 17.3 G | about 4.15 s with P of #569 |
| P1 + P4 + P2 + P3 | about 14.2 G (-18%) | about 3.5 s (-0.3 s from P1, -0.35 s from 1.65 G of instructions) |
| and P6 at 2.0x | about 11 G (-36%) | about 2.8 s |
| and P5 (repeat visit, after P1) | about 10 G | about 2.6 s |
| Chrome (cold, #565) | | 1.2 to 1.5 s |

## 8. Honest bytecode and baseline number

- Without P6 the first row of Gmail stays near 3.5 s in bare (hypothesis). About 14 G remain on the page thread. The largest parts are the tree-walk dispatch (30% of 17.3 G today), property reads and allocation.
- With P6 at the R1 factor (2.49x on hot code) the page thread falls to about 10 G and the first row to about 2.6 s. At 2.0x: about 11 G and 2.8 s.
- Chrome gets to 1.2 to 1.5 s with Ignition, Sparkplug and TurboFan and a code cache. In the settle run bare needs 104 G against 12 to 19.6 G for Chrome. That is 5 to 8x. A tier of the byte-code kind gives 20 to 24% on the first row at most. **Only a baseline JIT (a Sparkplug-like template compiler with machine-code ICs) can give 3x or more on the interpreter share.** That design is not portable (the core must stay OS-independent) and this doc does not propose it.
- Gmail's 17 G is also a cost of what the page does: 3.06 M calls and 50 M steps for 50 rows. Chrome runs the same steps faster, not in fewer steps.

## 9. Not measured

- Wall time of any change (nothing was built; the times are instruction counts divided by an assumed 4 to 5 G per second).
- ~~The share of dictionary receivers in the refills~~ Counted in P2 step 1: 91% of the misses.
- Arguments objects, scope-depth lookups, string operations other than the natives, closure memory.
- Why `fr_gate` sends 28% of the calls to the env path.
- The byte lane with lazy functions (it refuses them; the gain in section 5, P6, is a model).
- Memory (peak RSS) for any design.
- Another account, a larger inbox, a slow network, a second run with a warm cache.
- The instrumented counts carry 13% overhead at the first row; settle is inflated more (142 G read against 103 G plain). Shares are reliable; absolute sizes are low by that factor.
- The 47 journeys and the cost matrix were not run (docs only).

## 10. Notes

- `/data/bare-tmp/bin/` (`heavy`, `base-build`) was gone during the work. A private copy of `heavy` with the same lock directory was used from `/data/bare-tmp/r2/gmail-js/bin/`.
- Raw data: `/data/bare-tmp/r2/gmail-js/raw/` (not in the repo: it holds session data). Counter patch and scripts: `/data/bare-tmp/r2/gmail-js/` (`mkcnt.py`, `run.py`, `an-stat.py`, `an-rec.py`).

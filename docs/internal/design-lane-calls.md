# Design: calls on the bytecode lane

Date: 2026-11-17. Base: `daa72fc4` (branch `j1-lazy-bytecode`, 99.7% of calls on the lane). Status: design for review. No engine change is part of this commit.
Input: `docs/research-lane-op-histogram.md` (call setup is 16.4% of the steady page; `Interp::select` alone is 2.0%).
Repo rule (`CLAUDE.md`): do not patch hot spots. Find the design that removes a kind of work, and compare at least two designs.

## Summary

- 90.6% of all `CallR` ops on the page do not run inline. They leave the loop and go through `bc_call`, `call_fn`, `call_fn_inner`, `call_script`, `select`, `bc_enter`, `bc_run_rf`, and a new `bc_loop`.
- 66.4% of all `CallR` ops (73% of the ones that leave the loop) call a lane function of **another script slot**. Lazy function bodies each own a script slot. `CallR` runs inline only when callee and caller share a slot (`bc_target`: `script != self.cur`).
- Such a call costs 1,079 instructions. A same-script `CallR` costs 471. (Micro, section 1.4.)
- A throwaway experiment (cross-script `CallR`, no `select`) cut the page by 7.3% at 50 keys (8458.30M to 7839.77M) and 3.7% at load. All 1006 unit tests passed. It also made the narrow rows 0.6% to 3.9% worse (loop layout), so a real build must keep the loop arm small.
- Recommended: make the lane script-neutral (design A), then a direct native-call arm (design C). A call-site cache (design B) saves little (hypothesis, about 1%). Environments are already lazy; they are not a lever.

## 1. Measured profile

### 1.1 Method

- Machine: AMD Ryzen 9 5900HX, Linux. Other agents ran at the same time. Only instruction counts are used.
- Symbol build: `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false`, own `CARGO_TARGET_DIR`. Same code as the normal build.
- Callgrind on the page (`type_load /data/bare-tmp/jit-page '#a1' N`, `BARE_JS_EXEC=auto`) at 0 keys (476.8M) and 5 keys (1,319.7M). Steady = 5 keys minus 0 keys = 842.9M. Also `eval_load calls 1` (166.5M) and `closures 1` (51.2M), `BARE_JS_EXEC=byte`.
- Self cost per function and per source line is parsed from the callgrind files (recursion levels merged). Call counts come from the call edges.
- Call classes (section 1.2) come from a throwaway counter patch in a scratch worktree (not committed; patch kept outside the repo). Counts are exact, run by run.
- Narrow rows and page totals: `ic.sh`, min of 3, spread below 0.05%.

### 1.2 Count of calls (page, `Auto`)

`CallR` is the only call op the generator emits (`CALL` and `NEW` appear only for `new`: 2,166 at 50 keys). Counts of executed `CallR` ops, at the loop:

| Class | 5 keys | 50 keys | Share of `CallR` at 50 keys |
|---|---|---|---|
| Inline in the loop (same script, register function) | 25,969 | 198,998 | 9.4% |
| Left the loop (`bc_target` is `None`) | 246,099 | 1,913,276 | 90.6% |
| of which: callee is a native | 64,477 | 488,357 | 23.1% |
| of which: bound, host | 4,126 | 18,886 | 0.9% |
| of which: first call of a lazy function (`lazy_resolve`) | 256 | 616 | 0.03% |
| of which: lane function, other script, register function, no environment | 167,340 | 1,327,544 | 62.9% |
| of which: lane function, other script, register function with environment | 3,952 | 31,582 | 1.5% |
| of which: lane function, other script, not a register function | 5,240 | 43,288 | 2.0% |
| of which: callee on the tree walker (refused, not yet compiled) | about 700 | about 3,000 | 0.1% |

- Lane functions that were entered through `call_script`: 1,514,029 (`runs` in `bcstats`) at 50 keys. That is 88.4% of `calls_lane` (1,713,027). The others are the inline calls. The difference between 1,514,029 and the cross-script count (1,402,414) is callbacks from natives (`map`, jobs, event handlers) and `new`.
- Every `call_script` call selects twice. `select` is called 388,110 times at 5 keys. `call_script` is called 193,699 times. So every call of a lane function by `call_script` crosses a script boundary. Hypothesis for the reason (checked in `parse.rs`): a function body of 128 bytes or more, in a source of 16 KiB or more, is parsed lazily. The first call parses it into a tree of its own and a script slot of its own (`lazy_load`). The function object then points at that slot (`lazy_resolve`). The large hot functions of the bundle are all lazy. So a call from one large function to another large function always crosses slots. Same-slot calls remain only for small inner functions that the parser kept.
- Environments: the calls that create an environment are has-environment register functions and non-register functions: 32,913 + 50,832 entries through `bc_enter`, and 8,219 + 5,747 inline `CallR`. That is about 97,700 of 1,713,027 calls (5.7%). Stage 2 `rf` already removed the environment for the other 94.3%.

### 1.3 Cost of the call path (page, steady, 5 keys minus 0 keys)

Self instructions, call counts and instructions per call. Natives share `bc_call` and `call_fn_inner`, so those two rows are averages over all callees (hypothesis for the split).

| Step | Self instr (steady) | Calls | Per call | Share of steady 842.9M |
|---|---|---|---|---|
| `CallR` arm in `bc_loop` (lines 1177-1247, outside the inline path) | 8.56M | | | 1.02% |
| `bc_call` (arg vector from `arg_pool`, `call_fn` wrapper, `mark_call`, `stk.pos`) | 38.35M | 186,328 | 205.8 | 4.55% |
| `call_fn_inner` (`funcs.get`, depth, `tick`, `low_stack`, `in_frames`) | 21.30M | 218,480 | 97.5 | 2.53% |
| `call_script` (`func_ref`, lazy check, mode, `saved_*`, `select`) | 13.78M | 148,288 | 92.9 | 1.63% |
| `Interp::select` (two per call) | 20.42M | 296,664 | 68.8 | 2.42% |
| `bc_enter` (`bc_code`, `Rc` clone, `in_frame`, restore) | 17.87M | 148,288 | 120.5 | 2.12% |
| `bc_run_rf` (`RegStack` from the pool, `resize`, copy args, push back) | 18.84M | 142,864 | 131.8 | 2.23% |
| `bc_loop` entry (prologue) and `ret!` | 3.33M | | | 0.40% |
| Total call path | **142.4M** | | | **16.9%** |
| `new_env` | 0.43M | 12,250 | 35.4 | 0.05% |
| `bind_call` | 3.13M | 9,810 | 318.6 | 0.37% |
| `mark_call` | 0.05M | 3,746 | 14.0 | 0.01% |

- The sum (16.9%) matches the 16.4% of the research note. The difference is the grouping.
- Per lane call through `call_script`, the sum of the rows above is about 867 instructions (hypothesis for the native share inside `bc_call` and `call_fn_inner`). With 148,288 such calls that is about 129M, 15.3% of the steady page. The native calls (about 47,000 in the steady window) make up the rest.
- `select` costs 68.8 instructions per swap. The swap moves `prog` (an `Rc`), `cc` (`Caches`: three `Vec`s and a `Box`), and exchanges two `Code` slots in `scripts`.
- Environment, `bind_call`, `new_env`, `declare` and `release`: 0.4% of the steady page together. This is not a lever.
- Not separated: `tick`, `release` and the depth check (inlined into the rows above). `Error.stack` bookkeeping is cheap: `CallR` stores the call position in the `StackFrame` (`x & 0x7fff_ffff`), and `mark_call` runs only on the slow path.

### 1.4 What a call costs in a clean micro

Source: a loop of 200,000 calls of `f(i, s)` where `f` returns its first argument, against the same loop without the call (`s = i`). `BARE_JS_EXEC=byte`. Counts are instructions per call, loop excluded.

| Case | Instructions per call |
|---|---|
| Same script, inline `CallR` (base) | 471 |
| Other script (both functions lazy), through `call_script` (base) | 1,079 |
| Other script, experiment (design A, no `select`) | 484 |
| Same script, experiment | 480 |

Split of the 471 of the same-script call (callgrind lines, base): dispatch and decode of 5 ops 109, `CallR` arm 86, `ret!` and `Ret` arm 34, `GetGlobal` of `f` 44, `bc_target` 20 (+13 in `obj_tab`, `rc`, `option`), inlined std code (`Vec` push and pop of `Fr`, capacity checks, moves of `Fr`, slice bounds) 123, other 42. So the inline call machinery itself is about 280 instructions. `Fr` has eight fields (hypothesis: 56 to 64 bytes per push). Real engines spend 30 to 60 here (hypothesis, from the code in section 3, not measured).

## 2. The current design and its costs

### 2.1 Data structures

- `Interp.prog: Rc<Program>` and `Interp.cc: Caches` hold the code and the caches of the **current** script. `scripts: Vec<Code>` holds all scripts; the slot of the current script holds a placeholder. `select(id)` swaps `prog`, `cc`, and the two `Code` slots (`eval_scripts.rs:442`). The swap exists so that the tree walker reads `self.prog` and `self.cc` as plain fields (no index).
- `funcs: ObjId -> Callee` (hash map). `Callee::Script { script, fid, env, self_binding }`. `lazy_resolve` rewrites `(script, fid)` of the function object to the lazy tree's slot on the first call.
- `bc.codes[script][fid]: BcSlot` (`New`, `Seen`, `Tree`, `Byte(Rc<Compiled>)`). `Compiled { code, kv, cells }` holds the bytecode. It has no field that names its script.
- `RegStack { regs, frames }` from `bc.pool`. A frame is `Fr { code: Rc<Compiled>, pc, base, dst, env, this, depth, callee_env }`.

### 2.2 Algorithm of one call (`CallR`)

1. `CallR` arm: set `this`, call `bc_target(regs[b])`: `funcs.get`, compare `script` with `self.cur`, index `bc.codes[cur][fid]`, clone the `Rc`.
2. Same script and a register function: check depth, `tick`, bind `this`, push `StackFrame` and `Fr`, grow `st.regs`, fill locals, switch `code`, `ops`, `regs`. About 280 instructions with the return (section 1.4).
3. Otherwise: `bc_call`. It pops `arg_pool`, copies the arguments, runs `mark_call` (reads `prog.expr`), then `call_fn`, which calls `call_fn_inner`: depth check, `tick`, `funcs.get` again, `low_stack`, `in_frames`, then `call_script`. That checks the lazy body and the function mode, saves `env`, `strict` and `cur`, runs `select(script)` and `bc_enter`. `bc_enter` runs `bc_code` (index, `Rc` clone), `in_frame`, then `bc_run_rf`: pool pop, `resize`, copy args. Then a new `bc_loop` (a Rust call with a prologue). On return the same steps run backward: `select(saved)`, restore `env` and `strict`, and `arg_pool` push.

Cost of step 3 per call: about 870 instructions. Step 2: about 280 (section 1.3 and 1.4). Step 3 repeats the `funcs.get` of step 1 and crosses two `select`s.

## 3. What real engines do (sources read in this session)

Files were fetched from the public repositories into a scratch directory and read. Line numbers are those of the files on 2026-11-17 (`main` or `master` at that time).

- **QuickJS** (`quickjs.c`, `JS_CallInternal`, line 18239). The callee code is one load from the function object: `b = p->u.func.function_bytecode` (18319). The realm is in the bytecode object: `ctx = b->realm` (18364). There is no state swap. The frame `JSStackFrame` (line 412, 9 fields) is a C local; the registers come from `alloca` (18339); `arg_buf = argv` uses the caller's argument slots without a copy unless fewer arguments than parameters were given (18321-18350). A call is a C recursion (`OP_call_method` at 18713 calls `JS_CallInternal`). Cost model: cheap frame, no per-call heap work, but one C frame per call.
- **Hermes** (`lib/VM/Interpreter.cpp`, `doCall` at 1576). The arguments sit in the register stack at the top; `StackFramePtr::initFrame(runtime.stackPointer_, ...)` (1589) writes the frame header in place. A JS callee does not recurse in C: `curCodeBlock = calleeBlock; goto tailCall` (1613-1615). `func->getCodeBlock(runtime)` and `calleeBlock->lazyCompile(runtime)` (1608-1609) find the code. `CallDirect` (1640-1660) takes the callee code block from the module by index and skips the function object. Cost model: the loop never leaves for a JS-to-JS call, and the code block pointer is the only state that changes.
- **JSC LLInt** (`LowLevelInterpreter64.asm`, `callHelper`, 2515-2575). Each call op has metadata with a `CallLinkInfo`. The op compares the callee object with `m_callee` (`bqneq t0, t1, .opCallSlow`, 2544) and jumps to `m_monomorphicCallDestination` (2551). The frame is written in the caller's register area (2520-2535). A miss goes to a slow thunk (`virtualThunkFor`, `LowLevelInterpreter.asm:2724`), which loads the code from `FunctionExecutable` (`m_codeBlockForCall`). Cost model: monomorphic cache keyed by callee object, a handful of loads, no hash lookup.
- **SpiderMonkey** (`js/src/vm/Interpreter.cpp`, `CASE(Call)` at 3226). A script function takes `fun->nonLazyScript()` (3355; a lazy script is made first by `JSFunction::getOrCreateScript`, 3300). `activation.pushInlineFrame(args, funScript, construct)` (3357) pushes the frame, and `SET_SCRIPT(REGS.fp()->script())` (3365) changes one local pointer. A realm guard switches the realm (3361 comment). The loop does not leave. Cost model: the script is a pointer in a local variable. No data of the old script is moved.
- **V8 Ignition** (`src/builtins/x64/builtins-x64.cc`). `Generate_CallFunction` (2728) loads `shared_function_info_` and `context_` from the function (`rsi = function->context_`, "Enter the context of the function"). `Generate_InterpreterEntryTrampoline` (1147) loads the bytecode array from the shared function info, calls `CompileLazy` when it was flushed (1162-1167, 1395), loads the feedback cell and vector, and pushes the frame (`rbp`, context, closure). Frames sit on the machine stack. Cost model: the code, the context and the feedback sit behind the function object, so nothing is swapped.

What they share, and what `bare` lacks:
1. The callee's code and tables are reached **through a pointer from the callee**. No engine copies or swaps the tables of the whole script at a call. `bare` swaps them because the tree walker reads fields of `self`.
2. A lazy function costs a check on the code pointer (V8 `CompileLazy`, Hermes `lazyCompile`, SpiderMonkey `getOrCreateScript`). It does not change where the loop runs.
3. JSC caches the callee per call site and skips the lookup. QuickJS, Hermes and V8 do not cache by call site in the interpreter; their lookup is one or two loads from the object. `bare` needs a hash lookup (`funcs.get`) at 20 to 33 instructions.

## 4. Options

### Design A: script-neutral lane (no state swap)

Idea: the lane does not need `self.prog` and `self.cc` for its hot ops. Give each `Compiled` the index of its script. Let `CallR` run any register function of any script inline, and select a script only when a slow op needs it.

Data:
- `Compiled.script: u32` (set in `bc_finish`; it is the current script at that time).
- `bc_target` returns the script of the callee. It checks `bc.codes[script][fid]` (the callee's slot, not `cur`).
- `StackFrame.script` of the callee is its own script (the experiment did this).
- A function `sync(script)`: `if self.cur != script { self.select(script) }`. The lane calls it before every op that reads `prog`, `cc` or `cur`. It does not restore: `bc_enter` and `call_script` already restore the script that was current at their entry.

Algorithm of a call: `CallR` as in section 2.2 step 2, with `bc_target` changed. The cross-script call has the same cost as the same-script call.

What the lane reads from `prog`, `cc` or `cur` today (from `bc_vm.rs` and what the experiment crashed on):
- `bc_call` (`mark_call` reads `prog.expr`; the callee expression is read for the error text). `CallR` already carries the position in `x`, so the fast path needs no `prog`. A not-callable error needs the script: `sync` there.
- `bc_closure` and `make_closure` (`prog.func`, `cur` for the new function object's script, `lazy_slot(cur, ..)`). `sync`.
- `bc_rare` (regexp, template, spread helpers), `bc_declare` (scope tables). `sync`.
- `bc_catch` and error creation (the stack capture reads `cur` or the frame scripts): hypothesis, not audited. A test with the poison script (below) must show it.
- `bc_enter` for a non-register callee and a has-environment callee: `prog.func`, `cc.cold.fflags`, `bind_call`. Keep these on the slow path in step 1.

How to find every reader (the main risk): a test mode where `select` parks the script and leaves `prog` and `cc` as an **empty placeholder** while the lane runs a register function. Any read of `prog` or `cc` in that mode panics (index out of range). The 1006 unit tests, `exec_diff.sh` and the page then find the readers. Each reader gets a `sync`. This is the same placeholder trick that `select` already uses for the slot of the current script.

Complexity: a call is O(1) with no hash lookup beyond `funcs.get`. Memory: 4 bytes per `Compiled`.
Invalidation: none. `bc.codes` slots are created once and never replaced by another `Byte` slot.
Instructions removed per call (measured): micro 1,079 to 484 (595 per call, 55%). Page steady: 59.9M of 842.9M (7.1%), 50 keys 618M (7.3%), load 17.5M (3.7%).
The steady result is below the sum of the removed rows (about 101M) because the inline arm costs more than the micro suggests: 130,000 more inline calls cost about 44M in `bc_loop` (about 340 each with std code) and `sync` added 55,000 `select` pairs (7.0M).

Risks:
- GC roots: no new root. `Fr.code` keeps the `Rc`. Registers are not roots (a collection does not run during a call), as before.
- `Error.stack`: the frame of the callee must carry the callee's script (done in the experiment). The stack text reads each frame's own script.
- Realms: `Compiled.script` is an index in the `Interp` that owns it. `bc.codes` is per `Interp`. No sharing.
- Limits: depth and `tick` stay in the arm (they are in the arm today).
- Lazy first call: the function object still points to the outer slot until the first call. `bc_target` sees a non-`Byte` slot and takes the slow path, which resolves it. Unchanged.
- The loop is layout-sensitive. The experiment made the narrow rows worse: loopfn +3.9% (827.04M to 859.04M), fib +2.1% (56.66M to 57.87M), calls +2.3% (1644.26M to 1681.27M), loop +3.1% (2489.03M to 2565.03M), closures +0.6% (479.57M to 482.37M). Cause (hypothesis): the arm and the new `sync` calls changed the code layout of `bc_loop`. A real build must keep the cross-script work out of the hot arm (a `#[cold]` or `#[inline(never)]` function for the rare cases, one `bc_target` for both cases) and then measure again.
- Test coverage: the experiment passed all 1006 unit tests with `sync` only where the page crashed. That is not proof. The poison mode is the proof.

### Design B: call-site cache

Idea (JSC `CallLinkInfo`): per call site, remember the last callee object and what the call needs, so a repeat call skips `funcs.get`, the slot lookups and the `Rc` lookup.

Data: a side table `Box<[CallIc]>` in `Compiled`, one entry per `CallR`: `{ fobj: ObjId, script: u32, fid: FuncId, env: u32, code: *const Compiled }`. A hit is `regs[b] == fobj`.
Hit rate (measured, page, 50 keys, 1,527,642 inline calls under design A): same object as the last call at that site 98.7%; same function but a different object 0.5%; other 0.4%; 786 sites. So the cache is monomorphic in practice.
Invalidation: the cache holds an `ObjId`. After a collection an id can name a new object. Clear all `CallIc` in `collect` (or keep a generation number). `lazy_resolve` rewrites `funcs`; a site that cached before the first call cannot exist, because the cache fills only after a `Byte` slot is seen.
Instructions removed per call (counted from section 1.4): `bc_target` is about 33 of the 471 (20 in `bc_vm.rs` lines 408-418 plus `obj_tab`, `rc`, `option`). The hit path still needs the compare, the load of the entry and the `Rc` clone. Gain about 20 to 30 per call, so about 4M to 5M instructions in the steady page (0.5% to 0.6%, hypothesis).
Risks: an extra 24 to 32 bytes per call site (786 sites on the page: 25 KB, small). More code in the hot arm (layout). Needs the clear in `collect`.
Verdict: small gain on this page. Do it only after A and only if the gate shows more. Real engines with a call cache (JSC) also use it to link JIT code, which `bare` does not have yet.

### Design C: direct native call arm

Idea: 23.1% of `CallR` ops call a native (488,357 at 50 keys). They pay `bc_call` (arg vector, `call_fn`), `call_fn_inner` (a second `funcs.get`, `in_frames`, `low_stack`, depth, `tick`) and then `call_native`. Add a branch in the `CallR` arm: if `funcs.get` gives `Callee::Native(n)`, call `call_native(n, this, &regs[first..first+n], false)` with the register slice as the argument slice. The slice is not copied. This is what QuickJS does (`arg_buf = argv`).
Why the copy exists today: `bc_call` takes `regs: &[Value]` and fills `argv` from `arg_pool` before it calls `call_fn`. Hypothesis: it protects the arguments if the callee grows `st.regs`. A native that re-enters the lane gets its own `RegStack` from the pool, so `st.regs` of the caller is not touched. Verify with a test before use.
Instructions removed per native call (hypothesis from the table in 1.3): `bc_call` 206 + `call_fn_inner` 97 minus the new arm (about 60): about 240. Steady page: about 47,000 native calls, so about 11M (1.3% of 842.9M, hypothesis). The depth and `tick` checks stay (limits). `Error.stack` needs the position: `x` has it. `frames` must be reset like `in_frames` does (the native may call back into script code through `call_fn`, which does its own `in_frames`).
Risks: a native that calls `call_fn` sees `self.frames` of the caller (the `in_frames` reset skipped). `call_fn_inner` skips it for natives already ("A native callee needs no stack check or frame reset"), so the same rule holds.

### Design D: environment-free frames (item c of the task)

Already done by stage 2 `rf`: 94.3% of calls have no environment (section 1.2). The 5.7% that create one cost `new_env` 0.43M, `bind_call` 3.13M: 0.4% of the steady page. No design work needed. A further cut would make `has_env` functions allocate the environment only when an inner closure is created. That is a compiler change with a gain below 0.4% (hypothesis: the upper bound is the total cost of those rows).

### Design E: cheaper `select` (data layout)

Make `Caches` a `Box` inside `Code`. A swap then moves pointers (hypothesis: 68.8 to about 25 per swap). It touches every `self.cc.x` read (one more load) and so the tree walker too. After design A the remaining `select` calls are 110,000 in the steady window (7.0M, 0.8%). The gain is at most about 0.5% and the cost is a risk for the tree walker. Not recommended unless A leaves `select` above 1%.

### Design F: inline small leaf functions

Listed only. It needs inlining in the code generator: a callee body in the caller's registers, with `this`, `arguments`, `Error.stack` frames and the depth limit kept right. Hypothesis: it removes the whole 280 to 480 instructions of a call for leaves, but it breaks `Error.stack` and recursion limits unless frames are kept virtual. Too invasive for now. Revisit after a narrow JIT.

### Comparison

Gains are the share of the steady page (842.9M at 5 keys) unless noted. "Measured" means the experiment of section 5.

| Design | Removes | Per call | Steady page | Load | Risk | Basis |
|---|---|---|---|---|---|---|
| A: script-neutral lane | the whole slow path for cross-script register calls (66% of `CallR`) | 595 | 7.1% (50 keys 7.3%) | 3.7% | readers of `prog`, `cc`, `cur`; loop layout | measured (experiment) |
| B: call-site cache | `funcs.get`, slot lookups | 20 to 30 | 0.5% to 0.6% | below 0.5% | GC clear, layout | hypothesis (hit rate measured) |
| C: native call arm | `bc_call`, `call_fn_inner`, the arg copy (23% of `CallR`) | about 240 | about 1.3% | hypothesis | frame reset, arg aliasing | hypothesis |
| D: lazy environments | `new_env`, `bind_call` for 5.7% of calls | about 350 for those (`new_env` + `bind_call`) | below 0.4% | below 0.4% | compiler change | counted upper bound |
| E: cheap `select` | 40 per swap | 40 to 80 | below 0.5% | | tree walker loads | hypothesis |
| F: inline leaves | the whole call | 280 to 480 | unknown | | `Error.stack`, limits | not measured |

## 5. Experiment (throwaway, not committed)

A scratch copy of `daa72fc4` (patch kept outside the repo: `/data/bare-tmp/call-child/experiment-cross-script-callr.patch`, 159 lines). It does this:
- `Compiled.script` field.
- `bc_target` accepts a callee in another script when it is a register function without environment. It returns the callee's script, and the `StackFrame` uses it.
- `bc_call` gets the call position from `x` (no `mark_call`) and a callee that is not callable syncs the script for the error text.
- `sync(c.script)` before `Closure`, `ClosureName`, `bc_rare`, `bc_declare` and `bc_call` (non-`CallR` form). `bc_catch` and the rest were not synced.

Results (page: `type_load ... '#a1' N`, `Auto`; rows: `byte`, scale 10; min of 3):

| Measure | Base `daa72fc4` | Experiment | Change |
|---|---|---|---|
| Page load (0 keys) | 472.00M | 454.49M | -3.7% |
| Page 50 keys | 8458.30M | 7839.77M | -7.31% |
| Page steady (callgrind, 5 keys minus 0) | 842.9M | 783.0M | -7.1% |
| `bcstats` `calls_lane` (50 keys) | 1,713,027 | 1,713,027 | same |
| Runs of `bc_loop` (50 keys) | 1,514,031 | 186,387 | -87.7% |
| Micro, call to another script | 1,079 per call | 484 per call | -55% |
| `select` calls (steady, 5 keys) | 296,664 | 110,251 | -62.8% |
| loopfn | 827.04M | 859.04M | +3.9% |
| fib | 56.66M | 57.87M | +2.1% |
| calls | 1644.26M | 1681.27M | +2.3% |
| loop | 2489.03M | 2565.03M | +3.1% |
| closures | 479.57M | 482.37M | +0.6% |

The page output was the same: same render counts and the same result value (`type_load` prints them). Unit tests: `cargo test -p bare-js --lib` on the scratch copy, 1006 passed, 0 failed, 1 ignored. The narrow-row loss is the reason for the first rule of step 1 below. The rows moved in a loop that has few calls (loopfn), so it is layout, not call cost.

## 6. Recommended design and step plan

Recommended: **A, then C**. B and E only if the gates ask for them. D and F: no.

Five lines:
1. `Compiled` knows its script. `CallR` runs a register function of any script inline: no `select`, no `bc_call` chain, no nested `bc_loop`.
2. The script is selected only by slow ops that read `prog`, `cc` or `cur` (`sync`). A poison placeholder test mode finds every reader.
3. The cross-script and same-script calls share one arm. The rare cases (has-environment callee, non-register callee, lazy first call, errors) stay in a cold function.
4. A native callee gets a direct arm that passes the register slice (no argument copy, no second lookup).
5. The call-site cache and a leaner `Fr` wait for a profile after steps 1 and 2.

### Step plan with gates

Cost rule for every step: no cost-matrix row more than +0.5% over origin/main `8b6a71bb2d` in `Auto`, under both allocators. Narrow rows (`rows.sh`, `byte`, scale 10): report all five before and after. A row more than +0.5% worse needs a fix or a clear reason. Page: load and 50 keys, `Auto`, `#a1`. Also `bcstats` unchanged in `calls_lane`, and `tools/js/exec_diff.sh target` ends with `differences: 0`.

1. **Step 1: script-neutral `CallR`.**
   - Failing test first: a source of 16 KiB or more with two lazy functions where the first calls the second and the second throws. Check `Error.stack` names both frames with the right script, the result, and `it.bc.stats()` (the inner call runs in the loop of the caller: `runs` stays small).
   - Add the poison mode (a test-only flag: `select` leaves an empty `prog` and `cc` for the running lane code). Run the unit tests and fix each panic with a `sync`.
   - Keep the hot arm as small as today. Put the cross-script checks in the same `bc_target`. Move any added work into `#[cold]` functions.
   - Gate: page 50 keys at most 8,000M (the experiment gave 7,840M); load at most 460M; narrow rows each within +0.5% of 827.04M, 56.66M, 1644.26M, 2489.03M, 479.57M; `select` calls in the steady window below 120,000 at 5 keys; cost matrix as above.
2. **Step 2: native call arm (design C).**
   - Failing test first: a native that calls back into script code and a native that throws, inside a lane function. Check results, `Error.stack` and the depth limit (a deep recursion through `Array.prototype.map` must still stop at `max_call_depth`).
   - Gate: native calls cost at least 150 fewer instructions each on a micro (`arr.push(i)` in a loop, callgrind per call); page steady at least 1% better than step 1; rows within +0.5%.
3. **Step 3 (only if the profile asks): has-environment and non-register callees inline, call-site cache, leaner `Fr`.**
   - Gate: re-profile after step 2. Do a step only if its rows are above 1% of the steady page. Keep the poison mode in CI.

Expected total (hypothesis): steady page -8% to -9% (A 7.1% plus C about 1.3%), load about -4%.

## 7. Risks (summary)

- Hidden readers of `prog`, `cc` and `cur` under a parked script. Controlled by the poison mode. An unaudited path is a wrong stack text or a wrong closure script, not a crash in production, so the test mode matters.
- `bc_loop` layout. The experiment lost up to 3.9% on narrow rows with no change in their work. The real build must keep the arm small and measure all five rows after each edit.
- The 7.1% was measured on one page. Other pages with fewer lazy functions gain less. A page with small scripts (below 16 KiB) has no cross-script calls (all functions are in one slot), so it gains nothing from A.
- C changes the frame reset rule for natives that call back. A test and the depth limit must cover it.

## 8. What was not measured

- The cost matrix, `exec_diff.sh` and clippy on the experiment (it is a sizing run, not a change).
- The full unit-test coverage of the experiment (1006 tests passed, but `sync` was added only where the page crashed).
- The private React dashboard journey and other pages. Page numbers are from the public React 19 bundle.
- Callgrind at 50 keys (it ran at 0 and 5 keys; the 50-key numbers are `perf stat` totals).
- The split of `bc_call` and `call_fn_inner` between script callees and natives (averages over all callees).
- The gain of designs B, C and E (hypothesis, from counted rows). Design D is a counted upper bound.
- Allocator effects (jemalloc or other). Only the default allocator was used.
- The wide-form, generator, async and tree-walker call paths (4,720 calls at 50 keys, 0.3%).
- Whether `bc_enter`'s non-register and has-environment callees (2.0% and 1.5% of `CallR`) could run inline (design step 3).
- Real-engine instruction counts: section 3 describes the code that was read, not a measurement of those engines.

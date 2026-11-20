# Design: dispatch of the js2 interpreter

Status: design, for owner review. No engine code changed. Date: 2026-11-21. Base: main `fd9c749b`. The `crates/js2` code of main `fd9c749b` is the same as the head of #775 (`2c695579`): `git diff 2c695579 fd9c749b -- crates/js2` is empty. #774 and #775 are merged.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "op" is one executed bytecode instruction of js2 (`bytecode.rs`).
- "turn" is one trip around the loop of a micro row. `loopfn` has 7 ops in a turn: `JnLtI`, `MulI`, `Add`, `LoadI.w`, `Mod`, `Inc`, `JmpBack`.
- "hot arm" is an arm of the `match` in `Vm::run` that the hot rows run. "cold arm" is one of the 48 arms that start with `cold_path()` (`docs/progress.md`, "Stable code of the hot loop").
- "rsp op" is an instruction in the hot path that reads or writes a stack slot (`[rsp+N]`).
- "M" is one million user-mode instructions (`instructions:u`).
- "cgu1" is `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`. "Default" is the release profile with 16 codegen units.

## 1. Summary

**Problem.** Each edit of `Vm::run` moves the register allocation of its hot arms. Past edits moved the rows by -6% to +7% with no change in the work (`docs/progress.md`: #770 +4.4%, #785 -6%). The cost of a turn is also high: 257 instructions for 7 ops on main, of which about 40 are rsp ops.

**What I measured** (section 3):

- All the `loopfn` cost is in `Vm::run`: 257 instructions per turn on main (262 on `f3cfa8f8`). One op costs 14 instructions of dispatch (`nop`) and 17 to 49 with its work.
- `pc` and `words` are stack slots in the hot loop, not registers. The hot path has 40 rsp ops per turn (23 reads, 17 writes). In a toy loop with the same ops, encoding and value tags, and only 4 live values, one turn costs 219 instructions (section 7.2). So about 38 instructions per turn, 15%, are the price of the pressure in `Vm::run`.
- #775 moves `fib` by -16.8% and `calls` by -5.2% at the default build. It moves `loopfn` by only -1.8% and `objects` by +1.9%.

**Hypotheses I tested and did not keep:**

- "`pc`, `ci` and `fp` live in memory because `unwind` takes `&mut` to them." I changed `unwind` to take and return values. `pc` stayed in `[rsp+0x30]`. The rows got worse by 0% to +1.5% (spike `s0`). Hypothesis: the pressure of 11 live values is the cause, not the address.
- "Outline the cold arms in the obvious way (`_ =>` arm)." This adds a bounds check to each of the 2 jump tables and the rows got worse by up to +9% (spike `sx`). The fix is to list the cold ops in the pattern (spike `sx2`).

**Recommendation: design (d), a hybrid, built in two steps.**

1. **Step 1: move the 48 cold arms into a second function** (design (a)). Spike `sx2` on main: -1.1% to -3.9% instructions at the default build, -0.6% to -4.1% at cgu1, in all 5 rows except `objects` at cgu1 (-1.5%, no row up). The size of `Vm::run` falls from 255,676 to 158,800 bytes (-38%). Rsp ops fall from 40 to 25 per turn. An edit in a cold arm moved no row (0.00 M at both builds). This also builds the cold path that step 2 needs. Cost: about 2 days (the spike copies the function text; the real change needs the macros to serve two functions).
2. **Step 2: tail-call handlers for the hot ops only**, with the cold function of step 1 as the fallback (design (b) plus (a)). In the toy, instructions per turn fall from 219 (loop and match) to 175 (stable tail calls), 167 (`become`) and 159 (direct threaded). Expected on the real rows: -15% to -23% instructions on `loopfn`, -8% to -18% on the others (hypothesis, section 6.4). Only the owner can choose between stable and nightly Rust (question 1). Step 2 does not start before the owner decides, and a real spike on `loopfn` and `fib` comes first.

I do not recommend direct threaded code (c) now: it saves 16 more instructions per turn than (b) in the toy, and it costs 4x the bytecode memory and a rewrite of the verifier, the root maps and the jump encoding.

**Browser slice.** Not measurable: js2 is not wired into the browser (`BARE_JS_EXEC=new` returns `NOT_READY`). Estimate (hypothesis): step 2 saves 6 to 9 instructions per op. If the browser average is near the 210 instructions per op of the old engine journey (`docs/design-engine-rewrite.md` section 1), the saving is 3% to 4% of the JS instructions. Step 1 saves under 1%.

## 2. How this was measured

- Machine: archMachine, 16 threads. Load average 17 at the start. **A wall-time or `perf stat` cycle, cache-miss or branch-miss measurement is not valid on this machine.** I used instruction counts only (`perf stat -e instructions:u`), `callgrind --dump-instr=yes` and `cachegrind --cache-sim=yes --branch-sim=yes`. The cache and branch figures are from a simulator with a simple predictor. They do not show what a modern CPU does.
- Rows: `interp_load` (`crates/js2/examples`), the method of `tools/js/instr.sh` (net = count at N minus count at N = 0, `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`), 3 runs for each cell. The widest spread of 3 runs is 0.004 M. The tables show the lowest run. N is 1,000,000 for `loopfn` and `calls`, 30 for `fib`, 200,000 for `closures` and `objects`.
- The net count of `loopfn` includes the Rust check loop of `interp_load` (about 15.5 instructions per turn). The callgrind figures in the next section count `Vm::run` alone.
- Trees: `f3cfa8f8` (the base of this task) and `fd9c749b` (main now). Each tree was exported with `git archive` and built with its own `CARGO_TARGET_DIR` under `/data/bare-tmp/dispatch`, with symbols kept (`CARGO_PROFILE_RELEASE_STRIP=false`; the counts equal the stripped build, `docs/progress.md`).
- Rust: stable 1.96.0 for the engine. Nightly 1.102.0 (2026-11-17) for `become` and Miri on the toy.
- Not repeated on another machine, on Windows or on arm64.

## 3. Measured profile

### 3.1 The five rows, main against its parent

Net M instructions, lowest of 3 runs. The percent is against `f3cfa8f8`.

| Row        | `f3cfa8f8` default |      main default | `f3cfa8f8` cgu1 |         main cgu1 |
| ---------- | -----------------: | ----------------: | --------------: | ----------------: |
| `loopfn`   |             277.50 |    272.50 (-1.8%) |          284.50 |    282.50 (-0.7%) |
| `calls`    |             618.50 |    586.50 (-5.2%) |          635.50 |    579.50 (-8.8%) |
| `fib`      |           1,325.73 | 1,103.60 (-16.7%) |        1,344.58 | 1,064.55 (-20.8%) |
| `closures` |             386.49 |    366.89 (-5.1%) |          393.75 |    365.95 (-7.1%) |
| `objects`  |             138.93 |    141.53 (+1.9%) |          141.33 |    143.53 (+1.6%) |

The task text said #775 gives -17% on `loopfn`. The run says the -17% is on `fib`. `loopfn` has no call.

### 3.2 One turn of `loopfn` in `Vm::run` (callgrind, 20,000 turns)

| Measure                                       |         `f3cfa8f8` |         main |    main cgu1 |
| --------------------------------------------- | -----------------: | -----------: | -----------: |
| Instructions in `Vm::run` per turn            |                262 |          257 |          267 |
| Instructions per op (7 ops)                   |               37.4 |         36.7 |         38.1 |
| Rsp ops per turn (reads + writes)             |       40 (23 + 17) | 40 (25 + 15) | 40 (23 + 17) |
| Static rsp ops in the hot path                |                 28 |           22 |           28 |
| Size of `Vm::run` (bytes)                     |            267,258 |      255,676 |      286,606 |
| Address span of the hot instructions (bytes)  | 95,985 (`0x176f1`) |       84,824 |       80,106 |
| 64-byte lines that a turn touches (hot / any) |            30 / 47 |      27 / 45 |          n/a |

The `f3cfa8f8` column agrees with the bisect finding (262 instructions, 28 rsp ops, `0x176f1` bytes). A turn touches 27 code lines (1.7 KB). It fits the L1 instruction cache of any CPU, so instruction-cache misses are not the problem of this row. The simulator shows 3.7 K instruction-cache misses for 20,000 turns. Hypothesis: they are start-up misses, since the hot path is 27 lines.

### 3.3 Cost of one op (`op_cost`, instructions, dispatch included)

| Op           | `f3cfa8f8` | main | spike `sx2` |
| ------------ | ---------: | ---: | ----------: |
| `nop`        |         13 |   14 |          12 |
| `mov`        |         20 |   23 |          19 |
| `loadi`      |         21 |   22 |          18 |
| `loadi_wide` |         29 |   30 |          30 |
| `jlti_no`    |         26 |   26 |          24 |
| `jmp`        |         16 |   17 |          15 |
| `inc`        |         40 |   39 |          36 |
| `add`        |         42 |   41 |          40 |
| `muli`       |         49 |   46 |          48 |
| `mod`        |         50 |   49 |          51 |
| `getfield`   |         61 |   62 |          60 |

Values are rounded. `nop` shows the floor of the dispatch: 14 instructions. Over the 7 ops of `loopfn` that is 98 of 257 instructions (38%). The rest is the tag test, the arithmetic and the store.

### 3.4 What the machine code shows (main, `loopfn`)

- The loop head loads `pc` from `[rsp+0x50]` and `words` from `[rsp+0x58]`, then stores the word at `[rsp+0xfc]`. Each op stores `pc` back. So `pc`, `words`, `ci` and `fp` are stack slots in the hot loop.
- The 157 call sites of `unwind` pass `lea rcx,[rsp+0x18]`, `[rsp+0x20]` and `[rsp+0x50]`. This is the `&mut ci`, `&mut fp` and `&mut pc` of `unwind`. Changing it did not move `pc` into a register (section 7.1, spike `s0`).
- `step!` is expanded twice: once for the narrow word and once for the `Wide` prefix. So each arm exists twice and there are two jump tables. `LoadI.w` (the 1,000,003 of `loopfn`) runs the prefix test, a wide decode of 19 instructions and a second jump. It costs 30 instructions against 22 for `LoadI`.
- All the narrow ops share one `jmp rax` (6 of 7 dispatches per turn). The cachegrind simulator counts 6.0 indirect mispredicts per turn. Real predictors with branch history do far better. I did not measure a real CPU.

### 3.5 Cache and branch counts (cachegrind, simulated)

20,000 turns of `loopfn`. The two trees are equal within 0.3% on every count.

| Count                             | `f3cfa8f8` |      main |
| --------------------------------- | ---------: | --------: |
| Instructions                      |  6,476,950 | 6,380,370 |
| Instruction-cache (I1) misses     |      3,677 |     3,637 |
| Branches (conditional + indirect) |    845,916 |   846,575 |
| Indirect mispredicts (simulated)  |    121,144 |   121,182 |

## 4. The current design, and what it costs

| Part     | Now                                                                                                      | Cost it causes                                                                                                                                                                                                    |
| -------- | -------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Loop     | One `loop` and one `match` on the op inside one function of 255,676 bytes (`interp.rs`)                  | 11 values are live across the whole loop (`realm`, `stack`, `fbs`, `prog`, `ci`, `fp`, `pc`, `code`, `words`, `fb`, `regs`). x86-64 has 15 usable registers. The allocator keeps `pc` and `words` in stack slots. |
| Arms     | 173 arms. 48 are cold, so about 460 of 960 lines are cold. Each arm is expanded twice (narrow and wide). | A call in a cold arm made the allocator save the loop state. `cold_path()` hints (#785) fixed the worst of it. Any new arm or edit can still move the hot arms.                                                   |
| Dispatch | One shared indirect jump for the narrow ops                                                              | 14 instructions per op. One jump site for all ops.                                                                                                                                                                |
| State    | Locals of `Vm::run`, `&mut` to the `Vec`s                                                                | No part of the state is fixed in a register by the code.                                                                                                                                                          |

## 5. How other engines solve it

| Engine               | Dispatch design                                                                                                                                                                                     | What it teaches us                                                                                                                                                                                                                                                                                                                           |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| CPython 3.11 to 3.13 | Computed goto, one `goto *table[op]` at the end of each arm (C extension of GCC and Clang).                                                                                                         | Each op has its own jump site. Not available in Rust.                                                                                                                                                                                                                                                                                        |
| CPython 3.14         | Optional tail-calling interpreter: one function per op, `musttail` and `preserve_none` (Clang 19 and later, x86-64 and AArch64).                                                                    | The first claim of 10% to 15% was wrong. A Clang 19 bug had merged the dispatch jumps in the baseline. The real gain is about 3% to 5% with Clang. It needs both attributes: `musttail` alone was bad. ([Elhage](https://blog.nelhage.com/post/cpython-tail-call/), [CPython issue 128563](https://github.com/python/cpython/issues/128563)) |
| LuaJIT 2             | Hand-written assembler interpreter with indirect threading and the state in fixed registers.                                                                                                        | The compiler does not allocate registers well in one big function. Pall's complaint is the same as our finding. Assembler is not an option for `bare`. ([lua-l](https://lua-users.org/lists/lua-l/2009-02/msg00411.html))                                                                                                                    |
| V8 Ignition          | Bytecode handlers are separate code snippets built with a machine-independent assembler and compiled by TurboFan. Each handler ends with a dispatch through a table. Accumulator and register file. | The handlers are separate units. An edit of one cannot move another. ([Ignition design](https://news.ycombinator.com/item?id=10034068))                                                                                                                                                                                                      |
| JavaScriptCore LLInt | Handlers written in `offlineasm`, a small language that makes assembler for each CPU.                                                                                                               | Same idea as Ignition, with hand control of the registers.                                                                                                                                                                                                                                                                                   |
| Wasmtime Pulley      | Two interpreters in Rust. A `loop { match }` (the default, stable). A tail loop with one function per op, which uses LLVM tail calls on stable (cfg flag) and is thought to be faster.              | The result is mixed: the match loop was 10% to 20% faster on two programs, the tail loop 3% to 4% faster on another, and others differ by 2% to 7% ([issue 9995](https://github.com/bytecodealliance/wasmtime/issues/9995), [docs](https://docs.wasmtime.dev/examples-pulley.html)). A tail loop is not a sure win.                          |
| Wasmi 2.0            | Several dispatch modes. With an unstable feature it uses `become`.                                                                                                                                  | Rust interpreters offer a switch between modes. ([blog](https://wasmi-labs.github.io/blog/posts/wasmi-v2.0/))                                                                                                                                                                                                                                |
| wasm3                | A direct-threaded interpreter in C. Each op is a function. The next op is a tail call. State is in arguments.                                                                                       | The origin of design (b) and (c). I did not check its numbers.                                                                                                                                                                                                                                                                               |

Rust options:

- `become` (explicit tail calls): nightly only. The feature is `incomplete` in the nightly of 2026-11-17 (tracking issue 112788). I confirmed this with a compile of a 3-line program on stable 1.96.0 and 1.98.1 (error E0658) and on nightly 1.102.0 (a warning that the feature is incomplete). Work goes on in 2026 (for example PRs 156007 and 165599), and it has known limits (`#[track_caller]`). No date for stable.
- `#[loop_match]` and `#[const_continue]`: nightly only (a compile of a small program on stable fails with E0554; on nightly it compiles with an "incomplete" warning). They aim at computed-goto code from a `loop { match }`. They do not remove the register pressure in a function of 255 KB.
- `return handler(args)` on stable: LLVM turns it into a jump at `opt-level` 1 and above, but nothing guarantees it. I measured a debug build of the toy: it overflows the 8 MB stack at 100,000 turns (700,000 ops). With `become`, a debug build ran 1,000,000 turns.
- Outlined cold arms (`#[inline(never)]` functions) and a state struct: section 7.1.

## 6. Designs

### 6.1 (a) Keep `loop { match }`, outline the cold arms

The hot function has the hot arms only. Its `match` lists each cold op in one arm that calls the cold function and then reloads `ci`, `fp`, `pc` and the frame locals. The cold function runs one op and returns the new state. Keep this arm as a list of ops, not as `_ =>`: a `_` arm adds a bounds check to both jump tables (spike `sx`).

- Instructions per op: no change by design. Measured -0.5% to -4% in the rows (section 7.1).
- Code size: `Vm::run` -38% (255,676 to 158,800 bytes). The text of the spike binary grows from 1,610,060 to 1,944,820 bytes, because `run_one` is a script copy that also holds the hot arms. The real change has each arm once, so I expect about the same total (hypothesis).
- Register pressure: down (rsp ops 40 to 25 per turn), but `pc` is still a stack slot in some arms.
- Robustness: a cold arm cannot move the hot code. Hot arms still move each other.
- Portability: stable Rust, all targets.
- Unsafe surface: no change.
- Debug: simple. The cold function is one `match`.
- Cost: about 2 days. The spike copies the function by script. The real code needs the macros of `Vm::run` (`r!`, `throw!`, `enter!`, `ret!`) to serve two functions. A `macro_rules!` that holds the locals and the macros together does it, or a small `Frame` struct. This is the main risk.

### 6.2 (b) Tail-call handlers, state in arguments

One `unsafe fn(ip: *const u32, regs: *mut Value, vm: &mut Vm) -> Ret` for each op. Each handler ends with `become next(...)` (nightly) or `return next(...)` (stable). The three state words are in registers on every handler entry. `ci`, `fp`, `code`, `fb` are loaded from the frame record or the `Vm` when an op needs them. A slow path must be a tail call too, to a cold handler. A non-tail call in a hot handler makes LLVM push and pop the callee-saved registers on every op (I saw 5 pushes and 5 pops in my first toy).

- Instructions per op: toy 25 per op against 31 for the match loop (stable), 24 with `become`.
- Code size: one function for each op, about 30 hot handlers. Each handler is small, so the hot code is dense.
- Register pressure: none between handlers. The allocator sees one handler at a time.
- Robustness: best. An edit of one handler changes only that function.
- Portability: `become` is nightly. Stable relies on LLVM. x86-64 System V has 6 argument registers and Windows x64 has 4, so 3 or 4 arguments fit on both. aarch64 has 8. The compiler decides tail calls for stable code. I could not test Windows, macOS or arm64.
- Unsafe surface: larger. The handlers are `unsafe fn` and the table holds function pointers. The `Op` transmute goes away.
- Miri: the toy ran under Miri with `become` (2,000 turns). The stable `return` version ran at 2,000 turns and did not finish 100,000 turns in 250 s (not diagnosed).
- Debug: a stable debug build overflows the stack (above). The tests of `cargo test` in debug need a second engine or `become`. Profiles are easier to read: each op is a symbol.
- Cost: large. All the arms become handlers (1.5 to 2 weeks for the hot ones, hypothesis).

### 6.3 (c) Direct-threaded code

The load step turns each instruction into `[handler pointer][operands]`. A handler jumps to `ip->handler`. No table lookup, no decode of the op byte.

- Toy: 159 instructions per turn (stable), 158 with `become`. Against 175 for design (b), a saving of 9%.
- Memory: the toy instruction has 16 bytes, against 4 for a word. A bundle of 1 MB of bytecode becomes 4 MB (hypothesis for the ratio on real code; the toy shows 16 against 4 bytes per instruction).
- It breaks four things: the verifier and the `Wide` form, the root maps keyed by `pc`, the exception tables, and the jump offsets. A load-time pass adds to time to ready.
- I do not recommend it now.

### 6.4 (d) Hybrid: (b) for the hot ops, (a) for the rest

The table has about 30 hot handlers. Each of the other ops points to one `h_cold` handler. `h_cold` calls the outlined function of design (a), which runs one op, and then tail-dispatches again. Step 1 of the plan builds the cold function for its own gain. Step 2 adds the handlers.

Expected instructions, step 2 against main (hypothesis). The toy saves 44 of 219 instructions per turn, 6.3 per op, with stable tail calls. The real function also has about 38 instructions per turn of spill cost that handlers may remove, so I use 6 to 9 instructions per op (the toy plus part of the spill). Ops per row (from `interp_load`): `loopfn` 7, `calls` 12, `fib` 17.5, `closures` 5, `objects` 2.4 per turn or call.

| Row        | Net M, main default | Expected saving | Expected change |
| ---------- | ------------------: | --------------: | --------------: |
| `loopfn`   |               272.5 |        42 to 63 |    -15% to -23% |
| `calls`    |               586.5 |       72 to 108 |    -12% to -18% |
| `fib`      |             1,103.6 |      105 to 158 |    -10% to -14% |
| `closures` |               366.9 |        30 to 45 |     -8% to -12% |
| `objects`  |               141.5 |        14 to 22 |    -10% to -15% |

These are instructions, not time. Pulley and another project (catallaxy, [issue 6](https://github.com/CodingAnarchy/catallaxy/issues/6)) report a tail loop that was slower in time on some programs. The first step of the plan below checks it on this engine before the rewrite.

### 6.5 Comparison

|                                              | (a) outline                             | (b) tail handlers                      | (c) direct threaded  | (d) hybrid                      |
| -------------------------------------------- | --------------------------------------- | -------------------------------------- | -------------------- | ------------------------------- |
| Instructions per turn, toy (match loop: 219) | n/a                                     | 175 (stable), 167 (`become`)           | 159                  | as (b)                          |
| Instructions on `loopfn`, real               | -1.1% (default), -2.1% (cgu1), measured | -15% to -23%, hypothesis               | not estimated        | as (b)                          |
| Code size of `Vm::run`                       | -38%                                    | hot handlers only                      | as (b)               | -38% plus handlers              |
| Bytecode memory                              | same                                    | same                                   | about 4x, hypothesis | same                            |
| Edit swing                                   | cold arms: none (measured)              | per handler                            | per handler          | per handler, and cold arms none |
| Rust                                         | stable                                  | stable (LLVM, no guarantee) or nightly | as (b)               | as (b)                          |
| Unsafe                                       | same                                    | more                                   | more                 | more                            |
| Miri                                         | same as now                             | runs on the toy                        | runs on the toy      | runs on the toy                 |
| Debug build                                  | same                                    | stable overflows, `become` works       | as (b)               | as (b)                          |
| Cost                                         | about 2 days                            | 1.5 to 2 weeks                         | more than (b)        | (a) + (b)                       |

## 7. Spikes

All code of the spikes is outside this change. Patches, the toy, the scripts and the raw output are in `/data/bare-tmp/dispatch-experiments` on archMachine. They are not in the PR and not on GitHub.

### 7.1 Real engine: design (a) and the `unwind` change

Spikes built from main `fd9c749b`. Net M instructions, lowest of 3 runs, and the percent against main of the same build.

| Spike | What it is                                                                                                            |
| ----- | --------------------------------------------------------------------------------------------------------------------- |
| `s0`  | `unwind` takes and returns `(ci, fp, pc)` by value.                                                                   |
| `sx`  | The 48 cold arms moved into `run_one` (a copy of the function that runs one op). The hot `match` has `_ =>` for them. |
| `sx2` | As `sx`, with the cold ops listed in the pattern (no bounds check).                                                   |
| `s12` | `sx2` plus `s0`.                                                                                                      |

Default build:

| Tree  |       `loopfn` |        `calls` |            `fib` |     `closures` |      `objects` |
| ----- | -------------: | -------------: | ---------------: | -------------: | -------------: |
| main  |         272.50 |         586.50 |         1,103.60 |         366.89 |         141.53 |
| `s0`  | 275.50 (+1.1%) |  586.50 (0.0%) | 1,119.75 (+1.5%) | 368.09 (+0.3%) | 142.93 (+1.0%) |
| `sx2` | 269.50 (-1.1%) | 565.50 (-3.6%) | 1,061.86 (-3.8%) | 358.09 (-2.4%) | 135.99 (-3.9%) |
| `s12` | 273.50 (+0.4%) | 574.50 (-2.0%) | 1,087.44 (-1.5%) | 361.69 (-1.4%) | 140.59 (-0.7%) |

cgu1:

| Tree  |       `loopfn` |        `calls` |            `fib` |     `closures` |      `objects` |
| ----- | -------------: | -------------: | ---------------: | -------------: | -------------: |
| main  |         282.50 |         579.50 |         1,064.55 |         365.95 |         143.53 |
| `s0`  | 290.50 (+2.8%) | 590.50 (+1.9%) | 1,087.44 (+2.1%) | 372.35 (+1.7%) | 147.93 (+3.1%) |
| `sx2` | 276.50 (-2.1%) | 555.50 (-4.1%) | 1,057.82 (-0.6%) | 356.75 (-2.5%) | 141.39 (-1.5%) |
| `s12` | 284.50 (+0.7%) | 581.50 (+0.3%) | 1,086.09 (+2.0%) | 368.15 (+0.6%) | 146.39 (+2.0%) |

The `sx` spike (the `_ =>` arm), default build: `loopfn` 297.50 (+9.2%), `calls` 605.50 (+3.2%), `fib` 1,118.41 (+1.3%), `closures` 374.69 (+2.1%), `objects` 144.73 (+2.3%). The hot trace shows `cmp dl,0x9b; ja` before each jump table.

What the spikes show:

- Spike `sx2` is the only one with a gain in every row at both builds, except `objects` at cgu1 (-1.5%, also a gain). The gain is small and steady.
- The `unwind` change (`s0`, `s12`) does not help. Drop it.
- `sx2` callgrind, `loopfn`: 254 instructions per turn in `Vm::run` (main 257), rsp ops 25 per turn (main 40), `Vm::run` 158,800 bytes (main 255,676). `pc` is still stored in `[rsp+0x30]` in several arms. So the pressure is lower but not gone.
- `cargo test --locked --release -p bare-js2 --lib` on `sx2`: 445 passed. This does not cover the 48 outlined arms beyond what the tests already run.
- **Stability test.** I edited the cold arm `GetSuper` of `sx2` (an `ensure!` of 100,000 words, a `black_box` loop over all registers, and 2 `gate!()`). All 10 numbers (5 rows, 2 builds) equal the `sx2` numbers to 0.01 M. The same edit shape in the hot arm `PopCtx`: main moved `loopfn` +2.2% and `closures` +2.8% at the default build, and `closures` +2.4% at cgu1. The same hot-arm edit on `sx2` moved nothing (0.00 M in all 10 numbers). One edit in each tree is not a proof for all edits.

### 7.2 Toy: the dispatch styles with the same ops

A single-file crate (`toy`) with the 7 ops of `loopfn`, the value tag of `value.rs` (tag `0xFFF9` in the top 16 bits), the word encoding of `bytecode.rs` (including the `Wide` prefix of `LoadI.w`), the integer fast paths and a cold call for each other path. It is **not** the engine: it has no calls, no frames, no cache, no GC and no feedback. It gives a floor for dispatch.

Instructions per turn of the loop code only (callgrind, function-level, the Rust check loop left out), 20,000 turns:

| Style                                                                 | Instructions per turn | Per op |
| --------------------------------------------------------------------- | --------------------: | -----: |
| `loop { match }`, `pc` and `regs` in locals (the shape of the engine) |                   219 |   31.3 |
| Same, with `&mut pc` passed to a cold call                            |                   232 |   33.1 |
| Tail calls, stable (`return h(..)`), 256-entry table                  |                   175 |   25.0 |
| Tail calls, nightly `become`                                          |                   167 |   23.9 |
| Direct threaded, stable                                               |                   159 |   22.7 |
| Direct threaded, `become`                                             |                   158 |   22.6 |

Simulated branches (cachegrind): the match loop has 6.0 indirect mispredicts per turn and 40 branches. The tail and direct styles have 0.02 mispredicts and 25 branches. The simulator uses a one-target predictor, so the gap on a real CPU is smaller (not measured).

What the toy shows:

- The first version of the tail handlers had 5 pushes and 5 pops in each handler, because the slow path was a normal call. The score was 207 per turn. Making the slow path a tail call to a cold handler removed them (175). The same rule applies to the real handlers.
- The rest of the cost is the value work (tag tests, the arithmetic), about 25 instructions for an `Add`. A better dispatch does not remove it.
- The `match` toy with 4 live values costs 219. The engine costs 257. This 38 is the upper bound of what less pressure can give the engine in design (a).

Stable tail calls in a debug build overflow at 100,000 turns. With `become` the debug build ran 1,000,000 turns. Miri ran the toy with `become` at 2,000 turns.

## 8. Recommended plan

Design (d) in two steps. Each step has its own PR. Rules for both: the cost rows at the default build and at cgu1 on both trees (the rule of `docs/progress.md`, "cost cgu rule"); test262 with no test lost; Miri on the interpreter modules; a Windows build.

**Step 1: outline the cold arms.**

1. Make the macros of `Vm::run` serve two functions (one `macro_rules!` holds the locals and the macros). Check: `cargo test --locked -p bare-js2 --lib` passes; the code of the hot arms is the same text.
2. Move the 48 cold arms into the cold function. The hot `match` lists the cold ops in one arm. Check: no `_ =>` arm and no bounds check before the jump tables (`objdump` of `Vm::run`); callgrind per-turn count of `loopfn`.
3. Measure the five rows, 3 runs, both builds. Expected: -1% to -4%, no row above main. Check the stability test: a heavy edit of a cold arm moves no row.
4. Add a test, a note at the `match` and a line to `docs/progress.md`. The note says: a new cold op goes in the cold function.
5. Checks: Miri on the `interp` and `interp_rt` modules; `cargo xwin test` on the Windows VM; test262 against `tools/fixtures/js/test262-js2-baseline.txt`.

**Step 2: tail-call handlers for the hot ops (starts after the owner answers questions 1 and 2).**

1. Spike on the real engine: handlers for the ops of `loopfn` and `fib` only, the rest through `h_cold`. Measure instructions, and wall time on an idle machine (not available today). Stop here if the wall time does not improve by at least 3% on `loopfn` and `fib`.
2. Handlers for the other hot ops (loads, moves, arithmetic, compare, jumps, field and global caches, call, return). Each slow path is a tail call to a cold handler. Check: no `push` or `pop` of callee-saved registers in a hot handler (`objdump`); no stack growth in a loop of 100 M ops in release.
3. Keep the `loop { match }` engine for debug builds and as a reference (question 2). A differential test runs both engines on the corpus of `bare-js-diff`.
4. Checks: cost rows at both builds; test262; Miri; Windows and aarch64 builds; the stability test on one hot handler.

Expected gain: step 1, -1% to -4% instructions on the five rows. Step 2, section 6.4 (hypothesis). Browser slice: section 1 (hypothesis, not measurable now).

## 9. Risks

- Step 1: the macros may not share across two functions without a large change. Fallback: a `Frame` struct that holds `ci`, `fp`, `pc` and the realm references.
- A gain in instruction counts may not appear in time. This is the Pulley result. The plan stops step 2 at its first check if so.
- Stable tail calls depend on LLVM. A future compiler may not make a tail call in one handler. A test that runs 100 M ops in release catches it. Nightly `become` removes the risk and adds an unstable feature (it is `incomplete`).
- A second engine for debug builds doubles the arms. Op bodies must be macros that both engines use.
- Windows x64 has 4 argument registers. 3 words of state fit. A fourth word (for example `fp`) needs a check there.

## 10. Not measured

- Wall time, cycles, real cache misses and real branch misses: the machine had a load average of 17, and no idle machine was available.
- Windows, macOS, arm64. The Windows and arm64 behavior of tail calls and of the toy.
- Miri on `Vm::run` or on any spike of the real engine. Miri on the toy only.
- test262 on the spikes. Only `cargo test -p bare-js2 --lib` on `sx2` (445 passed).
- A real tail-call engine. Only the toy, which has no calls, frames or caches.
- Instruction counts of the rows `closures` and `objects` for a hot-arm edit other than `PopCtx`.
- The browser slice: js2 is not wired in.
- The cost of the cold function's reload of the state on each cold op. The rows have no cold op in their loops. A script that uses classes in a loop would show it (hypothesis: about 50 instructions per cold op).

## 11. Questions for the owner

Each question has my recommended answer first.

1. **Stable or nightly for tail-call dispatch (step 2)?** Recommended: stable Rust with LLVM tail calls in release, the `loop { match }` engine kept for debug, and `become` as a later switch when it stabilizes. Alternatives: nightly `become` (guaranteed tail calls, debug works, but the whole build needs nightly or a feature gate), or never do step 2.
2. **Keep the `loop { match }` engine as a second engine for debug and tests?** Recommended: yes, behind the same op macros, and run a differential test on both. Without it, the stable tail design cannot run `cargo test` in debug (stack overflow at 100,000 turns in the toy).
3. **Build step 1 now, before the step 2 decision?** Recommended: yes. It gives -1% to -4% and makes cold-arm edits free. It is also the fallback path of step 2.
4. **Accept a gain in instruction counts as the first gate for step 2, with the wall-time check on an idle machine as the second?** Recommended: yes, and run the wall-time check before the full handler rewrite (step 2, item 1).
5. **Direct-threaded code (c): drop it?** Recommended: drop it. It saves 9% more instructions than (b) in the toy and costs 4x bytecode memory (memory is a stated priority) and a rewrite of the verifier, the root maps and the jump encoding.
6. **Wire js2 into the browser before step 2, so the browser slice can be measured?** Recommended: no. Step 2 is judged on the five rows and on wall time. The browser slice follows when js2 is wired.

## Decisions (owner, 2026-11-21)

1. Build step 1 (outline the cold arms of `Vm::run`) now.
2. Step 2 uses stable Rust with LLVM tail calls in release builds. Keep the `loop { match }` engine for debug builds and tests, with a differential test on both. Switch to `become` when it is stable.
3. Gate step 2 first on instruction counts from a real spike, then on a wall-time check on an idle machine, before the full rewrite.
4. Drop direct-threaded code.
5. Do not wire js2 into the browser only to measure step 2.

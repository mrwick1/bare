# Design: superinstructions and the cost of a dispatch in js2 (plan step S2.9)

Status: for review by the owner. Nothing in this document is built. The only code in the PR is the counting feature `op-count`, the examples that use it, and one test program.

Base: PR #763 (`t3/js2-core-builtins`: S2.7 and the switch table). Machine: archMachine, AMD Ryzen 9 5900HX, Linux. Method and raw numbers: `docs/progress.md`, section "2026-11-20: where the instructions go in js2 (plan step S2.9, measurement)".

Rules used here: a number is measured unless it is marked "hypothesis". A "model" number comes from the per-op costs below and the op counts. The model predicts the measured totals of `loop`, `loopfn`, `calls` and `fib` within 2%.

## 1. Summary

1. The plan said: fuse pairs, because fusing 8 pairs removes 17.7% of dispatches (design 6.11 item 8). With the new compiler the figure is higher: the top 8 pairs of a program remove 15% to 50% of its dispatches (bundle-like: 21%; section 3). So fusion works. These figures use the top list of each program, so they are an upper bound for one shared list (section 3).
2. But fusion alone does not reach QuickJS. js2 runs 30% to 50% fewer dispatches than QuickJS on the loop rows, and each dispatch costs about 3 times more (56.8 against 16.7 instructions on `loop`). The cost of one dispatch, not the count, is the main gap.
3. One dispatch costs 15 instructions before the op does anything, and 4 to 10 more to reach a register. Both costs come from how `Vm::run` keeps its state (section 2).
4. Two costs outside the dispatch matter as much: `GetGlobal` and `SetGlobal` (the top-level `var`s of `eval_load` programs are global properties; 44% to 57% of the net instructions on `loop` and `calls`), and the call and return pair (303 instructions; 59% of `fib`).
5. #763 has a regression that is not about dispatch. `script::load` deletes the `__def` global after the prelude, which puts the global object in dictionary mode, so every global access takes a slow path. It makes `loop` 3.4 to 3.7 times slower than variant B (152 M against 519 M to 567 M at scale 1). The numbers in this document use the build without the delete (variant B). Section 7 gives the evidence.
6. Recommendation: do not start with fused ops. Do five steps in this order (section 5): (a) fix the `__def` delete; (b) keep the register base in a local pointer; (c) a fast path for global access; (d) a fast path for the call and return pair, with two compiler changes; (e) then fuse the top 8 pairs and 8 triples, chosen from the histogram taken after (a) to (d). Expected result in instructions against QuickJS (hypothesis for all but (b)): `loop` 0.85, `loopfn` 0.73, `calls` 0.77, `fib` 0.78 at scale 10, `closures` 0.41, `throws` 0.28, bundle-like 0.83. `objects` (1.21), `webpack` (1.2 to 1.8) and `concat` (2.3 to 3.4) do not reach the gate with this work. Section 6 says why and what they need.

## 2. The design today and what it costs

### 2.1 Dispatch

`Vm::run` (`crates/js2/src/interp.rs`) is one `loop` with one `match` on the opcode. A word is `[op:8][a:8][b:8][c:8]`. A `Wide` prefix is tested before the match. The `step!` macro makes every arm twice, once for the narrow form and once for the wide form, so `run` is 355 KB of machine code (0x58b7a bytes), 19% of the 1.88 MB `script_run` binary. The 177 used opcodes leave 79 free in the 8-bit space.

The dispatch is 15 instructions, found with callgrind and `objdump` on the profiling build:

| Part      | Instructions | What                                                                                                                                                                                                                                                 |
| --------- | -----------: | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Loop head |           12 | reload `pc` from the stack frame; load the word; test for `Wide`; store `words` to the stack; compute `next` and `nextx` (two `lea`, for every op); extract the opcode; load the jump table base; load and add the table entry; one shared `jmp rax` |
| Arm tail  |            3 | store `pc`; reload `words`; jump back to the head                                                                                                                                                                                                    |

In the base of #763 (e1af87b1) the dispatch was 14 instructions. The extra one is the cause of the +2.4% on `fib` with no change in the hot path: in the new build, `words` is in `r13`, and the `lea r13,[rcx+2]` for `nextx` overwrites it. So the compiler stores `words` at the head and reloads it at the tail. This is register allocation in one very large function, not a change in an arm. (Verified by the two disassemblies; the per-op costs in 2.3 are measured on both builds.)

There is one shared `jmp rax`, so the branch predictor sees one indirect branch for all ops. On modern cores (Zen 3 here) the predictor uses the global history, so this is less costly than it was. Hypothesis: it is not the main cost. This document counts instructions, not branch misses (not measured).

### 2.2 Register access

A register read is `*stack.as_mut_ptr().add(fp + i)`. The compiler cannot keep `stack.ptr` or `fp` in a register across the arms, so each access reloads `fp`, shifts it, loads `stack.ptr` and adds. That is 4 instructions for each register operand. Two ops share an arm (`Add|AddI`), and the arm tests the op with a `cmp bl,0x3e` at run time (2 more). The tag masks are rebuilt with `movabs` in the arm.

### 2.3 Cost of each op, measured

Method: `examples/op_cost.rs` builds a loop of K=8 copies of the op, and `tools/js/instr.sh` counts the instructions (`perf stat`, event `instructions:u`, minimum of 3 runs; the 3 runs were equal). Cost = (count at K=8 minus count at K=0) / (turns x 8). The dispatch is included.

| Op                         | Instructions |     | Op                        | Instructions |
| -------------------------- | -----------: | --- | ------------------------- | -----------: |
| `Nop`                      |           15 |     | `GetField` (mono, hit)    |           65 |
| `Mov`                      |           26 |     | `SetField`                |           69 |
| `LoadI`                    |           26 |     | `GetGlobal`               |           68 |
| `LoadI` wide               |           37 |     | `SetGlobal`               |           74 |
| `AddI`, `Add` (int)        |           53 |     | `GetCtx`, `SetCtx`        |           57 |
| `Add` (double)             |           58 |     | `GetElem`, `GetElemI`     |     100, 101 |
| `MulI`                     |           56 |     | `NewObjLit`               |          182 |
| `Mod`, `ModI`              |           62 |     | `NewObj`                  |          115 |
| `Inc`                      |           49 |     | `Closure`                 |          208 |
| `JnLtI`, not taken / taken |      32 / 39 |     | `StrictNeq`               |           46 |
| `JSNeNull`, not taken      |           29 |     | `Call` + `Ret` (any form) |          303 |
| `JmpIfNot`, not taken      |           37 |     | `Jmp` / `JmpBack`         |      19 / 29 |

Measured pairs: `Lt` + `JmpIfNot` = 80, while the fused `JGt` = 37 (saves 43); `GetGlobal` + `JGt` = 105; `Mov` + `Mov` = 52; `Inc` + `Jmp` = 68.

The call and return pair: 238 in `Vm::run`, 51 in `Realm::function` (a shape-slot lookup for the code and the context of the callee, on every call), 13 in `memcpy` (the 10-word frame record), and bounds-checked `prog.codes[ci]` and `fbs[ci]` in `call!`, `ret!` and `reload!`.

### 2.4 Where the instructions go, per row

Model: net instructions (scale 10 minus scale 1, so the setup cost is out) split by the per-op costs. "Dispatch" is 15 per dispatch. "Global ops" is `GetGlobal` and `SetGlobal` including their dispatch. "Calls" is the `Call`/`Ret` work above the dispatch.

| Row | Dispatch | Global ops | Calls | Other ops (work after the dispatch) | Alloc, natives, strings, slow paths |
| --- | ---: | ---: | ---: | ---: | ---: |
| `loop` | 26.4% | 57.0% | 0 | 16.6% | 0 |
| `loopfn` | 33.0% | 0 | 0 | 67.0% | 0 |
| `calls` | 21.6% | 44.1% | 27.3% | 7.0% | 0 |
| `fib` | 19.0% | 13.3% | 59.0% | 7.1% | 1.6% |
| `closures` | 15.4% | 25.8% | 27.0% | 14.4% | 17.4% |
| `objects` | 13.4% | 21.3% | 15.1% | 7.2% | 43.0% |
| `throws` | 16.2% | 34.5% | 0 | 6.3% | 43.0% |

(The "other ops" column is what is left of the model total. The last column is work that the model does not cover, from the callgrind profile. Each row sums to 100%. The "other ops" work is mostly register access and the arithmetic itself.)

### 2.5 Against QuickJS, whole process

Instructions of the whole process (`script_run --eval`, `qjs`), in M. Scale 1 is the S2 gate input (the setup cost is in it); scale 10 shows the slope. js2 is variant B (no `__def` delete).

| Row               | js2, scale 1 | QuickJS, scale 1 | js2 / QuickJS, 1 | js2 / QuickJS, 10 | Dispatches, js2 (scale 10) | Dispatches, QuickJS |
| ----------------- | -----------: | ---------------: | ---------------: | ----------------: | -------------------------: | ------------------: |
| `loop`            |        152.1 |             64.7 |             2.35 |              2.33 |                     26.0 M |              38.0 M |
| `loopfn`          |         68.2 |             44.1 |             1.55 |              1.49 |                     14.0 M |              28.0 M |
| `calls`           |        115.5 |             60.6 |             1.90 |              1.87 |                     16.0 M |              24.0 M |
| `fib`             |         8.84 |             4.37 |             2.02 |              1.47 |                    0.603 M |              1.02 M |
| `closures`        |         22.2 |             26.1 |             0.85 |              0.64 |                      1.8 M |              2.35 M |
| `objects`         |         91.1 |             53.7 |             1.70 |              1.67 |                      7.6 M |               8.2 M |
| `webpack`         |         9.75 |             5.48 |             1.78 |              1.24 |                       55 K |                81 K |
| `throws`          |         39.8 |             88.5 |             0.45 |              0.41 |                      3.8 M |               9.1 M |
| `concat`          |         4.61 |             1.34 |             3.45 |              2.46 |                       10 K |                15 K |
| bundle-like (new) |        128.0 |            136.5 |             0.94 |               n/a |                    0.907 M |              1.49 M |

Slope in instructions for each dispatch (net, js2 against QuickJS): `loop` 56.8 against 16.7; `loopfn` 45.4 against 15.3; `calls` 69.3 against 24.7; `fib` 79.0 against 33.6; `objects` 111.8 against 62.2; `closures` 97.2 against 120.

The setup cost matters at scale 1: an empty program is 4.47 M instructions on js2 and 1.23 M on QuickJS. About 3.2 M of the difference is the compile (lex, parse, encode) of the prelude on each start. No change in the interpreter loop can remove it. `concat` (4.6 M against 1.3 M), `fib` scale 1 and `webpack` scale 1 are bound by it.

## 3. The histogram

Dynamic counts with the `op-count` feature, on the 9 `eval_load` rows at scale 10 and on a webpack-like bundle (`examples/data/bundle_like.js`: a module table, a `require` with a cache, a store with a reducer `switch`, event subscription, an `assign` helper, a render with `for-in` and string concatenation; it runs in js2 and Node with the same result, 125972). The counter sees each dispatch. A pair is "fusable" when the second op is the next instruction in the code and no jump, handler or switch lands on it, so one fused op can replace both without a change of control flow.

Top 10 fusable pairs, mean share of all dispatches over the 9 `eval_load` rows plus the bundle-like program (10 programs); the second column is the bundle-like program alone:

| Pair                      | Mean, 10 programs | Bundle-like |
| ------------------------- | ----------------: | ----------: |
| `GetGlobal` + `Inc`       |             4.71% |         n/a |
| `Inc` + `SetGlobal`       |             4.71% |         n/a |
| `GetGlobal` + `JnLtI`     |             4.20% |         n/a |
| `SetGlobal` + `JmpBack`   |             4.19% |         n/a |
| `JnLtI` + `GetGlobal`     |             4.19% |         n/a |
| `SetGlobal` + `GetGlobal` |             3.29% |         n/a |
| `GetGlobal` + `GetGlobal` |             3.12% |         n/a |
| `LoadI` + `Mod`           |             2.91% |         n/a |
| `Add` + `LoadI`           |             2.89% |         n/a |
| `MulI` + `Add`            |             2.82% |         n/a |

The first 7 pairs exist because the `eval_load` programs keep their variables in globals. Real code (the bundle-like program) has none of them. Its top pairs are: `GetField` + `JnLt` 6.0%, `Inc` + `JmpBack` 4.5%, `LoadAtom` + `Add` 3.0%, `Mov` + `GetField` 2.8%, `Add` + `Inc` 2.8%, `Mov` + `Call` 2.6%, `GetField` + `Mov` 2.6%, `GetCtx` + `GetField` 2.3%, `GetField` + `GetField` 2.2%, `JnLt` + `Mov` 2.0%. Next in the eval_load list: `Inc` + `JmpBack` 2.32%, `Add` + `SetGlobal` 1.99%, `GetGlobal` + `SubI` 1.54%, `SubI` + `CallU` 1.54%.

Top triples (mean): `GetGlobal` + `Inc` + `SetGlobal` 4.71%, `GetGlobal` + `JnLtI` + `GetGlobal` 4.19%, `Inc` + `SetGlobal` + `JmpBack` 4.19%, `SetGlobal` + `GetGlobal` + `Inc` 3.29%, `Add` + `LoadI` + `Mod` 2.82%, `MulI` + `Add` + `LoadI` 2.82%.

Hot ops of the bundle-like program: `GetField` 16.8%, `Mov` 12.0%, `Add` 7.9%, `JnLt` 6.4%, `LoadAtom` 5.3%, `JmpBack` 5.1%, `Inc` 4.5%, `JSNeA` 4.5%, `Call` 3.9%, `GetCtx` 3.8%, `Ret` 3.7%.

Dispatches removed by greedy fusion on the recorded run (left to right; a triple first, then a pair):

| Program | 8 pairs | 8 pairs + 8 triples | 16 + 16 | 32 + 32 |
| --- | ---: | ---: | ---: | ---: |
| `loop` | 38.5% | 61.5% | 61.5% | 61.5% |
| `loopfn` | 42.9% | 57.1% | 57.1% | 57.1% |
| `calls` | 37.5% | 56.2% | 62.5% | 62.5% |
| `fib` | 23.1% | 46.2% | 46.2% | 46.2% |
| `closures` | 27.8% | 44.4% | 58.3% | 58.3% |
| `objects` | 28.9% | 39.4% | 52.6% | 60.5% |
| `webpack` | 15.4% | 31.8% | 39.9% | 52.7% |
| `throws` | 31.6% | 45.6% | 54.4% | 57.9% |
| `concat` | 49.9% | 59.9% | 60.0% | 60.0% |
| bundle-like | 21.3% | 28.6% | 39.1% | 48.3% |

Caveat: each cell uses the top list of that program alone. One list for all programs removes less. I did not measure the shared list (to verify; `op_count` would take the union of the lists of all files). The `eval_load` loops are small, so a few patterns cover them. The bundle-like program is a better guide to real code: 8 pairs remove 21%, and the gain grows slowly (33.8% with 32 pairs).

One more fact: `loop` runs a wide `LoadI 1000003` in each turn, because the constant does not fit in 16 bits. It is 7.7% of the dispatches, and it costs 37 instructions each turn. The compiler could keep it as an operand (section 4, option 2).

## 4. Options

Common basis for the gain figures: a fused op removes the dispatch (15) and the work that its two parts do twice (a second register-base reload, the store and load of an intermediate): about 21 instructions for each removed dispatch (hypothesis; range 15 to 30). The measured case, compare-and-branch, saves 43.

### Option 1: fused pairs and triples from the histogram

What: new opcodes for the frequent chains, for example `GetField`+`JnLt`, `Inc`+`JmpBack`, `LoadAtom`+`Add`, `Mov`+`Call`.

Data layout (the overlay encoding): the fused opcode replaces the opcode byte of the first instruction, in place. The words of the second instruction stay where they are. The fast arm does both ops, then sets `pc` after the second instruction. The slow path of the fused arm (a cache miss, a type that is not the expected one) runs the normal arm of the first op and falls into the second. So the code has the same length, every jump target, root map, handler range and line entry is where it was, and a debugger or the disassembler can still see two instructions. The 177 used opcodes leave 79 slots. 8 pairs and 8 triples use 16.

Verifier impact: small. `verify` already decodes by `Op::form`. It adds one rule: a fused op must be followed by the instruction it expects, and no jump target may land on the second instruction. The compiler (or a pass after `encode`) writes the fused ops, so the verifier checks the result, as it checks a switch table today. A fused op is a safepoint when one of its parts is; its root map entry sits at the pc of the first word, and the part that can allocate is the first part or the second. To avoid a second map entry, the rule is: only the first part may be a safepoint (`GetField`+`JnLt` yes, `Inc`+`SetGlobal` yes if `SetGlobal` is not a safepoint). Check against the `roots` code: to verify.

Expected gain (model, 21 instructions for each removed dispatch, 8 pairs + 8 triples with the list of each program, so an upper bound), instructions of the whole process at scale 10, as a ratio to QuickJS:

| Row         | Now / QuickJS | After fusion alone |
| ----------- | ------------: | -----------------: |
| `loop`      |          2.33 |     1.80 (8 pairs) |
| `loopfn`    |          1.49 |               1.10 |
| `calls`     |          1.87 |               1.56 |
| `fib`       |          1.47 |               1.30 |
| `closures`  |          0.64 |               0.58 |
| `objects`   |          1.67 |               1.55 |
| `webpack`   |          1.24 |               1.23 |
| bundle-like |          0.94 |               0.90 |

The figures after fusion use the 8 + 8 removal counts of the table in section 3. The model and its output are in `docs/progress.md`.

Code size: each fused arm is added to `run` twice (narrow and wide), because of `step!`. Estimate: 1.5 KB of machine code for each fused arm with its slow path (hypothesis; to measure on the first one). 16 arms would add about 24 KB, 7% of `run`.

Risk: the gain is real but it is 23% to 26% on the best rows and 4% on bundle-like code (model, upper bound), as long as the dispatch and register access cost 15 + 4 each. A fused arm also adds to the size of `run`, and the layout of `run` already moves each op by 2% (the `fib` case above). The histogram depends on the compiler: it moved from 17.7% (design) to 21% to 39% (now) and will move again after the S2.5 follow-ups.

### Option 2: compare-and-branch and operand-form fusion (in the compiler, no new dispatch design)

What: use the forms that exist (`JLt`, `JnLtI`, `JSEqNull`, `JmpIfNot` with a fused compare) wherever the compiler can. Add operand forms where a constant sits in a register today: a `Mod`, `Mul` or `Add` with a 32-bit constant in the `x` word (`ModX`, `AddX`), so `LoadI wide` + `Mod` is one op; and rewrite `Jmp` to a `Ret` (a jump whose target is a `Ret` becomes the `Ret`, so `Mov; Jmp; Ret` is `Mov; Ret`).

Data layout: new `AbcX`-form ops with a 32-bit `x`. The verifier checks `x` as it checks `AsBxX`. No overlay, no new rule.

Expected gain: `LoadI wide` is 37 per turn on `loop`, `loopfn` and `calls`: 7.7% of dispatches, and about 8% of instructions on those rows (model: 1.38 against 1.49 on `loop`, 1.12 against 1.30 on `loopfn`, 1.09 against 1.16 on `calls`, after the other steps). The jump-to-return rewrite removes about 45 instructions for each leaf call of `fib` (a dispatch of `Jmp`: 19, plus the `Mov` result move; model).

Code size: about 3 new arms. Risk: low. The jump-to-return rule must keep the root map and the handler ranges right (a `Ret` inside a `try` with a `finally` must not change; the compiler has to emit the `Ret` only where the jump would leave all handlers anyway: to verify).

### Option 3: call and return specialisation

What: make the pair 303 into about 170 (hypothesis, from the parts in 2.3): (a) keep the code pointer and the context in the function cell in direct slots, so `Realm::function` (51) is not on the call path; (b) check `ci` once, then use `get_unchecked` for `codes[ci]` and `fbs[ci]` (to verify: the invariant "`ci` is a code index" holds when the function value is made); (c) write the 10-word record with 10 stores in the call arm (no `memcpy`: 13); (d) a call arm for a callee that has no rest parameter, no `arguments` and fewer or equal arguments than parameters (the common case), and a slow arm for all the others.

Data layout: two more words in the function cell (code index and context), written when the closure is made. The cell moves from N to N+2 words; the GC trace for the cell does not change when the new words are plain integers (to verify with the owner of S1 layouts).

Verifier impact: none (the call ops stay as they are; the invariant moves into `Closure`).

Expected gain: `calls` 1.37 to 1.16 (after the global change), `fib` 1.34 to 1.00 at scale 10 (model: 59% of `fib` is in the call and return). Zero on `loop` and `loopfn`.

Code size: smaller (the slow arm already exists). Risk: medium. It touches the cell layout (GC and the realm's function table) and the frame record. The `fib` +2.4% shows that `run` is sensitive. A test of the invariant: a call test in `interp_tests.rs` for each shape of the callee.

### Option 4: lean dispatch state (keep the register base in a local)

What: a local `regs: *mut Value` for `stack.as_mut_ptr().add(fp)`. It is set in `reload!`, after a `stack.resize` in `ensure!`, and after a call and return. `r!` becomes `*regs.add(i)`. This is a small change of about 12 lines.

Measured (spike, not kept): per op, instructions saved: `Mov` 3, `LoadI` 3, `LoadI` wide 5, `AddI` 8, `Add` 9, `MulI` 8, `Mod` 10, `Inc` 7, `JnLtI` 3, `GetGlobal` 4, `SetGlobal` 2, `GetElem` 10, `GetElemI` 9, `Lt` 7; `GetField`, `GetCtx`, `SetCtx`, `Jmp` and `Closure` 0; `Call` +7 (the pointer is set again).

Whole process, scale 10, instructions in M, variant B, before and after the spike: `loop` 1480.5 to 1356.5 (-8.4%), `loopfn` 640.6 to 556.6 (-13.1%), `calls` 1113.6 to 1054.6 (-5.3%), `fib` 52.2 to 50.9 (-2.5%), `closures` 179.6 to 174.1 (-3.1%), `objects` 855.8 to 825.2 (-3.6%).

Verifier impact: none. Risk: the pointer must be set again each time the stack can move (`ensure!`, a native that re-enters the VM, `unwind`, a GC that moves the stack: js2 does not move it today). A missed spot is a use after free, not a wrong value (to verify that no other code moves the stack), so it needs a test that grows the stack in a callee (deep recursion, `Vm::call` from a native) and Miri on `interp_tests` (not run in this task). The `fib` effect shows that the gain is about 2.5% and that it depends on how the compiler lays out `run`.

A deeper variant (not measured): an `ip: *const u32` instead of `pc` and `words`, no `Wide` test in the common path (a wide op gets its own opcodes, `LoadIW` and so on, or the prefix becomes a jump to a cold copy of the arm), and `next` computed only in the arms that use it. Estimate: 8 more instructions on each dispatch (hypothesis). The cost is a big edit of `run`, and a verifier rule that no instruction word run reaches an `ip` outside the code (a pointer, not an index). Not recommended in this step.

### Option 5: register-operand quickening

What: rewrite an instruction in place at run time after the first run, once the operand types are known (for example `Add` with two int32 becomes `AddII`, which skips the type test of the double path).

Data layout: the code array becomes mutable.

Verifier impact: large. `Code` is shared between realms today (to verify: the design says code is immutable and shareable). Writing into it makes a data race for two threads, or a copy for each realm. Each quickened form must pass the verifier rules again, or the verifier must be run on the result.

Expected gain: the int path of `Add` is 53 instructions, of which the type test is about 6 (hypothesis, not measured). So about 10% of an arithmetic op; 2% to 5% of a row. Not worth the risk. Rejected.

### Option 6: threaded dispatch and tail calls

What: replace the `match` by a table of function pointers, one per op, each ending in a tail call to the next (`become`, or "musttail" in C), or replicate the dispatch in each arm (computed goto).

Facts: stable Rust has no guaranteed tail call. `become` is nightly only (to verify against the toolchain pin of the repo; the project builds on stable today). Without a guaranteed tail call, each op returns to a loop and the gain is lost. Computed goto does not exist in Rust. `match` in a `loop` is the only form that stable gives; LLVM can merge the dispatch into each arm (tail duplication) when the function is small enough, and `run` is too large for that.

Expected gain: the dispatch in a good threaded interpreter is 4 to 6 instructions (hypothesis from other engines), against 15 here. But the 15 are mostly state reloads (option 4), so option 4 gets part of the gain without the language risk.

Risk: high, needs a nightly compiler. Rejected until `become` is stable. Revisit then.

### Option 7: the cost outside the dispatch (not a superinstruction, but the biggest single item)

7a. Global access cache. `GetGlobal` is 68 and `SetGlobal` is 74 instructions; the global object is a normal object with a shape, and a site cache can hold (shape id, slot). A hit is the same code as `GetField` hit (65, 4 of which are the dispatch state of option 4), so the gain is small unless the global cell is a direct slot: a per-site pointer to the global's slot, valid while the global shape stays (a "global cell" cache; V8 and JSC use it). Target: about 35 for `GetGlobal` and 38 for `SetGlobal` (hypothesis, from `GetCtx` at 57 minus the dispatch state of option 4 and the shape check).

Model: `loop` 2.14 to 1.49 against QuickJS (after option 4), `calls` 1.77 to 1.37, `fib` 1.43 to 1.34. This applies only where the variables are global. Real code in a module or function has locals and context slots, so on the bundle-like program the gain is 1 point (0.93 to 0.92).

7b. The `__def` delete (section 7) is a bug, not a design choice.

7c. A prototype-chain cache for `GetField`. In the bundle-like profile, 17% of the instructions are the property slow paths (`get_prop_g`, `get_field_slow`, `Shapes::find`, `lookup_g`), because a get that finds the property on the prototype is not cached. This is the largest item that is left in `objects` and the bundle. It is a design of its own (invalidation of a prototype shape) and it is not part of S2.9. Built later: `docs/design-js2-proto-ic.md`.

7d. The prelude compile at start (3.2 M instructions), as in 2.5. Fix: encode the prelude at build time or keep a snapshot. Design of its own.

## 5. Recommendation

One staged design. Each step has its own check, and the next step is chosen from the histogram taken after the step before. Order and expected gain (model, whole process, scale 10 unless said; ratio = js2 / QuickJS instructions, lower is better):

| Step | What                                                            | `loop` | `loopfn` | `calls` | `fib` | `closures` | `objects` | bundle-like (scale 1) |
| ---- | --------------------------------------------------------------- | -----: | -------: | ------: | ----: | ---------: | --------: | --------------------: |
| 0    | now, variant B (variant A is worse: 8.3 for `loop`)             |   2.33 |     1.49 |    1.87 |  1.47 |       0.64 |      1.67 |                  0.94 |
| 1    | fix the `__def` delete (section 7)                              |   2.33 |     1.49 |    1.87 |  1.47 |       0.64 |      1.67 |                  0.94 |
| 2    | option 4, `regs` local (measured per op)                        |   2.14 |     1.30 |    1.77 |  1.43 |       0.62 |      1.62 |                  0.93 |
| 3    | option 7a, global cell cache                                    |   1.49 |     1.30 |    1.37 |  1.34 |       0.55 |      1.45 |                  0.92 |
| 4    | option 3, call path, and option 2, constants and jump-to-return |   1.38 |     1.12 |    1.09 |  0.94 |       0.47 |      1.33 |                  0.87 |
| 5    | option 1, top 8 pairs and 8 triples, chosen after step 4        |   0.85 |     0.73 |    0.77 |  0.78 |       0.41 |      1.21 |                  0.83 |

Step 1 changes nothing in the table because the table starts from variant B. The saving of step 1 is against variant A: `loop` from 8.3 to 2.33 at scale 10 (5270 M against 1480 M instructions).

Only step 2 is measured. Steps 3 to 5 are model values (hypothesis): the model has errors, and fusion in particular uses the 21-instruction estimate. If a fused op saves 15 instead of 21 for each removed dispatch, step 5 gives `loop` 1.00, `loopfn` 0.84, `calls` 0.87, `fib` 0.83, `objects` 1.25; if it saves 30, 0.62, 0.57, 0.64, 0.71, 1.16. A single shared fusion list removes less than section 3 shows (not measured), so read the step 5 row as a best case.

Why this order: fusion is the last step because (a) its gain per removed dispatch depends on the state cost that steps 2 to 4 change, (b) the pair list depends on the compiler and on what steps 3 and 4 remove (no `GetGlobal` pairs are left to fuse after step 3, and the `LoadI` + `Mod` pair goes in step 4), and (c) it is the only step that adds a verifier rule and a big amount of machine code. Steps 2 to 4 are small edits of 10 to 100 lines each.

What the recommendation does not give: the gate at "at or below QuickJS on every row". After step 5:

- `objects` 1.21: the rest is alloc, natives, strings and the property slow paths (option 7c).
- `webpack` 1.2 to 1.8 and `concat` 2.3 to 3.4: the setup and the compile (option 7d). The interpreter is 55 K dispatches of 52 M instructions on `webpack`.
- `fib` at scale 1: about 1.5, because of the 3.2 M of the prelude compile.

The S2 gate needs 7c and 7d, or a different gate for those rows (open question 1).

Per step checks (to write before the code, as the rules say): per-op cost table (`op_cost`) before and after, the 9 rows at scale 1 and 10 with 3 runs each, `cargo test --locked -p bare-js2 --lib`, a test that grows the stack in a callee (step 2), a shape-change test for global cells (step 3), a test for each callee shape (step 4), and the verifier tests for each fused op, including a jump onto the second instruction, which must be refused (step 5).

## 6. What fusion cannot do here

- It cannot reach `fib`, `objects` and `calls` alone (section 4, option 1, table): they have 40% to 60% of their instructions in the call path, the property path and the allocator.
- It cannot remove the setup cost. At scale 1, the 3.2 M of the prelude is 35% of `fib` and 70% of `concat`.
- It gains less on real code than on `eval_load`: 4% on the bundle-like program with 8 pairs and 8 triples (model), against 23% on `loop`. The top pairs of `eval_load` are global access pairs, which exist only because the test programs use top-level `var`.

## 7. The `__def` regression in #763

`script::load` runs the prelude, then deletes the `__def` global (`delete_prop`). A delete puts the global object in dictionary mode. The cache never stores a dictionary shape, so every `GetGlobal` and `SetGlobal` goes to the slow paths (`Table::probe`, `Shapes::find`, `lookup_g`, `get_field_slow`, `set_field_slow`, `get_prop_g`, `get_global_slow`).

Evidence: `script_run --eval` on the clean #763 build gives 519 M to 567 M instructions on `loop` at scale 1 (the count changes from run to run of one binary; hypothesis: the slow path depends on addresses, which ASLR changes), while the S2.6 note says 132.7 M. With the delete replaced by `let _ = (g, k);` (variant B), `loop` is 152 M. The same effect shows on `calls`, `fib`, `objects`, `throws` and the others (variant A against B in `docs/progress.md`). I did not fix it in this PR. It is a one-line change in `script.rs`, but the right fix is a choice for the owner (open question 2): keep the global and hide it, set the value to `undefined` instead of deleting, or let the global object leave dictionary mode after a delete of the last added property.

## 8. Open questions

1. Is the S2 gate (instructions at or below QuickJS on every `eval_load` row) still right for `concat`, `webpack` and `fib` at scale 1? The prelude compile alone is 3.2 M more than QuickJS's whole empty program. Options: a snapshot or build-time encoded prelude (option 7d) before S2 closes; or the gate on the net slope (scale 10 minus scale 1); or the gate only on rows that run at least 1 M dispatches.
2. How should the `__def` delete be fixed (section 7)? It decides the numbers of every row that uses a global. I suggest the first option: it is one line, and a hidden global is what the prelude needs.
3. Is the order in section 5 right? In particular: should the global cell cache (7a) come before fusion? It is worth 0.65 on `loop`, but only on programs with top-level `var`s. The test programs are the only ones that look like that. If real bundles are the target, then 7a is worth 1 point and 7c (the prototype cache) is worth more.
4. Overlay encoding (option 1): is a fused opcode that hides the second instruction in the code acceptable for the JIT later (S5)? A JIT reads the bytecode and would have to un-fuse. The overlay keeps the second instruction in place, so un-fusing is a write of the first opcode byte. The alternative is a separate fused code array that is made on demand; it costs memory per function.
5. May the compiler (S2.5 follow-up) emit fused ops directly, or must a pass after `encode` do it? A pass keeps the compiler simple and the pass can run with the real histogram of the program (profile-guided); the compiler knows the jump targets.
6. May the function cell grow by two words (option 3)? S1 owns the layout of the cell.
7. Do you want the deeper dispatch change (an `ip` pointer, no `Wide` test in the hot path; option 4, last paragraph)? It is estimated at 8 instructions for each dispatch (hypothesis, not measured), but it is a big edit of the largest function in the engine.
8. Is it acceptable to keep `op-count` in the tree, behind a feature, as the tool for the next histogram? It costs nothing when off (section 9).

## 9. The counting feature

`cargo build --release -p bare-js2 --features op-count --example op_count`. With the feature off, `opcount` does not exist and `run` has no call to it. The A/B of the normal build is in `docs/progress.md`. `examples/op_count.rs FILE...` prints, for each program, the dispatches, the hot ops, the hot pairs, the fusable pairs and triples, and the dispatches that greedy fusion removes (tables in section 3). `examples/op_cost.rs CASE K N` is the program for the per-op costs (section 2.3).

## 10. Decisions (owner, 2026-11-20)

- Plan: build all 5 steps of section 5 in order. Each step is one PR. Measure after each step. Fusion uses the histogram taken after steps 1 to 4.
- Question 1: the S2 gate uses the net slope (scale 10 minus scale 1). The startup cost gets its own target, a cheaper prelude.
- Question 2: keep `__def` and hide it. Do not delete it.
- Question 3: the order of section 5 stays. The global cell cache comes before fusion.
- Question 6: the function cell may grow by two words. The PR of step 4 records the peak RSS change on the bundle-like program.
- Questions 4, 5, 7 and 8 were not asked. The build follows section 5: the overlay encoding, a pass after `encode`, and `op-count` kept behind its feature. The deeper dispatch change of option 4 is not built now.

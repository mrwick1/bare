# js2 and QuickJS instruction gap on `loop`

Research and design, 2026-11-22. Base `f29a583de7acc4d0a388fdb8c14a7525b37dc93d`. The [engine decision benchmark](progress/2026-11-21-engine-decision-benchmark.md) used js2 scratch merge `05b520eb1876997a91b4e43368f33d493071f1e5`.

## 1. Measurements

The benchmark builds this source at scale 10. The last expression returns the script completion value, `63`. The body runs 2,000,000 times. Scale 1 runs 200,000 times and returns `520003`.

```js
var s = 0; for (var i = 0; i < 2000000; i++) { s = (s + i * 3) % 1000003; } s
```

I used the archived raw inputs and runners from the engine decision benchmark. The inputs come from `eval_load CASE SCALE --source`. Their SHA-256 hashes match the benchmark input manifest. The Rust runner uses `Scope::run_script`. The C runner uses `JS_Eval` with `JS_EVAL_TYPE_GLOBAL`. Both create a fresh realm and check the printed completion value. The empty source is `0`.

Machine: archMachine, Ryzen 9 5900HX. js2: rustc 1.96.0, locked release, 1 js2 codegen unit from the workspace profile, debug symbols kept. QuickJS: Bellard 2026-06-04, GCC 16.2.1, `-O2 -flto -g`. The QuickJS source archive and reference results came from `/data/claude-tmp/qjs-base/`. I built a copy in the scratch directory.

### Retired instructions

`perf stat -e instructions:u`, three runs per cell. Counts include process startup and teardown. The table reports the minimum, maximum and their difference. Every result passed its checksum.

| Engine | Input | Minimum | Maximum | Spread |
| --- | --- | ---: | ---: | ---: |
| js2 | Scale 10 | 874,803,001 | 874,804,775 | 1,774 |
| QuickJS | Scale 10 | 635,115,543 | 635,116,084 | 541 |
| js2 | Scale 1 | 88,202,810 | 88,203,128 | 318 |
| QuickJS | Scale 1 | 64,511,162 | 64,511,690 | 528 |
| js2 | Empty | 724,892 | 725,552 | 660 |
| QuickJS | Empty | 1,052,554 | 1,052,773 | 219 |

The published minima are 868.80 M and 635.12 M. This run is +0.69% and within 0.01%, respectively. The js2 rerun uses the worktree commit above.

### Callgrind

I collected one symbolized trace per engine and source with `--dump-instr=yes --collect-jumps=yes`. I subtracted each engine's own empty count. The table includes compile and teardown work that the empty run does not share with the loop. It also shows the steady-state counts from the hot machine instructions alone.

| Engine | Gross loop `Ir` | Empty `Ir` | Net `Ir` | Net per turn | Hot per turn |
| --- | ---: | ---: | ---: | ---: | ---: |
| js2 | 874,838,591 | 760,773 | 874,077,818 | 437.039 | 437 |
| QuickJS | 635,135,183 | 1,071,624 | 634,063,559 | 317.032 | 317 |

The steady paths account for 874 M and 634 M instructions. The main interpreters account for almost all of both profiles. The net hardware counts are 437.039 and 317.031 instructions per turn. The hardware difference is 120.008 instructions per turn, or 240.015 M at scale 10.

## 2. Root cause

### Executed bytecodes

I dumped the js2 compiler output in script completion mode. I used `bc_hist -v` for the local function. I used a separate QuickJS build with `DUMP_BYTECODE=1`. Both disassemblies show one fixed path for each successful turn. I count the final failed test outside the turn. The measurement builds have no bytecode tracing.

| Engine | Ops per turn | Encoded bytes per turn | Executed order |
| --- | ---: | ---: | --- |
| js2 | 12 | 56 | `GetGlobal i; JnLtI; GetGlobal s; GetGlobal i; MulI; Add; ModX; SetGlobal s; GetGlobal i; Inc; SetGlobal i; JmpBack` |
| QuickJS | 19 | 41 | `get_var i; push_i32; lt; if_false8; get_var s; get_var i; push_3; mul; add; push_i32; mod; dup; put_var s; put_loc0; get_var i; post_inc; put_var i; drop; goto8` |

js2 uses 14 32-bit words. `JnLtI` and `ModX` each carry an extra immediate word. None of the 12 ops needs `Wide`. QuickJS uses compact stack ops but needs separate pushes, a Boolean branch, a completion-value copy and an old-value drop. js2 writes the remainder directly to the script completion register. The final load of `s` replaces that value in both engines.

A function with two uncaptured `var` locals runs this loop in six js2 ops and 13 QuickJS ops. js2 uses the counter register as the source of the compare and multiply and the destination of `Inc`. It writes `ModX` straight to the accumulator variable. QuickJS copies local values to the operand stack, writes the result with `put_loc0`, and increments with `inc_loc`.

### Instruction cost by operation

I matched the Callgrind addresses to `objdump -d -Mintel` and counted their dynamic executions. The table groups machine instructions by bytecode operation, with dispatch included in each row. It includes only the steady hot path. Small setup work and the periodic interrupt reset fall outside these integer counts.

| Work per turn | js2 | QuickJS | js2 extra |
| --- | ---: | ---: | ---: |
| Four global reads | 132 | 80 | 52 |
| Two global writes | 68 | 46 | 22 |
| Bound, compare, conditional branch and back edge | 55 | 53 | 2 |
| Multiply, including its constant | 49 | 34 | 15 |
| Add | 45 | 23 | 22 |
| Remainder, including its constant | 45 | 31 | 14 |
| Increment, including dropping the old value | 43 | 25 | 18 |
| Copy or store the completion value outside arithmetic and globals | 0 | 25 | -25 |
| **Total** | **437** | **317** | **120** |

The six global ops cost js2 200 instructions per turn. They account for 74 of the 120 extra instructions after the direct comparison with QuickJS. This is the largest group. QuickJS uses linked binding cells. js2 uses 16-byte per-site feedback entries. Each hit reloads the feedback, decodes an index, checks its bounds, compares the global mutation tag and loads the cached slot address. It then copies one 8-byte value. The x86-64 code loads the slot address with one unaligned pointer load from the packed cache. The warm trace takes the hit path almost every time. Lookup by name is confined to misses.

The common js2 dispatcher at `0x157be0` and `0x157c70` in the measured binary costs 11 instructions. It reloads `pc`, fetches a 32-bit word, checks `Wide`, spills the word-stream base and indexes a shared jump table. Every hot handler also jumps back to this common entry. Those 12 mandatory instructions per dispatch account for 144 instructions per turn. The count excludes the varied arm-tail state spills and reloads. QuickJS runs 17 dispatch tails of five instructions and two of four, for 93 per turn. This conservative dispatch split explains 51 of the 120 extra instructions. These figures overlap the operation rows above.

Most of the arithmetic difference comes from surrounding instructions. The machine code extracts register indices, checks and rebuilds boxed tags, and transfers results through the register file between ops. Some handlers spill the frame or reload the code and feedback. The paired generic/immediate arms for `Add` and `MulI` also test the selected form again. Both engines use one `idiv` per turn. js2 checks a general signed divisor and checks the dividend sign for `-0`. The js2 compare uses a three-way integer order followed by a branch on that order. QuickJS produces a Boolean directly. These details are visible in the machine code. Hypothesis: a direct comparison plus tighter tag checks will help outside the fused path. The data do not support a representation change as the first step.

### Controls

I changed only the source for these controls. The original source and binaries remained available. Each run returned `63`. The function control uses a function that returns `0` for its empty baseline. The split control uses the original empty script.

| Control | js2 perf min [spread], M | QuickJS perf min [spread], M | js2 / QuickJS net Callgrind instructions per turn |
| --- | ---: | ---: | ---: |
| Both variables are script globals | 874.803001 [0.001774] | 635.115543 [0.000541] | 437.039 / 317.032 |
| Change only `var i` in the loop head to `let i` | 608.786130 [0.000759] | 621.107543 [0.000372] | 304.031 / 310.028 |
| Move the same arithmetic into a function with two `var` locals | 474.812980 [0.000131] | 429.139704 [0.000112] | 237.031 / 214.036 |

The split removes three `GetGlobal i` ops and one `SetGlobal i` op from js2. It keeps the compare, arithmetic, global `s` and the same 2 M turns. QuickJS swaps the four global references for `get_loc_check` and `put_loc_check`. It still dispatches 19 ops. The net cost drops by 133.008 instructions per turn in js2 and 7.004 in QuickJS. That single change reverses the gap for this source. It shows the cost of issuing separate global-counter ops. The function control still leaves a 23.0 instruction gap per turn, and also changes QuickJS's bytecode for completion and increment. It does not isolate one guard or the exact benefit of changing the global object representation.

### Same-source dispatch experiment

I changed the back edge of the exact scale-10 source in a temporary build. The merged handler charges fuel, checks the cached `i` binding and int tag, and compares the payload directly. It keeps the first test and falls back to the original header on a miss. The script and observable global writes stay the same. A switch selects either path in the same release binary.

| Path | Perf minimum | Perf maximum | Callgrind `Ir` | Shared dispatch entries |
| --- | ---: | ---: | ---: | ---: |
| Original bytecode | 874,804,266 | 874,805,046 | 874,839,711 | 24,000,012 |
| Fused back edge and header | 810,804,296 | 810,804,632 | 810,840,292 | 20,000,014 |

All three hardware runs on each path returned `63`. The minimum fell by 64.000 M, or 32 instructions per turn. Callgrind confirms two fewer steady dispatches per turn. The saved work includes dispatch, a temporary reload and the three-way compare sequence. The experiment keeps the global cache and fuel check. It demonstrates a 27% reduction of the measured gap through one fused path. I reverted the patch.

## 3. QuickJS design

QuickJS uses a stack bytecode and GCC computed goto in `JS_CallInternal`. The opcode is one byte. The hot dispatch loads the next byte, advances the program counter, and jumps through a table. In this binary that sequence costs four or five machine instructions. It shares some tails. This measurement counts retired instructions. It does not measure indirect-branch prediction.

`resolve_scope_var` lowers these script names to indexed `get_var` and `put_var` ops. `js_closure_define_global_var` links the script and the global property to the same `JSVarRef`. The reference owns, or points to, the live mutable value. `get_var` checks for the uninitialized marker. `put_var` tests for the marker and const state. Property updates can change or detach the reference when the descriptor changes. Slow paths then use normal property semantics. These ops avoid an atom lookup and a global shape epoch comparison on the bound path. They still use pointer loads and check whether the old or copied value needs reference counting. Function locals use fixed frame slots and short opcodes such as `get_loc1`, `put_loc0` and `inc_loc`.

On this 64-bit build, `quickjs.h` defines a 16-byte `JSValue` with a payload word and a separate tag word. `JS_TAG_INT` is zero. A bitwise OR can test two tags together. A nonnegative tag skips reference count updates. js2 uses an 8-byte NaN box with the int tag `0xFFF9` in the top 16 bits and the int32 payload in the low 32 bits. js2 copies the word without reference counting. It extracts or combines tag bits for arithmetic. Its local operands are fixed offsets from the current frame register base. Neither engine allocates a Number on this hot path.

The multiply result is at most 5,999,997 and the sum at most 6,999,999. The counter, bound and remainder fit in signed int32. Both engines take the integer paths for every hot operation. Both retain overflow checks and handle signed zero. QuickJS uses checked widening for addition and multiplication, and uses signed division for a nonnegative dividend and a positive divisor. It uses slow arithmetic for other remainder inputs. js2 uses checked int32 arithmetic, a signed remainder with zero and `-1` guards, and boxes `-0` when required. js2 has inline double or mixed-number paths and falls back for coercion or BigInt. QuickJS uses the slow comparison helper for non-int operands. The benchmark takes neither engine's double path.

QuickJS materializes a Boolean for `lt`. `if_false8` pops it and polls the interrupt counter. `goto8` polls it again. The two counter sequences cost ten instructions per turn in the profiled binary. The counter resets every 10,000 polls. js2 fuses the compare and conditional branch. Its `JmpBack` decrements the fuel budget once per turn. The hot paths have no stack growth, allocation or collection. QuickJS checks stack space at frame entry and can collect on allocation. js2 checks capacity at frame entry and checks the GC trigger on allocating or slow paths. The verifier bounds the register and word accesses on this path.

For dispatch alternatives and the earlier js2 prototype, see [the dispatch design](design-js2-dispatch.md). It compares V8 Ignition's generated handlers and JSC LLInt's platform-specific handlers with the Rust match loop. The comparison here uses the measured Bellard engine.

## 4. Designs for js2

### A. Fuse adjacent dispatches across verified operations

Keep the 32-bit stream, the register frame and the boxed values. Add a small set of fused opcodes over the verified stream. A fused back edge can use the existing signed offset word and read the already verified target operands. Use the original bytecode words for the slow path. A fused read or arithmetic op can use the first word as a marker and keep the remaining operand and fallback words. The verifier must check the original instruction sequence and register dataflow. Retain the original PC of each operation that can run the slow path.

Use one shared fast path per pattern. Check all required global caches and primitive tags before any effect. Avoid boxing intermediates that are dead at the end of the group. Do the write only after the read, arithmetic and overflow checks succeed. Then resume at the next original PC. If a guard fails, enter the original sequence before any side effect. Preserve each jump target, handler entry and GC root map. Keep one fuel debit at the original back edge. Do not reduce the fuel charge.

The compile pass is O(bytecode words) and the fast path is O(1) per group. The marker approach reuses the existing words. A small fixed number of extra handlers grows interpreter text. The loop saves six dispatches on turns that use all three patterns below. It also avoids temporary register traffic and redundant tag or cache checks. Hypothesis: the combined scale-10 count is 600–650 M. Size estimate: three small-to-medium PRs. Startup can regress from the scan or handler text. Closures, throws and browser code can regress from changes in the hot match's register allocation. Select patterns from their traces and test each row before combining changes.

### B. Use stable binding cells for eligible global names

Link declared globals to heap cells that contain a boxed value and a validity or descriptor state. Store one binding index in each referencing instruction. Use a frame or code binding table of cell pointers. Global-object reads and writes use the same cell. Descriptor changes, lexical shadowing and delete must update or detach the cell. Missing and accessor cases use the existing property path. Other globals can keep their slot cache. The binding table keeps cells live while scripts or closures can still use them.

Linking costs O(distinct bound names) plus the property lookups at link time. Access is O(1). Expect an 8-byte value plus status, alignment and heap header per linked name and a pointer per table entry. The bytecode instruction count stays at 12 per turn. The profile places 116 instructions per turn in the body of the six js2 cache-hit handlers before dispatch tails. QuickJS spends 96 on the body of its six cell handlers. Hypothesis: saving 12–30 instructions per turn with stable cells puts the row at 815–850 M. This estimate does not assume that all mutation checks can disappear. Size estimate: several PRs across binding resolution, property descriptors, heap tracing and the interpreter. Allocation and linking can raise startup and RSS. Captured globals, realm lifetime, throws during property access and browser host globals need coverage. This design leaves the dispatch difference in place.

### C. Change the dispatch architecture

Keep the existing word stream and shared slow interpreter. Split hot handlers and pass the bytecode cursor, register base and interpreter state explicitly. Tail-dispatch through a fixed handler table while preserving constant native stack depth. Any target or build mode without a guaranteed tail transfer needs the existing match interpreter. Use the [dispatch design](design-js2-dispatch.md) for prototype approaches and its compiler and target restrictions.

Setup is O(1), with O(1) dispatch per op. Bytecode and value storage stay the same. The extra table holds one address per opcode. The hot handlers change native text layout. The existing toy saved 6–9 instructions per op. Applying that saving to 12 ops gives 659–731 M at scale 10. That is a hypothesis from a toy on another loop. It does not account for global feedback, state transfers or fallback in this VM. Size estimate: medium to large. Debug, Miri, release and each supported target must verify bounded stack use. Startup and browser journeys need cold-path tests. Closures and throws need entry, return, reentry and unwind tests. This experiment can serve several benchmark rows. It changes every dispatch, so its regression risk is wider than option A.

## 5. Recommendation and implementation plan

Build option A in this order. Keep the general interpreter for guard failures. Reserve the binding-cell and dispatch changes for separate experiments. Each count below is the whole scale-10 process and uses 875 M as the local baseline. Projections are hypotheses.

| PR | Change and files | Correctness and cost gate | Expected count |
| --- | --- | --- | ---: |
| 1 | Fuse a verified `JmpBack` to a narrow `GetGlobal; JnLtI` loop header. Keep the original header for first entry and misses. Match it in the compiler after control flow is known. Add the encoding and verifier rules in `compile.rs`, `bytecode.rs`, `verify.rs` and the handler in `interp.rs`. | Check zero and one turn, continue and labels, equal and NaN bounds, an accessor or invalidated cache, and exact fuel exhaustion with the post-update globals visible. Compare the same loop's result and two fewer steady dispatches. | 805–820 M (spike: 811 M) |
| 2 | Fuse `GetGlobal; Inc; SetGlobal` when the sites refer to the same ordinary writable binding and the operand is an int32 below `INT32_MAX`. Preserve the old path on a miss and materialize any live register. Touch `compile.rs`, `bytecode.rs`, `verify.rs`, `interp.rs` and reuse the cache logic in `interp_rt.rs`. | Compare prefix and unused postfix updates, overflow and BigInt, strict and sloppy writes, accessors, deletion, lexical shadowing and forced collection. Check that a slow coercion or setter runs once. Target two additional dispatches removed per turn. | 700–750 M |
| 3 | Fuse hot integer producer/consumer sequences. Start with `MulI; Add; ModX` and fall back before the first op if tags or the arithmetic guards fail. Keep live intermediates. Consider fusing the adjacent global load or store only if the measured row still exceeds 635 M. Touch `compile.rs`, `bytecode.rs`, `verify.rs` and `interp.rs`. | Compare string addition, BigInt, object coercion order, int overflow, `NaN`, fractions, negative zero, zero and negative divisors. Check stale roots and original exception locations on the fallback. Count the removed dispatches during the test. | 600–650 M, about 635 M target |

The second PR targets 30–55 saved instructions per turn. It removes two dispatches and combines register copies and checks. The third PR targets about 45 more instructions per turn from dispatch and unboxed intermediates. Recalculate these estimates from each PR profile.

Each PR uses narrow compiler, verifier, interpreter and script tests via `t`, as applicable. Use the relevant `interp_load` rows and `cost_matrix.sh -g` for touched rows. Measure three fresh processes with the checked `loop` source, plus empty, local, calls, closures, throws, objects and webpack controls. Apply the repo's +0.5% cost rule under both allocators or at `codegen-units=1`. Check verifier rejection of truncated and malformed encodings. Recheck the same instruction buckets after each change. Run the paired browser journeys and global/local loop ramps from the decision benchmark when the complete design is ready. Record instructions and peak RSS in a separate progress report.

## 6. Overlap with other work

- The calls and webpack cases can share the dispatch measurements and the bounded fusion mechanism. Use their traces to choose a shared set of profitable sequences. Keep distinct measurements for call, parse and compile costs. Objects can use the same dispatch measurements. Its hot object and index accesses have different guards.
- The [dispatch design](design-js2-dispatch.md) already describes per-handler dispatch. Reuse its platform and codegen checks. Measure fusion and dispatch changes separately before combining them.
- The adaptive GC trigger changes allocation-path work. This loop runs no allocation path per turn. Keep the allocation and root-map tests in the fusion gates. PRs #908 and #909 remove integer-string atom creation on index reads. This source does no index reads.
- The engine decision benchmark already includes browser journeys and global and local browser loop ramps. Use that method on the final implementation to measure the browser effect.

Research artifacts and commands are in `/data/bare-tmp/r22/gap-loop/` on archMachine. The two symbolized launchers, raw inputs, hardware samples, disassemblies, Callgrind files and reverted spike patch are there. Unverified here: production fusion correctness, the full projected saving, wall time, peak RSS, browser journeys, and other platforms.

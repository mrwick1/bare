# js2 and QuickJS instruction gap on calls

## 1. Measurements

The [engine decision benchmark](progress/2026-11-21-engine-decision-benchmark.md) measured 689.85 M instructions for js2 and 594.15 M for the QuickJS direct launcher. This run reproduces the gap. The js2 count is 0.58% above the reference. The QuickJS count differs by less than 0.01%.

I used archMachine, AMD Ryzen 9 5900HX, Rust 1.96.0 and GCC 16.2.1. The js2 tree was `f29a583de7acc4d0a388fdb8c14a7525b37dc93d`. It differs from the scratch merge in the decision benchmark. I used a locked release build with `CARGO_BUILD_JOBS=3`, a private target, debug symbols and no strip. The js2 launcher used `Scope::run_script`. The QuickJS launcher used `JS_Eval` with `JS_EVAL_TYPE_GLOBAL`. It linked the Bellard 2026-06-04 engine at `-O2` with LTO and debug symbols. The QuickJS archive hash was `b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a`. I built from a copy of the archive. [QuickJS release archive index](https://bellard.org/quickjs/)

Both launchers read the raw input from the decision benchmark. Its SHA-256 at scale 10 was `4aca5300500f34ebc46c5af388f15500d505cedaa9eb98461dc99964d1e8cb60`. Each scale-10 run returned `35741`. The script makes one call to `f(s, i)` on each of 1,000,000 turns. The call takes two small integers. The body computes `(a * 31 + b) % 65521`. The accumulating result, the counter and the function binding are script globals. Scale 1 makes 100,000 calls. The empty case evaluates `0`.

### Hardware instructions

Each row ran in three fresh processes with `perf stat -e instructions:u` and at least 99% counter coverage. The allocator was the default system allocator. The table lists raw counts in run order. The spread is the maximum minus the minimum.

| Case | Engine | Samples | Spread |
|---|---|---|---:|
| `calls_10` | js2 | 693,852,369 / 693,852,693 / 693,852,578 | 324 |
| `calls_10` | QuickJS | 594,147,945 / 594,148,054 / 594,148,117 | 172 |
| `calls_1` | js2 | 70,152,094 / 70,152,315 / 70,152,446 | 352 |
| `calls_1` | QuickJS | 60,444,814 / 60,444,679 / 60,444,852 | 173 |
| Empty | js2 | 727,046 / 726,787 / 727,448 | 661 |
| Empty | QuickJS | 1,053,350 / 1,053,069 / 1,052,986 | 364 |

After subtracting the minimum empty count, scale 10 takes 693.13 instructions per turn on js2 and 593.09 on QuickJS. Each turn contains one source call and one source return. The numbers per turn also include loop control, argument evaluation and the callee's arithmetic. Scale 1 gives 694.25 and 593.92 instructions per turn after the same subtraction.

### Callgrind instructions

I ran Valgrind 3.25.1 with `--tool=callgrind --dump-instr=yes` on each launcher and the same input. I summed exclusive instruction counts from the executed native address ranges. I checked the ranges against `objdump` and the debug source locations. I charged js2's shared dispatcher to the instruction it enters. QuickJS's computed-goto dispatch usually sits at the end of an opcode handler. This convention can move a few instructions across adjacent boundaries.

| Case | js2 Ir | QuickJS Ir |
|---|---:|---:|
| `calls_10` | 693,887,643 | 594,167,033 |
| Empty | 762,258 | 1,071,900 |
| Net | 693,125,385 | 593,095,133 |

The following costs are instructions per complete turn. A turn is also the unit for one paired call and return. Cold activity and the final failed loop test account for about 0.1 instruction per turn after empty subtraction.

| Work per turn | js2 | QuickJS | Difference |
|---|---:|---:|---:|
| Five global reads, including the load of `f` | 165 | 101 | +64 |
| Two global writes | 68 | 46 | +22 |
| Call instruction and frame entry, after argument evaluation | 167 | 132 | +35 |
| Return, frame teardown and delivery to the caller | 56 | 90 | −34 |
| Callee body before return | 139 | 121 | +18 |
| Other loop operations | 98 | 103 | −5 |
| **Total** | **693** | **593** | **+100** |

### Bytecodes per turn

I disassembled js2 with the compiler used by `bc_hist` and `ScriptMode { value: true, .. }` for the script. I verified each code and counted the instructions along the common loop path. For QuickJS I used a separate build with `DUMP_BYTECODE=1`. The dump build did not supply timings.

- js2 executes **11 caller + 4 callee = 15** instructions. The caller runs five `GetGlobal`, two `SetGlobal`, `JnLtI`, `Inc`, `JmpBack` and `CallU`. The callee runs `MulI`, `Add`, `ModX` and `Ret`. Extra words in `JnLtI` and `ModX` are operands. A lazy stub runs once.
- QuickJS executes **16 caller + 8 callee = 24** instructions. The caller runs `get_var` five times and `put_var` twice. The other instructions are `push_i32`, `lt`, `if_false8`, `call2`, `dup`, `put_loc0`, `post_inc`, `drop` and `goto8`. The callee runs `get_arg0`, `push_i8`, `mul`, `get_arg1`, `add`, `push_i32`, `mod` and `return`.

## 2. Root cause and controls

**Script-global traffic accounts for 86 of the 100 extra instructions per turn.** On each turn js2 reads `i` for the test, reads `f`, `s` and `i` for the call, and reads `i` again for the update. It writes `s` and `i`. Each of those seven instructions goes through the shared interpreter dispatch. Each hit bounds-checks the feedback cache, indexes a 16-byte entry, checks the realm's global mutation tag, and loads or stores a value through the slot address. The sampled global read and write handlers cost about 33 and 34 instructions each. QuickJS uses variable reference cells and costs about 20 and 23. Neither hot path does a name lookup through a property table. The per-site dispatch, cache index and tag checks are visible in the js2 disassembly. [Global handlers](../crates/js2/src/interp.rs#L1951) [Cache fill](../crates/js2/src/interp_rt.rs#L1465)

The call and return together cost about the same. js2 spends more on entry and saves a similar amount on return. A plain function's object tag and shape select the fast path in `Realm::function`. It loads the code index and context from inline slots. That work and the caller operand decode cost about 42 instructions, separate from the 33 for looking up `f` in the global. `enter!` finds the code by index and checks the end of the frame against the value stack length. The steady-state capacity check and branch cost about five instructions. There is no stack growth in the hot path. It writes ten 8-byte record words and copies the two 8-byte argument slots to parameter registers 0 and 1. The generic copy loop and its exits account for about 27 instructions. No fill runs for missing parameters. Feedback and code reloads run on both transfers. The current record provides the PCs for GC and resumption, caller code, result slot and flags, context depth, callee, context, receiver, `new.target` and arguments. [Frame entry and return](../crates/js2/src/interp.rs#L937) [Callee shape test](../crates/js2/src/interp_rt.rs#L1829)

The argument expressions already write into consecutive caller registers. The copy is from that window to the callee. The interpreter passes `undefined` as `this` and stores it in the record. No `LoadThisS` runs. `CF_ARGS` is clear, so the call does not run `new_args` or build a rest array. `Ret` restores the caller's frame pointer, PC and code. It writes the result to the destination register. It drops the frame by moving the pointer. It does not clear the registers or release values one by one. The rest of the gap comes mainly from the three arithmetic handlers. Operand decode, NaN-box tag tests, the common dispatch and the remainder path cost 139 instructions in total versus QuickJS's 121. Protect overflow, signed zero and coercion when changing these paths. [Call compilation](../crates/js2/src/compile_expr.rs#L1029) [Argument object](../crates/js2/src/lazy.rs#L326)

### Counterfactual runs

These input edits kept the original engines. I profiled one run of each with Callgrind and subtracted each empty count. The instruction costs include all work per turn. The first three controls below return the original checksum.

| Control | js2 / turn | QuickJS / turn | Gap / turn |
|---|---:|---:|---:|
| Original | 693.13 | 593.10 | 100.03 |
| Inline the arithmetic and keep the global loop | 437.10 | 316.10 | 121.00 |
| Put `s` and `i` inside an IIFE and keep the call to global `f` | 535.14 | 490.10 | 45.04 |
| Local variables and inline arithmetic | 237.11 | 213.11 | 24.00 |
| Original caller with `f(a, b) { return a; }`, result `1` | 554.11 | 492.07 | 62.04 |

Inlining removes the frame and its callee lookup on both engines. It makes the gap **21.0 instructions larger**. The run with local loop variables keeps the two-argument call and removes **55.0 instructions of relative cost**. The local version also changes completion tracking and stages arguments from registers. The address split above separates those differences from the exact seven global operations in the original run. The identity control removes the three arithmetic instructions but also changes the callee's register/operand-stack needs. Its delta is a combined body and layout effect.

### Cache bounds experiment

I changed only `GetGlobal`, `SetGlobal` and `SetGlobalS` in a local build. The handlers used the site index without repeating the cache slice bounds check. They still checked the global mutation tag. The raw script and checksum stayed the same. All samples were fresh processes with `perf stat` and at least 99% coverage.

| Allocator | Baseline minimum | Experiment samples | Spread | Change |
|---|---:|---|---:|---:|
| glibc | 693,852,369 | 679,849,918 / 679,849,612 / 679,849,939 | 327 | −14,002,757 |
| jemalloc | 697,253,025 | 683,254,325 / 683,254,818 / 683,254,007 | 811 | −13,999,018 |

The patch saves about two instructions per hot global access after register allocation and layout changes. It prices part of the 86-instruction global difference. Callgrind counted 679,886,239 total instructions on the glibc experiment, about 14.00 M below the baseline profile. I reverted the patch. The verifier checks the site number, and `Feedback::init` sizes the cache array from the code's sites. A shipping change must prove this invariant across all entry points and lazy replacement.

### Why the other call shapes differ

- `fib(23)` makes 92,735 entries, including the initial call. It has one argument per call. It uses a global function name at the recursive sites. It has no hot global counter or accumulator writes. js2 needs three instructions per base case and nine per recursive case. QuickJS needs six and sixteen. The js2 compare-and-branch and immediate subtracts avoid dispatches. Its trace also avoids reference counting for the recursive callee value. The measured totals are 34.53 M and 35.37 M. The gap on this row does not rank two-argument frame setup in isolation.
- `closures_10` makes 50,000 one-argument factory calls, 50,000 zero-argument indexed method calls and 50,000 calls to `Array.prototype.push`. It repeatedly makes and reads closures. It does not repeat the same two-argument math leaf at one site. `mk` puts its captured local in a js2 context. QuickJS gives the local a variable reference and closes that reference on return. The zero-argument js2 call copies no values. Both engines still use script globals in the outer loops. The measured totals are 207.29 M and 277.68 M. The QuickJS closure profile accounts for 80.18 M exclusive instructions in `mark_children`, `js_bytecode_function_mark`, `JS_RunGCInternal` and `gc_decref_child` together. The original calls path has no hot phase like this. Hypothesis: closure allocation, reference counts, collection and array access together explain the closure lead. I did not split the full closure workload by phase.

## 3. QuickJS call path

QuickJS binds the three declared names to global variable references when it creates the script closure. The global object stores references to the same cells. The steady-state `get_var` and `put_var` handlers use `var_refs[idx]->pvalue`. They test for an uninitialized value. Writes also test the const flag. The uninitialized path can look up a property on the global object. The hot path for this script does not search a shape or hash table. The script still reads the current value of `f` at each call. See `add_global_variables`, `js_closure_define_global_var` and `JS_CallInternal` in the pinned source.

`call2` finds the arguments on the operand stack and saves the return PC. It calls `JS_CallInternal` with both `this` and `new_target` set to `undefined`. Its flags are zero. The callee checks the interrupt counter, the value tag and the function class. It takes the bytecode pointer from the function object and checks the native stack limit before `alloca`. The frame links to the previous frame. The realm and bytecode come from the callee. The native C call saves the machine return address.

For this call, `argc == b->arg_count == 2`. QuickJS points `arg_buf` at the caller's operand stack. It can write those slots because the call consumes the operands. It allocates two 16-byte operand slots for `f` on the native stack in this build. `f` has zero locals and zero variable references. The loops to initialize locals and variable references run zero times. If arguments are missing or the caller requests a copy, QuickJS copies the available formal arguments and fills the missing slots with `undefined`. Extra arguments remain accessible by the original argument vector. The compiler emits `push_this` when the function reads `this`. In sloppy code it uses the callee's global object for a null or undefined receiver and boxes other primitives. Strict code takes the incoming value. The compiler emits `special_object` to build mapped or unmapped arguments on demand from the argument vector. The body of `f` has neither opcode.

`return` pops the result. The cleanup loops test for live variable references and remaining values. This callee has none. The C return restores the previous interpreter invocation. The caller tests for exceptions, releases the callee and the two operands, and pushes the result. Only the callee value has a reference count in this case. The integer operands still incur the tag tests.

V8 removed a related source of argument copies by putting the actual count in its callee frame and using fixed offsets for arguments. Its caller pads missing parameters. This is evidence for arranging the calling convention around the argument slots. The V8 change removed its argument adaptor frame. The js2 problem is the copy into a register interpreter frame. [V8 engineering note](https://v8.dev/blog/adaptor-frame)

## 4. Designs for js2

All counts below are gross predictions for this machine and input. Only the cache bounds experiment has a measured implementation.

### A. Combine adjacent global operations

Keep the realm's slots and the current cache entries. Generate small guarded paths for frequent adjacent operations. Start with a global integer update, a global read followed by an integer branch, and a run of two or three own-data global reads. The compiler can prove the update reads and writes the same name. The runtime must prove that both feedback entries are current and point to the same writable data slot. The other paths also require current feedback. Read the mutation tag once for the group. A typed operation checks the operand before any store. Use the generic instruction sequence for a cold site, a non-integer, overflow, a stale entry or an accessor. Preserve the order of property resolution and any coercions.

Keep the original bytecode and its distinct PCs for the fallback. A fast instruction before it can identify the rest of the sequence by fixed offsets or a compact descriptor. On a hit it writes the same live registers and jumps past the sequence. On a miss it falls through at the first original PC. The verifier must check both control-flow paths, every slot operand, the register effects, and the branch destination. Do not enter a fused group halfway across a safepoint or a handler boundary. On a hit these operations cannot allocate or re-enter. On a miss the old instructions retain their root maps, source locations and exception PCs.

The hit cost is O(k) for a group of k operations, with k bounded by three. It has one dispatch and no temporary heap allocation. The cache entries remain 16 bytes each. The code pays a few extra words per matched sequence and can keep the original site numbering. The three patterns plus the measured cache change target about **592 M** together. Hypothesis: code layout can move the result by several million instructions. Large cold-path duplication can raise startup size and perturb the dispatch loop for `closures`, `throws` and `objects`. Browser risks are stale pointers, exception order and hidden global-property effects. Keep hit code small and use the existing slow handlers. Estimate three small feature patches after the bounds patch, about 250–400 lines total plus tests.

### B. Place eligible globals in stable binding cells

Give common script-global bindings a stable realm-owned cell. Store the cell reference in the global property and in the code's resolved binding table. Store the value and a state for access semantics in the cell. A normal read or write uses the cell. Redefinition, lexical shadowing, deletion or invalidation updates its state or redirects access. Native and user property operations go through the same binding. The slow path implements the current property semantics for uncommon states. A call reads the cell at the original evaluation point so reassigning `f` still works.

Steady-state access is O(1). It removes repeated validation of pointers into moving slot storage. It adds a per-name side table, an indirection from the global property and at least one value and state word per adopted binding. The GC must mark cells and their values for every reachable holder. The version, state or equivalent check remains necessary for globals that can change kind. A target of **624–654 M** assumes a saving of 6–10 instructions on each of the seven accesses. It keeps the seven dispatches. Startup allocates and binds cells. Closures can retain the cells, and exceptions can observe redefinition. Browser host globals and cross-script shadowing add risk. This is several PRs across `object.rs`, `shapes.rs`, `interp_rt.rs`, `api_script.rs`, `interp.rs` and the collector. Measure it if guarded sequences leave a material gap across nonadjacent accesses.

### C. Reuse the caller's argument slots

Place a consumable argument window at the top of the caller's live range. Use it as the callee's parameter registers for an eligible ordinary call. Reserve room before that window for a frame link. Store the caller's frame offset explicitly, since its full register count no longer locates the previous frame. Include the callee and live receiver in the record before overwriting any caller temporaries. A verifier must prove that a callee write cannot overwrite a live caller value or a caller root. Use the current path for arity mismatch, native reentry, slow callable kinds and calls that need all arguments. Keep the existing stack limit check.

This changes exact-arity setup from O(argc) copy work to O(1) metadata work. The values still occupy contiguous slots. A frame may need a saved offset and more register-window space. The copy loop costs about 27 instructions in this case. An estimate of **670–684 M** allows for the additional checks and link data. It leaves global traffic intact. Zero-argument closure calls have little copy work to remove. More work in the call opcode could make startup or those closures worse. Throws, generators, GC roots, stack growth and browser reentry depend on frame traversal. This is several hundred lines across `compile_expr.rs`, `bytecode.rs`, `verify.rs`, `interp.rs` and the suspension paths. Revisit it only with a workload whose instruction split shows call entry as a large net cost.

## 5. Recommendation and implementation plan

Start with design A. Keep frame layout and the calling convention during this work. Price each step on the exact script-completion input. Use the same frozen input and checksum for all comparisons. Measure the corresponding `interp_load` rows as controls. The hand-written `interp_load calls` uses locals and does not measure this global path.

For each PR run the narrow tests with `t` and typecheck `bare-js2`. Measure the affected micro rows and `tools/js/cost_matrix.sh -g` rows at the repository's +0.5% rule. Use both allocators or the repository's `codegen-units=1` control as applicable. Measure `calls_1`, `calls_10`, empty, local call, `loop`, `loopfn`, `fib`, `closures`, `throws` and `objects` where the touched path applies. Use `webpack` when changing compiler output or dispatch layout. For the final step, replay the affected browser controls with the same source or responses. Include top-level and function-local loop ramps, one script-heavy page and the typing journey. Compare completion, diagnostics, instructions and peak RSS.

| PR | Work and files | Targeted tests and cost gate | Expected `calls_10` |
|---|---|---|---:|
| 1 | Use the site range already proved by the verifier. Check the code-to-feedback invariant at initialization and replacement. Touch `interp.rs` and `interp_rt.rs`. Add `verify_global_site_tests.rs` for the verifier cases. | Reject an invalid site in narrow and wide bytecode. Exercise a lazy callee, a nested run, and a global cache refill after mutation. Require about 14 M fewer instructions and a narrow micro gate. | **680 M**, measured probe |
| 2 | Emit a guarded fast instruction for a discarded global integer increment. Keep the original three instructions for failure. Touch `compile_expr.rs`, `compile_stmt.rs` if the effect-only context needs it, `bytecode.rs`, `verify.rs` and `interp.rs`. Add `interp_global_update_tests.rs`. | Check global `i++` results, getter/setter order and counts, mutation after warmup, nonwritable strict/sloppy behavior, lexical shadowing, string/BigInt coercion and integer overflow. Exercise GC and a thrown getter on fallback. Target 35–45 M fewer than PR 1. | **639 M**, estimate |
| 3 | Combine an own-data global load with the integer comparison branch. Use the direct signed comparison on the integer hit. Keep the original read and comparison on miss. Touch `compile_expr.rs`, `bytecode.rs`, `verify.rs` and `interp.rs`. Add `interp_global_branch_tests.rs`. | Check both branch directions and bound extremes, NaN, coercions with side effects, getters that re-enter and throw, and backedge fuel. Require a gain on `calls` and check the `loop` row. Target 18–26 M fewer than PR 2. | **617 M**, estimate |
| 4 | Batch adjacent global data reads into the call input window. Read the current tag once and validate all entries before using this path. Keep the original reads and `CallU` on fallback. Touch `compile_expr.rs`, `bytecode.rs`, `verify.rs` and `interp.rs`. Add `interp_global_read_batch_tests.rs`. | Check left-to-right effects when the first getter changes a later binding, replacement of `f` during argument evaluation, exceptions during argument loads, reentry and a moving stack. Run the closure, webpack and browser gates. Target another 20–32 M and check the QuickJS reference. | **592 M**, estimate |

The projected range is about **580–608 M**, against QuickJS at **594.15 M**. If a step moves hot code across the compiler's register-allocation threshold, measure the native instruction split again before adding another opcode. Keep a step only if it improves the shared rows or removes enough repeated global work to justify its code and startup cost. The timing probes here did not implement the fusions.

## 6. Overlap with other work

- **Loop.** The loop investigation shares global feedback, dispatch, comparisons and increments. Use one implementation and one set of cost measurements for those steps. Reprice call input batching after the loop changes. Do not add the predicted savings from two proposals that remove the same dispatches.
- **Objects.** The global path and named property paths share the feedback record, shape mutation and interpreter dispatch. Check `objects` when changing cache indexing or native layout. Global batches need only the global watch tag and the own-data guarantee.
- **Webpack.** Measure it when changing dispatch or code size. Module wrapper calls may have different frame and property costs. Use its own profile to decide whether these patterns occur often enough. Portable code must carry indices and offsets. Per-realm feedback owns slot addresses.
- **Adaptive GC and #908/#909.** This run used the frozen checkout above. The GC trigger changes allocation-heavy controls. #908/#909 change integer index reads. The calls loop has no indexed read or steady-state allocation. Rebase and remeasure the closure and objects controls before assigning their movement to the call work.

Raw samples, the source variants, disassemblies, Callgrind files and the reverted experiment are under `/data/bare-tmp/r22/gap-calls/` on archMachine. I did not run browser journeys, Windows, the compatibility suite or the proposed fusion implementations.

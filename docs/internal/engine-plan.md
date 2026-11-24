# Engine plan: shapes with inline caches, and a bytecode interpreter

Status (updated 2026-11-10): closed. Lane A is merged: steps A1 to A4 and A6. A7 is not needed, because the miss rate is 3.8%. Lane B stopped at the R1 gate on 2026-11-08 (#265). `byte` reached 2.49x on `eval_load loop 10`, and the gate needed 3x. Arjun chose to stop lane B. The B3 VM loop stays unmerged on branch `t3/engine-b3`. The format, the code generator and the verifier (B0 to B2) are on main, but no executor uses them. The tree walker is the only executor. Engine work now follows profiles of the tree walker. See progress.md, "B3: the bytecode VM loop stops at the R1 gate". The rest of this file is the original plan.

Original status: plan. Step C1 is done (PR #222: -3.0 M for each key, not the -6 to -10 M that the plan estimated). Steps A0 and B0 are the next ones. The numbers in sections 1 and 2 are from main b3052d2 (after PR #216 and #218), 2026-11-07.
Decisions from Arjun: build both A (shapes and inline caches) and B (bytecode). Keep the tree walker as tier 0 for code that runs once. It is not removed (step B13).

Words used in this file:
- "hypothesis" means a number or fact that no run proved.
- "M" means million instructions in user mode (`perf stat -e instructions:u`).
- "key" means one typed key on the React page of `tools/js/typing` (`type_load`, `#a1`).

## 1. Measured profile

Method. `perf record -e instructions:u -c 200000` of `type_load` with 50 keys and with 0 keys. The per-key cost is the difference divided by 50. Build: release, `-g`. `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` as in `tools/js/instr.sh`. Call counts come from uprobes (`perf probe -x`, counts for 50 keys minus 0 keys). The probes were deleted after the run.

Cost of one key: 414.4 M by `perf stat` (50 keys minus 0 keys), 417.8 M by the sample sum. Load and mount: 780 M.

| Group (self instructions per key) | M | Share | What is in it |
| --- | ---: | ---: | --- |
| AST dispatch and statements | 213.5 | 51% | `eval_expr_inner` 60.3, `machine` 30.4, `link_inner` 29.5, `eval_assign` 11.9, `enter_rare` 11.6, `exec_simple` 9.3, `link_base` 7.5, `eval_seq` 6.6, `num_lit` 4.9, `binary` 4.1, `eval_args` 4.1, `to_boolean` 3.5, rest |
| Name and scope | 29.6 | 7% | `get_name` 18.0, `assign_name` 6.0, `expr_key` 2.5, `name_str` 1.7 |
| Property get and set in the heap | 85.7 | 21% | `Heap::lookup` 32.3, `PropMap::find` 32.2, `get_prop` 6.9, `Heap::set` 3.3, `get_own_property` 2.9, `peek_own_property` 2.6 |
| Property define (new properties) | 10.4 | 2.5% | `ordinary_define` 5.8, `define_own_property` 4.6 |
| Calls and frames | 21.7 | 5% | `call_frame` 4.7, `call_script` 4.5, `call_fn_inner` 4.4, `HashMap::contains_key` 6.0 (this is `is_callable`: `funcs` is a hash map) |
| Allocation | 11.9 | 3% | `alloc_object` 2.2, malloc and free 2.6, `reserve_rehash` 2.5, `intern` 1.7, `HashMap::insert` 1.5 |
| GC | 6.7 | 1.6% | `collect` 2.7, `stored_edges` 2.3, `Heap::collect` 1.7 |
| Long tail | 38.3 | 9% | many small symbols |

Counts for one key (uprobes):
- 1,016 K calls of `eval_expr_inner`. About 59 instructions of self cost for each call.
- 457 K calls of `get_name`. That is 45% of all node visits. 39 instructions for each call.
- 154 K calls of `get_prop`, 197 K of `Heap::lookup`, 429 K of `PropMap::find`. One `lookup` makes 2.2 `find` calls (own object, then the prototype chain). The call-graph shows 79% of `lookup` comes from `get_prop` and 19% from `Heap::set`.
  Cost of one property read: (32.3 + 32.2) M x 0.79 / 154 K = about 330 instructions.
- 33 K JS calls (`call_script`), 87 K simple statements, 38 K `machine` runs, 16.6 K `enter_rare` runs.
  One JS call costs 12.5 K instructions including all that it runs. A call runs about 44 evaluator steps.
- `enter_rare` costs about 700 instructions for each call (loop entry). Cause not found. Hypothesis: the loop head setup. To investigate (step C2).

Rows (`tools/js/instr.sh`, minimum of 3, glibc with the tcache setting, main b3052d2). These are the baseline of every later step:

| Row | M |
| --- | ---: |
| eval_load loop 10 | 6509.2 |
| eval_load calls 10 | 4672.4 |
| eval_load fib 10 | 229.7 |
| eval_load closures 10 | 814.0 |
| eval_load objects 10 | 2806.5 |
| eval_load webpack 10 | 44.6 |
| proxy_load reactive 10 | 261.5 |
| class_load construct 10 | 2144.8 |
| gen_load forof 10 | 356.7 |
| async_load nested 10 | 64.5 |
| scripts_load few 10 | 136.7 |
| arrobj_load objects 10 | 60.5 |

Top symbols by row (self share):
- `calls 10` and `fib 10`: `eval_expr_inner` 22% to 26%, `machine` 11% to 13%, `num_lit` 4% to 5%, `get_global` 4%, `call_frame` 4% to 6%. The heap is not in the top list. These rows are all dispatch.
- `loop 10`: 3.2 K instructions for each iteration of about 14 node visits. That is about 230 instructions for each visit.
- `objects 10`: `PropMap::find` 5.1%, `Heap::lookup` 4.5%, `Heap::intern` 3.4%, `ordinary_define` 3.3%.
- `webpack 10` (44.6 M): mostly parse and lex. Not an evaluator row at this scale.
- `proxy reactive 10`: `link_inner` 11%, `machine` 10%, `eval_expr_inner` 8%, `Heap::lookup` 5%.

Memory facts (`MEM=1`, 20 keys): peak RSS 36.9 MiB. Syntax trees 2.74 MiB (with 0.3 MiB source) plus their caches 1.57 MiB. 131,379 syntax nodes. After load: 24 K objects, property entries 4.47 MiB used of 5.84 MiB, index tables 0.77 MiB. One property entry is 48 bytes (`Option<(PropertyKey, Property)>`: key 12, `Property` 32). A `Value` is 16 bytes. An element is 16 bytes. The object slab is 80 bytes or less for each object.

What the profile says:
- About 62% of a key is interpretive overhead (dispatch, names, calls). It is not the heap. Option B owns it.
- About 21% is property reads and writes with a hash or a scan. Option A owns it.
- No part is large enough alone to give 2x. The two options together are needed.

## 2. The options in short

### Option A: shapes and inline caches on the current walker

How objects are stored now: `Object { proto, flags, elems: Vec<Option<Value>>, props: PropMap }`. `PropMap` is a `Vec<Option<(key, Property)>>` in insertion order. It has a `KeyMap` hash index above 8 entries. A read is `get_prop` -> `Heap::get` -> `lookup` -> `own` -> `PropMap::find` for each object in the chain. A hint (`own_data_at`) exists only for global names.

Design:
- A shape is a node in a transition tree. Shape = (parent, key, attributes, count, shared key table). One root shape for all ordinary objects. One root for arrays (slot 0 is `length`). Roots differ by object kind: function, host object with lazy `on*`, and so on. A kind that needs lazy or host work is not cacheable.
- The key table is shared along a chain and is cut by `count` (Boa does this: `property_table.rs`). It is a linear scan up to 8 keys and a hash above. The table is copied when a chain branches.
- An object keeps `proto` as now. An object stores `shape: u32` and `slots: Vec<Value>` (16 bytes for each property instead of 48). Attributes (writable, enumerable, configurable, accessor) are in the shape. An accessor uses two slots (get, set). Boa does the same.
- Dictionary mode is today's `PropMap`, unchanged, behind `dict: Option<Box<PropMap>>`. Shape id 0 means dictionary. An object goes to dictionary mode when: any property is deleted (except the last one added), an attribute of an existing property changes, `setPrototypeOf` runs, the count is above 128, the chain from the root is longer than 1024 (Boa: `TRANSITION_COUNT_MAX` 1024), or the object is exotic (proxy, host named or indexed, typed array, String wrapper). Dictionary objects never hit an inline cache.
- Array elements stay in `elems` (fast elements). They do not change shapes (V8 does the same: adding an index adds no hidden class).
- Inline cache (`Ic`, plain `Copy` data, 24 bytes): `shape u32`, `slot u32`, `kind u8` (empty, own, proto, absent), `holder ObjId` (8 bytes), `epoch u32`.
  - Own hit: `obj.shape == ic.shape` then `slots[ic.slot]`.
  - Proto hit: the shape matches, `obj.proto == ic.proto_first`, `heap.proto_epoch == ic.epoch`, and the holder handle is still valid (the generation check that every handle read does already). The value is read from the holder at hit time, so a value change needs no invalidation.
  - Absent hit (for `a.optional` that is `undefined`): the same checks, returns `undefined`.
- Invalidation. `proto_epoch` is one counter in the heap. Every object that an `Ic` fill walked over (receiver chain and holder) gets the flag `IC_CHAIN`. Adding, deleting or reconfiguring a property of a flagged object, or `setPrototypeOf` on it, bumps the epoch. The flag is set only on the cold fill path. The mutation paths test one bit. Hypothesis: epoch bumps are rare after startup. If a page thrashes the epoch, refine to V8-style validity cells for each prototype object.
- Where the cache lives. AST first: `expr_ic: Vec<u32>` (4 bytes for each node, 0 = none) points into `ics: Vec<Ic>`. An `Ic` is made on the second run of a site, as the name slots are today. Bytecode later: `Code.ics: Box<[Cell<Ic>]>`, one for each property instruction, the operand is the index. The same `Ic` type and the same functions serve both (section 3).
- Shape memory: about 64 to 80 bytes for each shape plus the shared tables. Shapes are not freed in the first step. A cap on the total count sends new objects to dictionary mode. A later step frees shapes in `collect`.

How others do it (cited):
- QuickJS: shapes shared between objects, hash-consed in a runtime table (`JSShape` has `hash`, `prop_hash_mask`, `proto`, a hash table and `prop[]`). The manual says: "The object shapes (object prototype, property names and flags) are shared between objects to save memory." The page names no inline cache. The `OP_get_field` body was not readable in the fetch, so "no inline cache" is to verify in `quickjs.c`. Sources: https://bellard.org/quickjs/quickjs.html, https://github.com/quickjs-ng/quickjs (`quickjs.c`).
- Boa: `Shape` is shared or unique (dictionary). Shared shapes keep the prototype, a forward transition table and a property table shared by the chain. Weak transitions. Unique shape after 1024 transitions. An `InlineCache` for each property instruction is in the `CodeBlock` (`ic`), polymorphic with 4 entries, megamorphic flag, `WeakShape` entries. A proto hit caches only the first prototype (`SlotAttributes::PROTOTYPE`). Source: `core/engine/src/{object/shape,vm/inline_cache}` in https://github.com/boa-dev/boa (commit 329e897).
- V8: a hidden class (map) for each shape. A transition tree. In-object properties. Dictionary mode after heavy add and delete. Inline caches depend on the hidden class and do not work on dictionary objects. Source: https://v8.dev/blog/fast-properties.
- JavaScriptCore: structures are hash-consed. The LLInt `get_by_id` keeps a structure id and an offset in the instruction metadata. One compare, then a load. The first run misses on purpose (an impossible id). Source: https://webkit.org/blog/10308/speculation-in-javascriptcore/.
- Hermes: bytecode only here. The doc pages that were readable do not describe hidden classes or a property cache. Not verified. Source: https://github.com/facebook/hermes/blob/main/doc/Design.md.
- engine262: an AST walker built for spec fidelity. Speed is a non-goal. No shapes. Source: https://github.com/engine262/engine262.
- Kiesel: a Zig engine with a bytecode VM (LibJS design). The page says nothing about object storage. Source: https://codeberg.org/kiesel-js/kiesel.

Estimated gain (hypotheses, from the counts above):
- Property read. Now about 330 instructions. An inline cache hit is about 35 instructions (shape load, compare, slot load, two checks). At a hit rate of 85% to 95% the 154 K reads cost 8 to 13 M instead of 51 M. Saving: 38 to 43 M.
- Property write (set side is about 23 M). With a store cache and an add-property transition cache at an 80% hit rate: saving 8 to 15 M.
- Object and closure creation (define path 10.4 M plus part of the allocation): a literal site caches its final shape, a closure starts with a preset function shape. Saving 5 to 10 M.
- Total A: 50 to 70 M for each key (12% to 17%). Estimate 60 M. Result: about 355 M for each key.
- Memory: 16 bytes for each property instead of 48, no hash index for objects above 8 properties. Estimate: property entries 4.47 MiB and index tables 0.77 MiB become about 1.6 MiB slots and 0.2 MiB shapes. Saving 3 to 3.5 MiB of 27 MiB RSS after load. Cost: 24 bytes for each executed property site.
- Code size: about 1,200 to 1,800 new lines in `heap.rs` and a new `heap_shape.rs`, plus about 300 lines in the evaluator. Binary +30 to 60 KB (hypothesis).
- Risk: medium. The `Object` layout, the GC trace, `own_keys` order, accessors and attribute changes all touch `heap.rs` (3.2 K lines, one owner at a time). A wrong cache is a silent wrong answer, so debug builds check every hit against the generic path (as `find_name` does today).

### Option B: bytecode interpreter

Design:
- Register machine, three-address, 32-bit instruction words `[op:8][a:8][b:8][c:8]`. A 16-bit operand form and a jump form (like Lua 5.4). A wide prefix word for 16-bit registers and 32-bit constants (like Ignition `Wide` and Hermes `Long` forms). Rule: 255 registers for each function. A function that does not fit stays on the tree walker.
  Reason for words and not bytes: decode is a shift and a mask, the loop reads `code[pc]` with one bounds check, and the code is safe Rust. Bytes are 30% smaller but cost more to decode (hypothesis).
- Dispatch is `loop { match op }` in safe Rust. It compiles to a jump table. Computed goto does not exist in Rust. Tail-call threading needs `become` (explicit tail calls). It is unstable: rustc 1.96.0 stable gives error E0658 (checked). The project uses stable Rust, so `become` is out. Plan: measure `loop + match` first. Move every slow path to `#[cold] #[inline(never)]` functions. This repo measured the same effect: `machine` frame went from 1,120 to 416 bytes by outlining cold arms.
- Frames. One `Vec<Value>` register stack. One `Vec<Frame>` of (code, pc, base, env, this, callee). A JS to JS call pushes a frame and continues the same loop. No Rust recursion for JS to JS calls. A native builtin that calls back (map, sort, getters, proxy traps) starts a nested loop. This ends the 2.8 KB of native stack for each JS call and most of the `stacker` growth for pure JS recursion. The limits `max_call_depth` 10,000 and the `RangeError` behavior stay.
- Exceptions: a handler table for each function (pc range -> handler pc, exception register). `finally` uses a completion register and a jump (as Ignition and QuickJS do). Errors that are limits or unsupported constructs still bypass `catch` and `finally` as today.
- Closures and scopes. Captured variables live in `Env` (the existing arena). Stage 1 keeps the walker's exact scope layout (declaration order, one `Env` for each scope that has one now), so a bytecode function and a tree function can sit in one scope chain. The resolver data (`sc_all`, `RScope`) already exists after the first walk. Stage 2 (step B12) moves uncaptured locals to registers. It adds a capture bit to the shared analysis. It changes the layout rule on both engines together (the walker's frame slots already skip an `Env`; the rule becomes "a scope has no `Env` when it has no captured binding").
  TDZ for registers: only bindings that the compiler proves initialized go to registers. Others stay in `Env`. No new `Value` variant is needed.
- Generators and async: a suspended frame is its register slice plus pc, moved into a heap object. This replaces the `Run`, `Fr` and `Slot` machine for functions that run on bytecode. `eval_async.rs` is 89.7 KB and `eval_generator.rs` is 14.7 KB. The tree walker stays (step B13), so these files stay. They serve code that runs on tier 0.
- Classes, templates, regexp literals, destructuring: first as coarse ops that call the existing runtime helpers (`eval_classes.rs`, 55 KB). Fine-grained bytecode for destructuring comes next, because it is the common React shape.

How others do it (cited):
- QuickJS: stack-based bytecode, no intermediate tree, stack size computed at compile time. "A stack-based bytecode was chosen because it is simple and generates compact code." Closure variables are "almost as fast as local variables". Source: https://bellard.org/quickjs/quickjs.html.
- Hermes: register-based, one-byte register indexes ("no function ever uses more than 256 registers"), variable-length instructions, scope hop count plus slot index for non-local variables, linear-scan register allocation. Source: https://github.com/facebook/hermes/blob/main/doc/Design.md.
- V8 Ignition: register machine with an accumulator. Bytecode is "between 50% to 25% the size of the equivalent baseline machine code". About 5% less memory for each tab on small devices. Source: https://v8.dev/blog/ignition-interpreter.
- JavaScriptCore LLInt: register bytecode, about one byte for each opcode or operand with wide prefixes, the same stream is cached and handed to the JIT tiers. Written in an assembler macro language so a call is one frame. Source: https://webkit.org/blog/10308/speculation-in-javascriptcore/.
- Boa: register VM with NaN boxing. Local measurement (docs/resources.md, "Boa compared with bare-js"): bare-js was faster than Boa on 6 of 8 workloads by 1.1x to 4.4x, and parse plus compile of the 40 staging chunks (1.7 MiB) took 0.634 s and 52.6 MiB against 0.030 s and 12.5 MiB for bare-js parse. A bytecode design alone does not give speed. This is the main warning for B. The cost sits in the details: `Copy` values, no reference counts, no clones in the hot path.

Estimated gain (hypotheses):
- A node visit costs about 230 to 300 instructions now (`loop 10`: 3.2 K for about 14 visits). A bytecode op in a Rust `match` loop should cost 30 to 60 instructions with the work inside. A register op with no heap work is about 15. `loop 10` has about 12 ops for each iteration, so pure dispatch rows should gain 4x to 7x.
- The React key: the groups that B replaces are dispatch 213.5, names 29.6 and calls 15.7 (without `contains_key`), 259 M together. Model: about 0.8 M ops for each key at about 45 instructions, plus call setup and `Env` work. Estimate 100 to 140 M after stage 1, 80 to 115 M after stage 2. Saving (stage 1): 120 to 160 M. Stage 2: 20 to 30 M more.
- Heap work (property access, allocation, GC) is not touched by B. It stays at about 140 M until A lands.
- RSS: bytecode of the functions that run, hypothesis 0.3 to 0.6 MiB for this page (4 bytes for each op, 131 K nodes if all were compiled, about 0.7 ops for each node), plus 24 bytes for each property instruction for the cache. While the tree walker and its tables stay (4.3 MiB), RSS is +0.5 to 0.8 MiB. Dropping the tree after compile (reparse a function from source when it first runs) would give -2.4 MiB. Not planned yet.
- Compile time: the parser needs 0.030 s for 1.7 MiB. Hypothesis: a code generator pass costs 1x to 2x the parse time for the code it compiles. All of a 2 MiB bundle: 30 to 70 ms. Lazy compile of called functions only: a third of that. Run-once code pays the compile, the tree walker does not. This is why the plan keeps tiers (section 4, step B11).
- Effort: large. The language surface that React and Next.js bundles use is wide (classes, destructuring, spread, generators, async). Estimate 10 to 14 engineer-weeks for B1 to B12 (hypothesis).
- Risk: high. Semantics are re-implemented for each node kind. The shared realm, the host re-entrance, `descend` limits, sloppy `arguments`, TDZ and the async machine must all agree with the tree walker. Control: the differential harness (section 5).

### Why both, in this order

- A has no dependency on B. Its hit path is in the heap. B can call the same functions.
- B has no dependency on A for correctness. It calls the cache API, which is a no-op until A2 lands. After A2 the VM gets the caches with no B change.
- A is smaller and gives memory. B gives the larger part of the time. They run in parallel lanes (section 4).

Expected end state (hypothesis, from the ranges above): about 165 to 230 M for each key (1.8x to 2.5x faster), about 30 to 40 ms for each key on the busy machine of the measurements (now 73 ms). Chrome takes 3.4 to 4.7 ms. The gap goes from about 20x to about 8x to 10x. A bigger step needs a smaller `Value`, a smaller object handle, or a JIT. Those are deferred (section 7).

## 3. The contract that both lanes share

Put it in `docs/decisions.md` (section "Shapes and inline caches") in step A0. Both lanes code against it.

```text
heap_shape.rs (new, a submodule of heap like heap_big.rs)
  pub struct Ic { shape: u32, slot: u32, kind: u8, holder: ObjId, epoch: u32 }   // Copy, 24 bytes, no pointers
  Heap::ic_own(&self, o: ObjId, ic: &Ic) -> Option<Value>        // #[inline(always)], own hit only
  Heap::get_ic(&mut self, o: ObjId, key: PropertyKey, ic: &mut Ic) -> R<Get>   // #[cold] slow path, fills the cache
  Heap::set_ic(&mut self, o: ObjId, key: PropertyKey, v: Value, ic: &mut Ic) -> R<Set>
```

Rules:
- `Ic` is plain data. It holds no pointer and needs no tracing. A stale `holder` fails the generation check. A freed shape resets every `Ic` (step A6). All tables of caches are reachable from `Interp`.
- AST path: `Interp.ics: IcState { expr_ic: Vec<u32>, ics: Vec<Ic> }`. Bytecode path: `Code.ics: Box<[Cell<Ic>]>`. The caller passes `&mut Ic` or a copy and writes it back. The two tables are disjoint fields from `heap`, so there is no borrow conflict.
- The interpreter calls `ic_own` inline. On `None` it calls `get_ic` out of line. The bytecode op `GetField dst, obj, ic_index, name_const` and the AST site use the same two calls.
- A function can run on the tree first and on bytecode later. The two caches are separate. The only cost is one more warm-up.
- Debug builds: every hit is compared with `Heap::get`. A mismatch panics.
- Each lane adds its state as one sub-struct (`ics: IcState`, `bc: BcState`) to `Interp` in its first step. Later steps edit the sub-struct files and not the `Interp` field list. This keeps the merge conflicts small.

## 4. Steps

Each step is one PR. Each lands alone with tests green. Perf rule for every step: no row of the cost matrix worse than +0.5% against the previous main, with glibc and with jemalloc, except where the step says "switch off path" (below). Sizes: S is up to 3 days, M is about 1 week, L is 1.5 to 3 weeks (hypotheses).

Switch (used by B steps). `Interp::set_exec(Exec)` with `Exec::{Tree, Auto, Byte}`. The default is `Tree` until step B11. The examples and the `bare` session read `BARE_JS_EXEC=tree|auto|byte` and call `set_exec`. The library reads no environment variable.
- `Tree`: never compile. This is today's engine.
- `Byte`: compile each function at its first call, and the script body when it has a loop or a call. Used for tests and for the differential harness.
- `Auto`: tiers. A function runs on the tree walker for its first call. It is compiled at its second call, or at its first call when it has a loop. This is the same rule as `fr_gate` and `FR_EAGER` for frame slots, for the same reason: code that runs once pays nothing. Generators and async functions follow the policy of step B9.
- A function that the compiler cannot compile (an unsupported node, more than 255 registers) is marked `Tree` for good. `Interp::stats()` counts these fallbacks. With `BARE_JS_EXEC_STRICT=1` (examples only) a fallback is a hard error, so the tests show every gap.
- Both engines call each other through `call_fn`. They share the `Env` arena and the scope layout (stage 1). So a mixed chain works.
- Steps B1 to B10 keep the default off. With the switch off, the added cost is one compare in `call_script`. That cost must stay within +0.5% on every row. Rows measured with the switch on are reported and may be worse until step B11.
- Step B11 flips the default to `Auto`. The tree walker stays as tier 0 (decided, step B13).

### Lane T: tools (no engine code)

**T0. Gates and baselines.** Size M. Depends on nothing.
- Files: `tools/js/cost_matrix.sh` (new), `tools/js/cost_rows.txt` (new, the 566 rows), `tools/js/typing/keycost.sh` (new), `tools/js/test262_check.sh` (new), `crates/js/examples/test262_run.rs` (new), `tools/fixtures/js/test262-subset.txt` and `test262-baseline.txt` (new), `docs/progress.md`.
- `cost_matrix.sh BASE_DIR NEW_DIR` runs every row of `cost_rows.txt` with `instr.sh` for both binaries, with glibc and with jemalloc, and prints the rows over +0.5% under both. Today this is done by hand for each PR (566 rows).
- `keycost.sh OUTDIR`: builds the page if missing, prints M for each key (50 keys minus 0 keys) for `#a1` and `#f0`, and the RSS at 20 and 50 keys.
- test262: not in the repository. Plan: pin a commit of `tc39/test262` in a checkout outside the repository. A runner reads the metadata (`flags`, `includes`, `features`, `negative`), loads the harness files (`assert.js`, `sta.js`, `propertyHelper.js`, `compareArray.js`, `doneprintHandle.js`, async helpers), skips features that bare-js does not support, and prints pass and fail for each directory. First target: `test/language/**` (this is where an engine rewrite breaks things), then `built-ins/{Object,Array,Function,Reflect,Proxy,Promise,Map,Set,Symbol}`. The baseline file records pass counts for each directory. Gate: no directory loses a passing test. The size of the checkout and the run time are not known (to verify).
- Check: the three tools run on main and the baselines are written to `docs/progress.md`.
- Parallel: yes, with everything. It touches only `tools/` and one example.

### Lane C: small wins that survive the migration

**C1. `funcs` map to a slot table.** Size S. Depends on nothing.
- Files: `crates/js/src/eval.rs` (`funcs`, `is_callable`, `type_of`, the places that read `funcs`), maybe `eval_host.rs`.
- Data: `funcs: FxMap<ObjId, Callee>` becomes a table indexed by object slot with the generation (the same idea as `IdMap` of PR #216). `is_callable` stops hashing.
- Estimate: `contains_key` 6.0 M plus `reserve_rehash` 2.5 M plus `HashMap::insert` 1.5 M. Saving 6 to 10 M for each key (1.5% to 2.4%).
- Check: `type_load` -6 to -10 M for each key; `eval_load closures 10`, `class_load construct 10` not worse; RSS not worse.
- Parallel: touches `eval.rs`. Merge it first, because A steps edit the same file.
- Done in PR #222. The table is `ObjTab` (`crates/js/src/obj_tab.rs`): a sparse set (`at[slot]` holds the generation and the place in a dense vector), not the paged table of the first idea, because 47% of the objects on the page are functions and the pages would be full. Result: -3.0 M for each key (411.3 M), RSS not worse, `closures 10` -1.7%, `class_load construct 10` -0.2%, `calls 10` -1.2%, `fib 10` -2.2%. The caller of `HashMap::contains_key` is `Interp::is_callable`, inlined at 64 sites. The saving is lower than the estimate because the inlined check costs about 2.5 M.

**C2. Find why `enter_rare` costs 700 instructions.** Size S. Report, then a fix only if it is cheap.
- Files: `eval_statements.rs`.
- Check: a note in `docs/progress.md` with the cause. B must not copy the cost.

Skip as separate work, because B replaces the code: `num_lit` caching (4.9 M), the `Rc` clone at the start of `eval_expr_inner` and `link_inner` (hypothesis 8 to 12 M), a fast path in `eval_assign` (hypothesis 4 to 6 M). PR #216 saw code-layout noise on changes like these. Do them only if B slips by a quarter.

### Lane A: shapes and inline caches

**A0. Cache API stub and the decision record.** Size S. Depends on nothing.
- Files: `crates/js/src/heap_shape.rs` (new: `Ic`, `ic_own` returns `None`, `get_ic` and `set_ic` call `get` and `set`), `heap.rs` (one `#[path]` line), `docs/decisions.md`.
- Data: `Ic` as in section 3. `Interp.ics: IcState` (empty).
- Check: the cost matrix shows no change (no code runs). `cargo test` passes.
- Why separate: it unblocks B3 to use the API before A2 exists.

**A1. Shapes, with no cache yet.** Size L. Depends on A0 and C1 (same file area).
- Files: `heap.rs` (`Object`, `PropMap` use, `own`, `define`, `delete`, `set`, `own_keys`, mark and sweep, `obj_bytes`, `footprint`), `heap_shape.rs` (shape table, transitions, shared key table, root shapes), `heap_proxy.rs`, `heap_named.rs`, `heap_binary.rs`, `heap_prim.rs` (exotic kinds go to dictionary mode), `eval.rs` (only the places that read `PropMap` positions: `own_pos`, `own_data_at`, `set_own_data_at`, which keep working on dictionary objects and on shape slots).
- Data: `Object { proto, shape: u32, flags, elems, slots: Vec<Value>, dict: Option<Box<PropMap>> }`, still 80 bytes or less. `Shape { parent, key, attrs, count, table: Rc<Table>, child: (key, attrs, id), children: Option<Box<Map>> }`. Shapes are never freed in this step. A cap on the shape count (hypothesis: 65,536) sends new objects to dictionary mode. The dictionary rules are in section 2.
- Check:
  - Unit tests: property order (integer keys, then strings in insertion order, then symbols), attributes, accessors in two slots, delete and re-add, every dictionary trigger, arrays with `length` in slot 0, GC trace of slots, `footprint`.
  - A property test: random operation sequences (define, delete, set, freeze, setPrototypeOf) run against both a shape object and a plain `PropMap` model. Compare keys, values and attributes after each operation. 100 K operations.
  - Type page: instructions for each key within +-0.5% of 414 M (no cache yet). Hypothesis: neutral, because adding a property needs one transition lookup and `PropMap::set` does a second `find` today. Also run the `objects 10`, `arrobj_load objects`, `revive_load` and `stringify_load` rows (creation-heavy). If creation rows are worse than +0.5%, A1 merges stacked on A2 as one merge unit. The single-entry `child` cache in each shape is the first fix.
  - RSS: `MEM=1` property entries plus index tables fall from about 5.2 MiB to about 1.8 MiB after load (hypothesis). RSS at 50 keys no more than 36.1 MiB and expected about 33 MiB.
- Parallel: this step owns `heap.rs`. No other step edits `heap.rs` at the same time.

**A2. Inline cache on member reads and calls (AST path).** Size M. Depends on A1.
- Files: `heap_shape.rs` (real `ic_own`, `get_ic`, the fill walk, `IC_CHAIN` flag, `proto_epoch`), `heap.rs` (bump the epoch in add, delete, reconfigure, `setPrototypeOf` of a flagged object), `eval.rs` (`link_inner` Member arm, `get_prop`, `link_base`, `IcState` tables), `eval_stack.rs` or the stats code for counters.
- Data: `expr_ic: Vec<u32>` (4 bytes for each node of the script, filled when the script is selected), `ics: Vec<Ic>` (24 bytes for each site that ran twice). Counters (hit, miss, per kind) in `MEM=1` output only.
- Check:
  - Debug builds compare every hit with `Heap::get`.
  - Tests: shadowing (add the same key on the receiver, on a middle prototype, on the holder), prototype replacement, a getter turned into a data property, a value change on the holder, a proxy in the chain, a collect that frees the holder, an object that went to dictionary mode, polymorphic sites with 2 and 5 shapes.
  - Type page: -30 to -45 M for each key. Estimate -40 M (to about 365 M). The cost of a hit and the miss rate come from the new counters.
  - Rows: `objects 10`, `proxy reactive 10`, `class_load construct 10`, `arrobj_load objects 10` lower. `calls 10`, `fib 10`, `loop 10` within +0.5% (they read global names, not members).
  - RSS: the cache tables add 0.1 to 0.3 MiB.
- Parallel: edits `eval.rs` (`link_inner`, `get_prop`). B3 edits `call_script` in the same file. Small conflict risk. See "Lanes and conflicts".

**A3. Inline cache on member writes (store and add-property transition).** Size M. Depends on A2.
- Files: `heap_shape.rs` (`set_ic`, a transition cache: old shape -> new shape and slot), `eval.rs` (`eval_assign`, `member_target`, `put_prop`, `eval_update`).
- Data: reuse `Ic` with two more kinds: store (own writable data property) and add (from shape S to S').
- Check: tests for read-only, setters on the chain, non-extensible, frozen (goes to dictionary), `Object.defineProperty` on a cached shape, array `length` writes. Type page -8 to -15 M for each key. Rows `objects 10`, `handler_load`, `class_load construct 10`.
- Parallel: after A2. Same files as A2.

**A4. Literal, closure and class-instance shapes.** Size S to M. Depends on A3.
- Files: `eval.rs` (`eval_object`, `eval_array`, `make_closure`, class construct), `heap_shape.rs` (`Heap::new_object_with_shape(shape, slots)`).
- Data: an object-literal site keeps its final shape (4 bytes in a side table). A closure starts from a preset function shape (`length`, `name`, lazy `prototype`). `new_array` starts from the array root.
- Check: type page -5 to -10 M for each key. Rows `closures 10`, `objects 10`, `class_load churn`. The `define` path (`ordinary_define` 5.8 M, `define_own_property` 4.6 M) falls.

**A5 (optional). Fast element reads and writes.** Size S. Only if a counter shows `a[i]` through `lookup`.
- Files: `eval.rs` (`Index` arm of `link_inner`, `eval_assign`), `heap.rs` (`get_elem`, `set_elem`).
- Check: rows `arrobj_load arrays`, `fill`, `sort`.

**A6. Free shapes in `collect`, and reset caches.** Size M. Depends on A2.
- Files: `heap.rs` (mark: set a mark on the shape of each live object and on its parents; sweep: drop unmarked shapes and their transition entries), `heap_shape.rs`, `eval.rs` (reset every `Ic` table after a shape sweep).
- Data: shape ids are never reused until the next sweep. A sweep runs only when the shape count has doubled since the last one (so it is rare). Hypothesis: the mark costs 1 store for each live object, under 0.5% of a collect.
- Check: a soak test that makes 1 M objects with distinct key sets keeps RSS flat. A collect test with caches live. `type_load` 50 keys: collect cost within +0.2 M for each key.

**A7. Polymorphic and megamorphic caches.** Size M. Only if the counters of A2 show a miss rate over 10% on the type page.
- Files: `heap_shape.rs`, `eval.rs`.
- Data: two entries for each site (48 bytes), then a global direct-mapped table of 4 K entries (about 64 KiB) keyed by hash(shape, key).
- Check: miss counters, `type_load`, RSS +64 KiB.

### Lane B: bytecode

**B0. Design record, format, and disassembler.** Size S. Depends on A0 (for the `Ic` type in `Code`).
- Files: `crates/js/src/bc.rs` (new: opcode enum `#[repr(u8)]`, encode and decode, `Code { ops: Vec<u32>, consts, ics, handlers, lines, nregs }`, a disassembler), `lib.rs` (one `mod` line), `docs/decisions.md` (section "Bytecode": register machine, 32-bit words, the switch, the tier rule, the scope rule).
- Check: encode and decode round trip for every form (including wide). No row can change (nothing runs).

**B1. Scope data for the compiler (stage 1: the walker's layout).** Size M. Depends on B0.
- Files: `crates/js/src/bc_scope.rs` (new). It reads `sc_all`, `RScope` and the site tables that the first walk makes (`eval_scope.rs`, read only).
- Data: for each function: the scopes in order, for each binding the (hops, index), flags (let, const, TDZ, function name), uses of `this`, `arguments`, `new.target`, `super`, the class barrier, and whether the function has a loop or a call (the same flags as `FL_*`).
- Check: a test walks every identifier site of the `eval-cases` corpus and the typing bundle and compares (hops, index) with what the tree walker resolves. They must be equal for every site.
- Parallel: yes, with A1 to A4 (new file).
- Done: `bc_scope.rs`. The corpora and the typing page give 0 differences. See docs/progress.md, "Scope data for the code generator".

**B2. Code generator, core subset, with a verifier.** Size L. Depends on B1.
- Files: `crates/js/src/bc_gen.rs`, `bc_gen_expr.rs`, `bc_gen_stmt.rs` (new), `bc_verify.rs` (new: register bounds, jump targets, handler ranges), `bc_tests.rs` (golden disassembly).
- Subset: literals, names (environment slot, global), member get and set with `ic` operands, calls (with `this`), `new`, unary, binary, logical, conditional, assignment, update, object and array literals, function expressions, template literals, `if`, `for`, `while`, `do`, `switch`, `break`, `continue`, `return`, `throw`, `try`, `catch`, `finally`. Any other node returns `Unsupported` and the function stays on the tree walker.
- Check: golden disassembly of 60 small programs. The verifier runs on the whole `eval-cases` corpus and on the typing bundle and reports the share of functions that compile (the coverage number). Code is dead until B3.
- Parallel: yes with A steps (new files only).
- Done: `bc_gen.rs`, `bc_gen_expr.rs`, `bc_gen_stmt.rs`, `bc_verify.rs`. The golden tests are in `bc_gen_tests.rs` (67 programs), because they run scripts. 80.5% of the corpus functions and 86.9% of the typing page functions compile, and all of them verify. See docs/progress.md, "Code generator and verifier".

**B3. The VM loop and the switch (core subset, no JS to JS calls yet).** Size L. Depends on B2 and A0.
- Files: `crates/js/src/bc_vm.rs` (new: the loop, the register stack, the handler search), `eval.rs` (`Interp.exec`, `BcState`, one check in `call_script`, `set_exec`), `eval_scripts.rs` (script body policy), `crates/js/examples/*.rs` (read `BARE_JS_EXEC`: `eval_load`, `type_load`, and the other load examples through one helper).
- Data: `BcState { codes: Vec<Option<Rc<Code>>> by function, regs: Vec<Value>, frames: Vec<Frame> }`. GC roots: the register stack up to its top, each frame's `env` and `this`. `collect` keeps its rule (depth 0).
- Check:
  - `BARE_JS_EXEC=byte` passes every `eval-cases` case (`eval_check.sh`, `statements_check.sh`, `sc_check.sh`) and `cargo test`.
  - Differential: the new `tools/js/exec_diff.sh` runs every `tools/fixtures/js/*-cases.txt` under `tree` and `byte` and compares the output.
  - Switch off: all rows within +0.5% (one compare in `call_script`).
  - Switch on, reported: `eval_load loop 10`, `calls 10`, `fib 10`, `closures 10`. Hypothesis: `loop` and `fib` gain 3x or more.
- Parallel: touches `eval.rs` `call_script` and the `Interp` struct. See conflicts.

**B4. JS to JS calls in the loop, `this`, `arguments`, parameters, `new`.** Size L. Depends on B3.
- Files: `bc_vm.rs`, `bc_gen_expr.rs`, `eval_classes.rs` (construct path for plain constructors).
- Data: `Frame { code, pc, base, env, this, callee, argc, flags }`. Call and return ops. Default and rest parameters. Sloppy and strict `arguments` (the layout of the tree walker). Native re-entrance starts a nested loop.
- Check: `recursion_load`, `stack_load` and the depth tests give the same limits and errors in both engines (`Limit(CallDepth)`, `RangeError`). Native stack for a JS call in `byte` mode is measured with gdb as in docs/resources.md (the tree walker: 2.8 KB). `calls 10` and `fib 10` report the gain.

**B5. Destructuring, spread, optional chaining, `for-in`, `for-of`, labels.** Size L. Depends on B4.
- Files: `bc_gen_pat.rs` (new), `bc_gen_stmt.rs`, `bc_vm.rs`.
- Check: the corpora of `iterator_check.sh`, `statements_check.sh`, `eval_check.sh`; iterator close on every exit path as in the tree walker. The coverage number on the typing bundle rises (target: 100% of the functions that run on the page).
- Parallel: B5 and B6 can run in two branches (different new files). They merge in order.

**B6. Classes, private names, `super`, getters and setters in literals, tagged templates, regexp literals, `delete`, `in`, `instanceof`, `typeof` of an unresolved name.** Size L. Depends on B4.
- Files: `bc_gen_class.rs` (new), `bc_gen_expr.rs`, `bc_vm.rs`. Class definition and field initializers call the helpers of `eval_classes.rs` as coarse ops.
- Check: `class_check.sh`, `subclass_check.sh`, `tagged_check.sh`; `class_load` rows in `byte` mode.

**B7. Cold paths and size budget of the loop.** Size S. Depends on B6.
- Files: `bc_vm.rs`.
- Work: move every slow path out of the loop; check the native frame size of the loop function (target 400 bytes or less, like `machine` today); try one hot superinstruction pair only if the op counts of the type page show a pair over 10% of ops.
- Check: instructions for `type_load` in `byte` mode; frame size by gdb.

**B8. Generators on bytecode frames.** Size L. Depends on B6.
- Files: `bc_vm_gen.rs` (new), `eval_generator.rs` (dispatch hook only).
- Data: a suspended generator holds its register slice, pc and env in one boxed value (the same GC contract as `gen_trace` and `gen_prune`).
- Check: `generator_check.sh` (50 cases), `gen_load` rows, memory for a suspended generator (769 B for `yield k` today, docs/resources.md).

**B9. Async functions and `await` on bytecode frames.** Size L. Depends on B8.
- Files: `bc_vm_gen.rs`, `eval_async.rs` (dispatch hook only), `eval_promise.rs` (resume job).
- Check: `async_check.sh`, `promise_check.sh`, `async_load` rows, the microtask order tests. Policy: an async or generator function compiles at its first call in `Auto` mode only after this step. Before it, they stay on the tree walker.

**B10. Coverage gate and tiering policy.** Size M. Depends on B5 to B9.
- Files: `eval.rs` (the `Auto` rule, fallback counters), `crates/js/examples/compile_load.rs` (new: compile time and RSS of the cached staging chunks, 40 files, 1.7 MiB), `docs/progress.md`.
- Check: in `byte` mode on the corpora, the UI fixtures and the typing page, fallbacks = 0. `compile_load` reports compile time against parse time (the number that sets the compile threshold of `Auto`). The test262 baseline holds in `tree`, `auto` and `byte` modes.

**B11. Flip the default to `Auto`.** Size S. Depends on B10, A2 at least.
- Files: `eval.rs` (default), examples (default), `docs/decisions.md`, `docs/progress.md`.
- Perf rule here: no cost-matrix row worse than +0.5% in `Auto` against the last `Tree` main, with both allocators. Run-once rows (`scripts_load`, `webpack`, `parse`-heavy rows) are the risk: tiering exists to keep them flat. If one still fails, raise the compile threshold (compile at the third call) and measure again. Rows that fail with no fix are listed in the PR and need a decision from Arjun.
- Check: `type_load` instructions for each key, RSS at 20 and 50 keys, `eval_load` rows, test262 baseline, all `*_check.sh`.

**B12. Registers for uncaptured locals (stage 2).** Size L. Depends on B11.
- Files: `bc_scope.rs` (capture bit), `eval_scope.rs` (the layout rule: no `Env` for a scope with no captured binding, for both engines), `bc_gen*.rs`, `bc_vm.rs`.
- Check: the scope tests and 60 `scope_*` cases pass in all three modes. The debug resolver cross-check still runs. Expected: 20 to 30 M for each key less, fewer `Env` objects (the `envs` row of `MEM=1`), `calls 10` and `fib 10` lower again.

**B13. Keep the tree walker as tier 0.** Decided by Arjun on 2026-11-07. Size: none (no work).
- The tree walker stays for code that runs once. It is not removed. `Auto` mode keeps it as the first tier (step B11).
- Consequences: `eval_scope.rs`, the frame-slot code, `eval_statements.rs`, the expression walker, `eval_async.rs` and `eval_generator.rs` stay. The gates run in all three modes (`tree`, `auto`, `byte`) for as long as both engines exist. `BARE_JS_EXEC=tree` stays as the differential reference. The tree is not dropped after compile (see section 7).
- `compile_load` (step B10) still reports compile time against parse time. It sets the compile threshold of `Auto`, and it does not decide a removal.

### Lanes and conflicts

| Lane | Files it owns | Can run with |
| --- | --- | --- |
| T0 | `tools/`, one example | all |
| C1, C2 | `eval.rs` (`funcs`), `eval_statements.rs` | A (merge C1 first) |
| A0 to A7 | `heap.rs`, `heap_shape.rs`, `heap_*.rs`, the member and assign parts of `eval.rs` | B0 to B2, B5 (new files), T0 |
| B0, B1, B2 | new `bc*.rs` files, `lib.rs` (one line) | A all, T0 |
| B3, B4 | `bc_vm.rs`, `call_script` and `Interp` in `eval.rs`, `eval_scripts.rs` | A (small textual conflict in `eval.rs`) |
| B5, B6 | new `bc_gen_*.rs` | each other (different files), A |
| B8, B9 | `bc_vm_gen.rs`, dispatch hooks | A |
| B12 | `eval_scope.rs`, `bc_scope.rs` | A (no overlap) |

Rules:
- Only one engineer works in `heap.rs` at a time (A0 to A7 are a chain).
- `eval.rs` includes about 30 other files in one module. Two lanes touch it: A2 to A4 (the member, assign, literal and closure code) and B3 and B4 (`call_script`, `Interp` fields). Add `ics: IcState` (A0) and `bc: BcState` (B0) as single fields first. Then each lane edits its own sub-struct.
- The new `bc*.rs` files are separate modules of the crate with `pub(crate)` access to what they need. They are not added to the `include!` blob. This keeps the compile time of `eval.rs` and the code layout of the walker steady (layout noise of +-1% is documented in docs/progress.md).

Order (a suggestion for two or three engineers):
1. Week 1: T0, C1, A0, B0 in parallel.
2. Weeks 2 to 4: A1 (one engineer), B1 and B2 (one engineer).
3. Weeks 4 to 6: A2, A3, A4 (the A engineer), B3 and B4 (the B engineer).
4. Weeks 6 to 12: A6, A7 (optional), B5 and B6 (two engineers if available), B7, B8, B9.
5. Weeks 12 to 14: B10, B11. Then B12. B13 is decided: the tree walker stays.

### Measured checks by milestone

Targets are estimates from section 2. A step passes when its own number moves as listed and no gate fails.

| After | Instructions for each key (`#a1`) | RSS at 50 keys | Other check |
| --- | ---: | ---: | --- |
| main today | 414 M | 36.1 MiB | baseline rows in section 1 |
| C1 | 404 to 408 M | same | `closures 10`, `class_load construct 10` not worse |
| A1 | 414 M +-0.5% | 32 to 33 MiB | creation rows flat; property tests |
| A2 | 365 to 385 M | +0.1 to 0.3 MiB | hit counters; `objects 10`, `proxy reactive 10` lower |
| A3 and A4 | 345 to 365 M | same | `closures 10`, `class_load` lower |
| A6 and A7 | same or lower | flat in the soak | miss rate under 10% |
| B3 (switch on) | not yet measured on the page | | `loop 10` and `fib 10` 3x or better |
| B10 (byte, with A) | 200 to 250 M | +0.5 to 0.8 MiB | fallbacks = 0; compile time recorded |
| B11 (Auto, default) | 190 to 240 M | +0.5 to 0.8 MiB | no row worse than +0.5%; test262 holds |
| B12 | 165 to 215 M | -0.2 to -0.5 MiB | `envs` count falls |

## 5. Correctness gates

Every PR runs these. Each gate has a command.
- G1. `cargo test --workspace` in debug (the resolver and cache cross-checks are on) and `cargo test --workspace --release`. Today about 2,400 tests.
- G2. Every `tools/js/*_check.sh` against Node (41 scripts: eval, statements, sc, class, subclass, generator, async, promise, iterator, proxy, reflect, regexp, typed, binary, bigint, date, number, math, intl, and the others). `parse_check.sh` needs `ACORN`. The fixtures are the 76 files in `tools/fixtures/js/`.
- G3. The cost rule: `tools/js/cost_matrix.sh BASE NEW` (T0). No row above +0.5% under both glibc and jemalloc. Rows move by up to 2% with unrelated changes under glibc (the thread cache, docs/resources.md), so the script uses the tcache setting and the rule needs both allocators.
- G4. `tools/js/typing/keycost.sh` (T0): instructions for each key and RSS. RSS budget 40 MiB at 50 keys.
- G5. test262 subset (T0): pass counts for each directory do not fall below the baseline. Run in `tree` mode for A steps. Run in `tree`, `auto` and `byte` modes for B steps from B3.
- G6. Differential harness `tools/js/exec_diff.sh` (B3): every `*-cases.txt` under `tree`, `auto` and `byte`; outputs must be equal. It also runs the load examples that have a checksum (`eval_load`, `gen_load`, `async_load`, `class_load`, `proxy_load`) in each mode.
- G7. Debug build: each inline cache hit is compared with the generic path. Each resolved name is compared with the full search (exists today).
- G8. UI and browser checks that use JS: `tools/ui` Chrome fixtures (`chrome_cases.mjs`, the handlers and form cases) run on main and on the branch. Same results.

Known gap in the gates: there is no test262 today. T0 adds it. Until T0 lands, A1 and B2 rely on G1, G2 and the property test.

## 6. Risks

- R1. Bytecode alone is not faster. Boa has bytecode and lost to bare-js on 6 of 8 rows. Control: B3 reports `loop`, `calls`, `fib` before more work goes in. Stop and re-plan if `byte` is not 3x on `loop 10`.
- R2. Run-once code pays compile cost. Control: `Auto` tiers. B10 measures it. B11 has an explicit rule.
- R3. A wrong cache gives a silent wrong answer. Control: G7, the property test, the epoch rules written once in `docs/decisions.md`.
- R4. `heap.rs` and `eval.rs` merge conflicts. Control: the single owner rule and the two sub-structs.
- R5. Code layout noise (+-1% on unrelated rows). Control: G3 with both allocators; a row that fails only by layout is explained as in earlier PRs.
- R6. A function with more than 255 registers or an unsupported node stays on the tree walker. Control: the fallback counter must be 0 on the corpora before B11.
- R7. Two engines share one scope layout in stage 1. A layout change on one side breaks the other. Control: B1 test compares every site; B12 changes both together.
- R8. The type page is one page. The live Next.js page was not run. A gain on the React form may not carry over. Control: report the `scripts_load`, `webpack`, `select_load react` rows with every step.

## 7. Deferred, with the reason

- 8-byte `Value` (NaN boxing). `Value` is a public 16-byte enum used in all code. Gain: half the register, slot and element memory. Cost: very large refactor. Revisit after B12 with a measured slot share.
- A smaller object handle. `ObjId` is `{index, gn}` and a read goes through `meta[index].dense` then `live[dense]`. This is part of the 164 instructions of `lookup`. After A2 most reads skip `lookup`. Measure then.
- Dropping the syntax tree after compile (reparse a function from source on first run). Saves 2.4 to 4 MiB on this page. Parse is fast (0.030 s for 1.7 MiB). Not planned: the tree walker stays (step B13), so the tree stays.
- Per-prototype root shapes (V8, JSC and Boa put the prototype in the shape). The plan checks `obj.proto` in the cache instead. Revisit if proto-hit sites are polymorphic over many prototypes.
- Garbage collection at safe points inside a long call. B makes it possible (precise register stack). Native Rust locals in builtins are still not rooted, so the depth-0 rule stays for now.
- Superinstructions and a baseline JIT. Not before the op mix of the real page is measured.

## 8. What was not measured

- Wall time. Only instruction counts are used here.
- jemalloc rows. All rows use glibc with the tcache setting. The cost rule needs both. T0 adds the jemalloc run.
- Any prototype of a shape, an inline cache or a bytecode loop. Every gain in sections 2 and 4 is a hypothesis computed from counts: 330 instructions for each property read, 59 for each `eval_expr_inner` call, 230 to 300 for each node visit, 48 bytes for each property entry. None was run.
- The hit rate of an inline cache on this page. No counter exists. A2 adds it. The 85% to 95% range is a guess.
- How many member sites are monomorphic. How many functions of the page run once, twice, or often.
- Bytecode size and compile time. No compiler exists. The numbers use the parse time of the staging chunks (0.030 s for 1.7 MiB) and a guess of 1x to 2x.
- The `#f0` input field (only `#a1`, the textarea, was profiled). The live Next.js page. A page with server components.
- `eval_load` and the other rows at scale 100. The `handler`, `xhr`, `sticky`, `wrapper`, `numeric` and `regexp` rows.
- test262 on bare-js. The size of the checkout and the pass rate.
- The Hermes and QuickJS inline-cache behavior (the fetched pages did not say; QuickJS `OP_get_field` body not read).
- The caller of `HashMap::contains_key`. The frame-pointer build lost its call chain. `is_callable` (`eval.rs:1235`) is the only hot use found by reading the code. Confirm in C1.
- The cause of the 700 instructions for each `enter_rare` call.

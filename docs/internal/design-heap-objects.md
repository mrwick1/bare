# Design: object allocation and property add on the bytecode lane

Status: proposal for owner review. No engine code changed. Base: `f2adb72194` (branch `j1-lazy-bytecode`).

## Summary

The heap has shapes, inline caches and a slab already. The lane does not use all of them.
- Object literals on the lane run `NewObject` and then one `DefField` per key. Each `DefField` costs about 630 instructions. The tree walker has a per-site literal shape that avoids this. The lane has no such path.
- A `SetField` that adds a property has a cache (`IC_ADD`), but the lane reaches it only after it builds the key and runs three checks out of line. One add costs about 470 instructions. An existing-slot store costs about 118.
- An object with more than 3 properties moves its values to a `Vec` and grows it 6, 12, 24. Every growth is a `realloc`.

The recommended design removes whole kinds of work: the key lookup on a cache hit, the transition and define per literal key, and the `realloc` chain. It does not touch interpreter dispatch. Dispatch is the larger cost of a field copy (see "Measured profile") and belongs to the JIT work.

## 1. Measured profile

Machine: this host (archMachine), idle for instruction counts. Build: release, `debug=line-tables-only`, separate target dir. Inputs: the fixture and the micro case from `baseline-jit-design:spike/jit-cranelift/measure/`, the public React page. Instruction counts are `perf stat -e instructions:u` (minimum of 3) or callgrind `Ir`. Spread between the 3 runs was not recorded; callgrind numbers are exact for a given run.

### 1.1 Cost by kind of work (`BARE_JS_EXEC=byte`, instructions per iteration, loop cost subtracted)

Method: the same loop at 1,000 and 11,000 iterations, difference divided by 10,000. The empty loop costs 131 per iteration on the lane and 815 on the tree walker.

| Work | Lane | Tree walker |
|---|---:|---:|
| `{}` (allocate an empty object) | 230 | 576 |
| `{a,b,c}` per key | 528 | 235 |
| `{a..h}` (8 keys) per key | 648 | 257 |
| 16-key literal, per key | 628 | 211 |
| `new F()` with an empty body | 1,345 | 1,608 |
| `this.k = 1` that adds a property, 16 adds (per add) | 468 | 854 |
| `this.k = 1`, 3 adds (per add) | 399 | not measured |
| field copy (`GetField` + `SetField`, existing slot) | 236 (about 118 per op) | not measured |

The tree walker's literal is cheaper per key than the lane's because it uses `object_shaped` (`eval.rs:3204`, `new_object_with_shape` in `heap_shape.rs`). The lane never calls `new_object_with_shape`.

### 1.2 Where one add and one define go (callgrind, 176,000 adds or defines)

Add by `SetField` (`IC_ADD` site, 16 fields per object), instructions per add:

| Part | Instructions |
|---|---:|
| `bc_loop` self (dispatch, failed `ic_store`) | 144 |
| `set_ic` (the add hit) | 89 |
| `bc_set_field` (key, `ic_refuses`, `has_exotics`) | 68 |
| `name_key` and `name_str` (key built before the hit) | 56 |
| malloc, realloc, free, libc | 60 |

Define by `DefField` (literal of 16 keys), instructions per define:

| Part | Instructions |
|---|---:|
| `define_own_property` and its inlined parts | 188 |
| `Interp::def` | 46 |
| `bc_def_field` | 47 |
| `name_key` and `name_str` | 45 |
| `check_key`, `push_slot`, key search (`find`, `Table::probe`) | about 70 |
| realloc and libc | about 45 |

The `DefField` path has no inline cache at all. Each define searches the shape for the key, then looks up the transition.

### 1.3 Field copy (`cwip_micro.js`, 20,000 iterations of 16 copies)

Callgrind total 78.3M. The copy loop costs 3,778 per iteration, 236 per copy. Of the total, `bc_loop` self is 64%, the IC code inlined into `bc_loop` (`heap_shape.rs`) is 14%, slice indexing 6.5%. Heap functions out of line are under 1%. The hit itself (`ic_own`, `ic_store`: handle check, shape compare, slot access) is about 28 instructions per op. About 90 per op is dispatch, operand decode, register and `ics[cc]` bounds checks, and the 24-byte `Ic` copy (`ic.get()`). **A heap redesign cannot make this case much faster.** Only a smaller dispatch or a JIT can.

### 1.4 Share of heap work

Method: callgrind self cost, grouped by function name with a regular expression (`/data/claude-tmp/scratch-heap/cat.py`, not committed). The groups are approximate. "Heap work" is: define, alloc and transition; property slow paths; malloc, free and libc; strings and keys; GC.

| Input | Define/alloc | Property slow | malloc/free | Strings/keys | GC | Heap total |
|---|---:|---:|---:|---:|---:|---:|
| Fixture, 6 renders, lane only (240.6M) | 14.6% | 10.8% | 10.1% | 4.8% | 0% | 40.3% |
| Micro (78.3M) | 0.5% | 0% | 3.6% | 0.6% | 0% | 4.7% |
| Page, load only (613.7M) | not split | | | | | |
| Page, typing, 10 keys minus 0 keys (2,696M, 269.6M per key) | 3.3% | 5.9% | 4.8% | 4.0% | 1.2% | 19.2% |
| Page, load and 5 keys (1,985M) | 3.5% | 5.7% | 7.3% | 4.6% | 1.1% | 22.2% |

Important: the page in `byte` mode still runs 68% of its typing cost in the tree walker (`eval_expr_inner`, `link_inner`, `machine`, `get_name`). Only the fixture is lane-only. The 40% heap share holds for code that runs on the lane. It is 19% of the whole page today. It will rise as lane coverage rises toward 90%. So a heap change gives little on the page now and more later. This is the honest limit of the expected gain on the page.

## 2. Current design and cost

Code: `crates/js/src/heap.rs`, `heap_shape.rs`, `eval_ic.rs`, `bc_vm.rs`.

**Object record.** `Object` is at most 104 bytes (`heap.rs:432`). It lives in a slab (`Slab<Object>`: `meta`, dense `live` vector, `back`, `free`). A handle is `(index, generation)`. Every access checks the generation.

**Slots.** The values of named properties are `repr: Repr`. `Repr::In([Value; 3])` holds up to 3 values inside the record. The fourth value moves all values to `Repr::Heap(Vec<Value>)`. `spill` allocates capacity 6. The `Vec` then grows by doubling (12, 24). A `Value` is at most 16 bytes. A 16-field object does 1 malloc and 2 reallocs. It reserves 24 slots (384 bytes) for 16 values (256 bytes) and the record is 104 bytes. Accounting (`obj_bytes`) counts length, not capacity.

**Shapes.** A shape is a node in a transition tree (`Shape`: `parent`, `count`, `nslots`, `table`, one inline `child`, optional `children` map). Keys of a chain share one `Table`. A table over 8 keys has a lazy hash index. `Shapes::add(from, key, attrs)`: test the inline first child (O(1)); else `recent` (256 entries, direct-mapped); else the children `HashMap`; else create. A property lookup in a shape is a scan up to 8 keys, else a hash probe. Native reads use `found` (256 entries).

**Add by define.** `define_own_property` calls `add_data_fast` first: it needs a full data descriptor, an ordinary shape-mode object, and `find(shape, key)` to miss; then `check_key`, `check_value`, `Shapes::add`, `charge`, `push_slot`. If `ic_chain` is set on the object, it takes the general `ordinary_define` path (validate, `own`, `add_cost`, `edit_with`, `add_prop`). Cost per define is 190 for the heap part and about 630 with the lane around it.

**Add by assignment.** `SetField` runs `ic_store` inline. On a miss it calls `bc_set_field`, which calls `name_key(n)` (interned string to key), checks `ic_refuses` and `has_exotics`, copies the `Ic`, and calls `set_ic`. `set_ic` tests `IC_ADD`, then `ic_add`: handle check, shape equal, `extensible`, `lazy`, `ic_chain`, epoch, prototype equal, `check_value`, `charge`, store the new shape, `push_slot`. The first fill (`set_fill`) walks the prototype chain and marks each prototype `ic_chain`. A `IC_ADD` entry is 24 bytes. Cost: about 470 per add, 16-field object.

**Object literal on the lane.** `bc_gen_expr.rs:object`: `NewObject`, then for each key: evaluate value, `DefField dst, t, k`. `NewObject` is `heap.new_object(Some(object_proto))`: one slab slot, no reserve.

**Object literal on the tree walker.** The site keeps the final shape in `expr_name`. Next runs evaluate values and call `new_object_with_shape(proto, shape, slots)`: one allocation, no transition, no define. The shape stays valid until a shape sweep (the evaluator resets entries when `Heap::shape_sweeps` changes).

**`new F()`.** `bc_call` collects arguments into a pooled `Vec`, then `construct` → `construct_inner`: reads `prototype` with `get_prop`, `new_object(proto)`, `call_script`. Measured: 1,345 per call with an empty body, more than 5 times a `{}`. Most of it is call and construct machinery, not the heap. It is outside this design, except the allocation.

**GC.** Mark and sweep over the slab. Not a part of this cost today (1.1% on typing).

## 3. What real engines do

I read the following. A fetch tool summarised each page. For two files the tool read only the first 100,000 characters; those limits are named.

- **V8, hidden classes and in-object properties.** https://v8.dev/blog/fast-properties : in-object properties "are stored directly on the object themselves" and "are the fastest properties available"; overflow goes to the properties store; each new named property creates a new HiddenClass in a transition tree; the in-object count "is predetermined by the initial size of the object".
- **V8, expected property count and slack tracking.** https://raw.githubusercontent.com/v8/v8/main/src/objects/js-function.cc : `EnsureHasInitialMap` calls `CalculateExpectedNofProperties`, which sums `shared->expected_nof_properties()` over the constructor chain. If the sum is above 0 it adds 8 and caps at `kMaxInObjectProperties`; the comment says "Inobject slack tracking will reclaim redundant inobject space later, so we can afford to adjust the estimate generously." It calls `map->StartInobjectSlackTracking()`. The file does not show how `expected_nof_properties` is computed, and the shrinking is in `Map` (not read).
- **V8, transitioning stores.** https://raw.githubusercontent.com/v8/v8/main/src/ic/ic.cc (first 100,000 characters): `StoreIC::LookupForWrite` calls `PrepareTransitionToDataProperty` and returns `IsCacheableTransition()`. `ComputeHandler`, case `TRANSITION`, returns `StoreHandler::StoreTransition` (or `StoreOwnTransition` for define) with `lookup->transition_map()`. `SetCache` records the pre-transition map and the handler in the feedback vector. This matches our `IC_ADD` (shape S, new shape S'). The handler layout is not in the part I read.
- **V8, object literals.** https://raw.githubusercontent.com/v8/v8/main/src/runtime/runtime-literals.cc : `CreateLiteral` builds a boilerplate in the `AllocationSite` on the second visit, then `DeepCopy` produces each new object with `CopyJSObjectWithAllocationSite` (copies map and fields). Nested object literals get no allocation site. So V8 allocates a literal in one copy, with the final map. This is the design of option B1 below.
- **SpiderMonkey.** https://raw.githubusercontent.com/mozilla/gecko-dev/master/js/src/vm/PlainObject.h : `PlainObject::createWithShape(cx, shape, kind, newKind)`, `createWithTemplate(cx, templateObject)`, `NewPlainObjectWithAllocKind` ("allows creating an object with fixed slots available for properties"), `ThisShapeForFunction` ("the shape to use when creating |this| for a known function callee"). The header has no comments on template objects. Fixed slots come from the `AllocKind`. I did not read the JIT side.
- **JavaScriptCore.** https://raw.githubusercontent.com/WebKit/WebKit/main/Source/JavaScriptCore/bytecode/AccessCase.h : access types `Transition` and `Replace`; for a transition, `m_structureID` holds the new structure and the old one is reached by `previousID()`; `createTransition` takes old and new structure. This is the same pair as our `IC_ADD`. The page has no text on inline capacity. The `StructureStubInfo.h` URL gave 404, so I read nothing about stub info.
- **QuickJS.** https://raw.githubusercontent.com/bellard/quickjs/master/quickjs.c (first 100,000 characters only): shapes are hashed in a runtime-wide table (`shape_hash`, `is_hashed`); `JSObject.prop` is a separate array; capacity starts at `JS_PROP_INITIAL_SIZE 2` and grows by 1.5 (`js_realloc_array`). QuickJS has no in-object slots and no pre-sizing in the part I read. I did not read `add_shape_property`. It is the simple scheme, and it is slower than ours.
- Hermes: not read.

Summary of the pattern: engines cache the transition at the store site (V8, JSC, ours), and allocate an object with its final map and size (V8 boilerplate, V8 expected count, SpiderMonkey template and alloc kind). Our engine has the first half in the tree walker and partly in `IC_ADD`. It lacks the second half on the lane.

## 4. Options

### A. Add-property cache (exists: `IC_ADD`). Improve its use on the lane.
- Data: the existing 24-byte `Ic`.
- Change: let `SET_FIELD` try `ic_add` inline after `ic_store`, before `bc_set_field`. The key is not built on a hit.
- Invalidation: unchanged (shape, prototype and `epoch`, `ic_chain`, `extensible`, `lazy`).
- Experiment (throwaway, scratch copy, not committed): a 4-line edit that calls `ic_add` in `SET_FIELD`. 16 adds: 8,966 → 6,423 per object (−2,543, −28%; −159 per add). 3 adds: 2,674 → 2,195 (−160 per add).
- Risk: code size of `bc_loop`; the `ic_add` body is larger than `ic_store`. Measure the cost matrix.

### B1. Pre-shaped object literal on the lane
- Data: a per-site cell with the final shape (like `expr_name` in the tree walker; on the lane a cell in `code.ics` or a new `lits` table). New op `NewLit dst, base, n` with the `n` values in consecutive registers `base..base+n`.
- Algorithm: the generator evaluates the values into registers in source order, then emits `NewLit`. First run: build with `NewObject` and `define` for each key, then read `literal_shape`; store it in the site. Later runs: `new_object_with_shape(object_proto, shape, &regs[base..base+n])`. One check per value (`check_value`), one slab allocation, one slot copy.
- Applies only to literals of named keys, no duplicates, no `__proto__:`, no getters, setters, spread or computed keys (the tree walker has the same test, `eval_object`). Other literals keep `NewObject` and `DefField`.
- Order of effects: values are evaluated before the object exists. No code can observe the new object before the literal finishes, except through a getter or spread (excluded) or a computed key (excluded). The tree walker already does this.
- Invalidation: a shape sweep (`Heap::shape_sweeps` changes) resets all sites. `new_object_with_shape` returns an error if the shape does not match the slot count, as today. A literal is a define, not a set, so a setter on `Object.prototype` does not matter and needs no epoch.
- Complexity: O(n) per literal with no search, no hash, no realloc beyond the one `Vec` for n > 3.
- Memory: unchanged for n ≤ 3. For n > 3 the `Vec` is exact (`to_vec`): 16 keys use 256 bytes in place of 384.
- Expected gain (hypothesis, not measured on the lane): the lane pays about 628 per key today. The tree walker, with its own dispatch, pays 211 to 257. On the lane the per-key cost should be a register load and a 16-byte copy plus `check_value`: I expect under 100 per key. For a 16-key literal that is about −8,000 of 10,280 per literal. The fixture spends 14.6% in define/alloc/transition. I expect to remove most of that and part of the 4.8% strings and keys (the `name_key` call per `DefField`). Hypothesis: −12% to −18% on the fixture.

### B2. Constructor pre-sizing
- Data: a `u8 slots_hint` on the function record (where `Callee::Script` lives), set to the maximum slot count seen at the end of a construction (monotonic, capped at `MAX_PROPS`).
- Algorithm: `construct_inner` calls `reserve_props(obj, hint)` after `new_object`. `reserve_slots` already allocates one exact `Vec` when `n + len > 3`.
- Result: 1 malloc in place of 1 malloc and 2 reallocs for 16 fields. This is V8's `expected_nof_properties` idea. No slack tracking is needed because our slots are not in-object: a too-large hint wastes only capacity of one `Vec`, and the hint never shrinks.
- Experiment (throwaway): changed `spill` capacity from 6 to 16 in a scratch copy. 16 adds: 6,423 → 5,620 (−803, about −50 per add). A 16-key `DefField` literal: 10,411 → 9,616 (−795).
- Memory: lowers capacity for objects that grow past 6: 384 → 256 bytes for a 16-value object. It raises it for objects that stay at 4 to 6 values only if the hint is too high; the hint is the observed maximum, so it is right for the next object of the same constructor. Accounting counts length, so limits do not change.
- Invalidation: none. It is only a capacity hint.
- Risk: a constructor whose objects differ a lot in size (a hint of 64 on an object that gets 4). Mitigation: set the hint from the last construction, not the maximum, or cap it at 32. To decide with data from the page.

### C. Copy by shape (same-shape slot copy)
- Data: none new. Algorithm: when source and target share a shape, copy `slots` in one `copy_from_slice`.
- Where it applies: `Object.assign`, spread and `{...o}` of an ordinary object with the same shape. Does not apply to `createWorkInProgress`: its 16 copies are 16 separate `GetField`/`SetField` pairs. The bytecode does not say they copy a whole object, and the target may have another shape or extra keys.
- Gain on the three inputs: micro 0 (it runs no spread). Fixture and page: not measured. I did not count `Object.assign` or spread calls. The refusals list shows `spread` (6,019 calls) is a lane refusal today, so the lane cannot use it yet.
- Verdict: do not build now. Revisit when spread is on the lane and a profile shows it.

### D. Slab or bump allocation of objects and slots
- Objects are in a slab already (`Slab<Object>`, dense `live` vector, free list). `alloc_object_cap` costs 841K for 11,000 allocations in callgrind (about 76 per object, small).
- The remaining malloc is the slots `Vec` of objects with more than 3 values: about 60 per add today (16-field case). B2 reduces it to one malloc per object.
- A bump arena for slots would need a copying GC or per-size free lists, a big change with the JIT's layout risk. Not recommended before B2 data. Hypothesis: after A, B1, B2 the malloc share of the fixture falls from 10% to about 3%.

### Comparison

All gains are instructions per operation on the lane, `byte` mode. "Measured" means I ran it (experiment, not committed). Others are hypotheses.

| Option | Removes | Fixture (hypothesis unless noted) | Micro | Page typing today | Risk |
|---|---|---|---|---|---|
| A. inline `IC_ADD` hit | key build, 3 out-of-line calls per add | `new`-heavy part: −28% of add cost (measured on 16-add micro) | 0 | small (hypothesis) | `bc_loop` size |
| B1. pre-shaped literal | transition, define, key per literal key | −12% to −18% (hypothesis) | 0 | small (hypothesis) | fallback and sweep reset |
| B2. constructor pre-size | 2 reallocs per big object | −5% of add cost (measured: −803 of 8,966 per 16-add object) | 0 | small (hypothesis) | wrong hint wastes capacity |
| C. copy by shape | per-key copy in assign and spread | not measured | 0 | not measured | needs spread on the lane |
| D. bump arena | malloc | about −7% (hypothesis) | 0 | small | GC and JIT layout |

## 5. Recommendation

Build A, B1 and B2, in this order. They share one idea: the site learns the final shape and size, and later runs skip the search. Together they remove the key build on every cached add, the transition and define on every literal key, and the `realloc` chain.

Do not build C or D now. Do not expect a gain on the micro: its cost is dispatch.

### Step plan

Every step has the same gates: `cargo test -p bare-js --lib` passes, clippy clean, `tools/js/exec_diff.sh target` gives `differences: 0`, cost matrix in `Auto` has no row over +0.5%, and the numbers below. Each step adds a test that fails first (the pattern in `a_lazy_function_runs_on_the_bytecode_lane`).

1. **A: inline add hit in `SET_FIELD`.** Cost model of the gate: `this.k = 1` add, 16 fields: from 8,966 to at most 6,500 per object (measured bound 6,423). Test: a site that adds a key, then the prototype gets a setter for that key (the next store must call it), then a frozen receiver, an object with `ic_chain`, and an array `length`.
2. **B2: constructor pre-size.** Gate: 16-add object at most 5,700 (measured bound 5,620). Also report peak memory (RSS) of the page and a 1,000-object test. Limit: no row of the matrix over +0.5%, no increase of peak memory on the page. Test: objects of one constructor with different final sizes; limits still count length.
3. **B1: lane pre-shaped literal.** Gate: 16-key literal from 10,280 to at most 3,000 (hypothesis); fixture instructions at least −10% (hypothesis); page typing and load not worse. Test: literal with a duplicate key, `__proto__:`, a getter, a spread, a computed key, a number key, and a shape sweep between two runs.
4. **Re-measure.** Profile the fixture and the page (5 keys) again, in the same split as section 1. Record in `docs/progress.md`. Only then decide on C and D.

Record the fixture and page numbers at the frozen base and after each step (3 runs each, with spread), as `CLAUDE.md` says.

## 6. Risks

- **Gain on the page is small today.** Typing runs 68% in the tree walker. Heap work is 19% of typing. A 15% cut of the lane share gives a small gain on the page until more code runs on the lane. The fixture gain does not predict the page gain.
- **`bc_loop` size.** A and B1 add code to the hot loop. Code size and register pressure can slow other ops. The cost matrix is the gate.
- **B1 order of evaluation.** Values are evaluated before the object exists. It is safe for the literals it covers (no getter, spread, computed key). The tree walker does the same today.
- **Shape sweep.** B1 sites hold shape ids. A sweep must reset them. The tree walker has this rule; the lane needs the same check.
- **JIT layout.** `Repr::In` or `Repr::Heap` is a branch on every slot access, and every access checks the slab generation. A JIT will need one shape compare and one slot load. A uniform slot pointer, and the `extensible`, `lazy` and `ic_chain` flags folded into the shape check, would let a JIT inline `IC_ADD` as a few instructions. This is a hypothesis for a later step. This design does not block it.
- **Hint waste (B2).** A constructor with very different object sizes gets a hint that is too high for some objects. Use the last or a capped value; measure on the page.

## 7. Not measured

- B1 on the lane (only the tree walker's number and an estimate). No experiment of a lane literal.
- The page in `Auto` mode, at 50 keys. The page profile is in `byte` mode at 10 and 5 keys.
- Wall time, peak memory (RSS), and the cost matrix for the experiments.
- GC cost of the allocation changes.
- Spread of the instruction counts over repeated runs (minimum of 3 only). Callgrind numbers come from one run each.
- Copy by shape on `Object.assign` and spread (no counts of these calls).
- The shape of constructor sizes on the real page, and how often `new F()` and literals occur there.
- The share of `hash` or `Table::probe` on large tables (more than 8 keys) on the page.
- Hermes. V8 `Map` slack tracking code, V8 handler layout, JSC `StructureStubInfo`, SpiderMonkey JIT code, QuickJS `add_shape_property`.
- The group split of section 1.4 is by function name. Some "other" cost (7%) may belong to heap work.
- Windows.

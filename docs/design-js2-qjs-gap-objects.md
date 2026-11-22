# Reduce property and array work in js2

## 1. Measurements

The decision benchmark reports 1,023.58 M user instructions for js2 and 507.64 M for direct QuickJS on `objects_10`. The script makes one ordinary object and one array. It adds 997 named properties, overwrites them 199,003 times, enumerates and reads them once, appends 200,000 integers, and calls one function 200,000 times. The checksum is `198902497,1199980`. A `forEach` call creates one callback, which all iterations share. [Decision benchmark](progress/2026-11-21-engine-decision-benchmark.md)

I used the `eval_load objects 10 --source` input saved by the decision run as `objects_10.raw.js`. Its SHA-256 is `fa7d7b1f70119468d1d234ccd504baf8800040d5ad7b02c614202384c75b0167`. The js2 decision runner calls `Scope::run_script` and prints the completion value. The C runner calls `JS_Eval` with `JS_EVAL_TYPE_GLOBAL`. Both have the same source and check the completion value. The control evaluates `0`.

The machine is archMachine, AMD Ryzen 9 5900HX, Linux `7.2.15-zen1-1-zen`. I used Rust 1.96.0, the release profile with debug symbols and one js2 codegen unit, and QuickJS 2026-06-04 with GCC 16.2.1, `-O2`, and LTO. The js2 checkout began at `f29a583d`. The later snapshot applies the engine changes in `48d03db9` from #908 and #909. Every sample used a fresh process with default glibc. The table has the minimum and maximum of three `perf stat -e instructions:u` runs. Each count had at least 99% coverage.

| Engine | Empty, M | Objects 1, M | Objects 10, M | Objects 10 spread |
| --- | ---: | ---: | ---: | ---: |
| js2 `f29a583d` | 0.724405–0.724635 | 105.051–105.055 | 1,018.916–1,020.639 | 0.1691% |
| js2 after #908/#909 | 0.724137–0.725214 | 97.405–97.407 | 937.324–937.363 | 0.0042% |
| QuickJS direct | 1.064750–1.064935 | 53.474624–53.474880 | 512.456–512.458 | 0.0003% |

The first pair reproduced the published counts within 1%. The QuickJS binary links the LTO objects from the recovered harness to a copy of the decision runner. Absolute counts vary between the two builds.

I ran Callgrind 3.25.1 with symbols, `--cache-sim=no`, one run per input and engine. Each prefix ends after one more phase of the original script. The separate create control makes `{}` and `[]`. The add prefix limits the first loop to 997 turns. The write, enumerate and push prefixes stop at the end of their respective source lines. Subtract the preceding prefix to estimate each phase. Subtract the empty case from the first prefix. Prefix differences include changes in compilation, GC and teardown. The two outer loops retain the original top-level global accesses. Use the allocation and fixed-key controls below for narrower comparisons.

| Phase | Count | js2 original, M | js2 after #908/#909, M | QuickJS, M | Instructions per operation, same engine order |
| --- | ---: | ---: | ---: | ---: | ---: |
| Create control above empty | 2 | 0.031 | 0.031 | 0.024 | See allocation controls |
| Computed adds | 997 | 2.496 | 2.390 | 1.751 | 2,503 / 2,397 / 1,756 |
| Computed overwrites | 199,003 | 363.994 | 342.754 | 273.252 | 1,829 / 1,722 / 1,373 |
| Enumerate and read | 997 | 2.692 | 2.735 | 0.774 | 2,700 / 2,743 / 776 |
| Append with `push` | 200,000 | 522.112 | 460.456 | 133.107 | 2,611 / 2,302 / 666 |
| `forEach` including callback | 200,000 | 129.356 | 130.298 | 102.647 | 647 / 651 / 513 |
| Total above empty | | 1,020.681 | 938.663 | 511.554 | |

The two first-loop phases include loop control and key construction. Over all 200,000 first-loop turns the empty-subtracted write prefix uses 1,833, 1,726 and 1,375 instructions per turn. The for-in figure includes iteration and summation as well as the read. The forEach figure includes final string assembly.

### Bytecodes per turn

I counted js2 dispatches with a separate `op-count` build of the post-change completion runner and checked the top-level disassembly. One wide instruction is one dispatch. I counted QuickJS instructions from its `DUMP_BYTECODE=1` listing. Counts cover a successful turn and exclude the final failed loop test. Native routines do work between dispatches.

| Successful turn | js2 | QuickJS |
| --- | ---: | ---: |
| Computed add or overwrite | 13 | 19 |
| Enumerate and read | 9 | 11 |
| Append with `push` | 11 | 16 |
| Dense `forEach` with callback | 14 (10 loop + 4 callback) | 6 (callback) |

js2 counted 2,600,012 dispatches through the write prefix, 4,809,002 through the push prefix and 7,609,055 for the full script. The 200,000 wide dispatches encode the divisor 997. The dense forEach loop has four argument moves, a load, a test for undefined, a call, and loop control. Its `in` branch does not run on this input.

QuickJS uses 19 dispatches for each computed store and 16 for each push. Its stack code moves values and saves the statement completion. js2 already emits fewer dispatches for both loops. The excess in the push phase comes from work inside the native call. QuickJS runs its `forEach` loop in C and dispatches six opcodes per script callback. The callback updates a global in both engines.

## 2. Root cause

The main gap is the generic indexed property route inside `Array.prototype.push`. The append phase accounts for 389 M of the original 509 M Callgrind gap and 327 M of the 427 M gap after #908/#909. Each push looks up the missing index on `Array.prototype` and `Object.prototype`. The old path formats decimal text and interns it. #908 avoids creating an atom on a miss. It still formats the same index and probes the realm and shared atom tables. The lookup costs 278 M inclusive in the original and 218 M in the post-change push prefix.

js2 also pays for general receiver and property checks, element storage, and a second length update. The whole native push routine costs 326 M inclusive in the post-change push prefix. QuickJS uses one guarded direct append. Small object headers and shape additions account for little of the gap. The remaining phases account for about 70 M in computed writes, 2 M in for-in and named reads, and 28 M in forEach.

### Direct append experiment

I added a local branch to `push` for a plain array, one argument and a small current length. It called `Obj::set_element` once at the current length, then returned the new length. The probe assumed the frozen input's unmodified prototypes. It kept element allocation and growth. The production design requires a guard for indexed prototype properties.

| Post-#909 engine | Objects 10, perf M across three runs | Push phase, Callgrind M | Objects 10 RSS, median MiB |
| --- | ---: | ---: | ---: |
| Baseline | 937.324–937.363 | 460.456 | 16.86 |
| Direct append probe | 634.514–634.542 | 159.236 | 16.88 |

The probe removes 302.810 M hardware instructions with the original checksum. Its Callgrind push phase uses 796 instructions per turn and the for-in prefix changes by less than 0.001 M. It closes 71% of the post-change hardware-counter gap. Peak RSS stays in the same range. Both runs allocate the same element records and computed key strings.

### Short key experiment

I added a second local probe on top of direct append. For a one-byte string followed by an integer from 0 to 999, it formatted into a four-byte buffer and looked up or created the atom before returning the result of addition. The normal property instruction still converted the value and checked the receiver. The probe interned matching addition results inside and outside property expressions. The implementation should select the path only for key use.

This probe cut another 31.848 M hardware instructions. The three full runs gave 602.666–602.790 M with the original checksum. The write prefix fell from 345.934 M to 314.098 M in Callgrind. Median peak RSS fell from 16.88 to 10.64 MiB, with a 10.64–10.71 MiB range. The trigger stayed at its default. This result supports removing temporary keys before changing the object layout. I reverted both probes.

### Ordinary object creation and adds

`NewObject` uses a realm root shape. `CellHeader` is 8 bytes. The ordinary root asks for four inline slots. Alignment makes it five slots and a 48-byte cell. Exact small literal shapes can use less space. The plain indexed array root uses a 32-byte cell with an elements pointer word. `Obj::new` takes a size-class free-list cell and initializes its header and value words. Allocation is O(1) on a nonempty list and initializes O(inline capacity) words. On refill, the allocator links the cells of a 64 KiB block.

`Obj::add` reserves slots, looks up a transition and installs the resulting shape. The first cached child uses an edge comparison. Branches use a hash map. A new linear transition appends to a shared descriptor table. A divergent branch may copy its prefix in O(property count). The 129th property converts this object to a dictionary. The table then adds entries in place and keeps slot numbers stable. The conversion copies the prefix once. Later adds probe and append in amortized O(1). Slots grow by 1.5×. Growth copies O(capacity) values and the old record becomes garbage.

In the original js2 add prefix, `Obj::add` and its callees use 0.41 M instructions for 997 adds, including the dictionary switch and slot growth. The entire prefix costs about 2.50 M above the create control. QuickJS spends 0.26 M inclusive in `add_property` and about 1.75 M above its create control. This bounds the effect of the shape transition gap on the full row. Existing writes are 99.5% of the computed stores.

| Separate allocation control | js2 original, instructions per added object | QuickJS direct, instructions per added object |
| --- | ---: | ---: |
| Empty creation minus reusing a reference | 93 | 346 |
| Four-field literal minus reusing an object with four writes | 136 | 2,146 |

The empty-object control compares `o = {}` with `o = a` inside a function-local loop. Both return `typeof o`. The four-property control compares a fresh `{a:i,b:i,c:i,d:i}` with four writes into a reused object of that layout. Both read `o.d` and sum it. The latter difference includes literal setup versus normal writes. It gives a lifecycle cost for this control. For the empty control, the allocation routines alone account for 84 instructions per js2 object and 147 per QuickJS object, inclusive. QuickJS also frees old objects as it goes. js2 uses its `NewObjLit` recipe and reuses the final shape in the four-field control. QuickJS spends 219.9 M inclusive in `add_property` across that control. The result supports keeping small js2 objects inline.

### Repeated writes and reads

`SetElem` checks and converts the key before it calls `set_prop_g`. The plain named path in `set_named` checks for function or host preparation, probes the dictionary, checks attributes and writes the slot. `Obj::slot_ptr` reads geometry through the shape and selects the inline word or overflow record. `GetElem` takes the same named lookup on `o[k]`. For-in builds a key snapshot and checks the current property before it yields each key. Reads and updates use average O(1) table probes. Atomization hashes O(key length) units before any lookup. Slot access is O(1).

The original write prefix spends 119.2 M inclusive in js2 `add_slow` and 118.3 M in QuickJS `js_add_slow`. js2 spends another 97.3 M in `key_of`, including 66.3 M in `atomize`. QuickJS spends 43.1 M in `JS_ValueToAtom`. The call trees place that cost under `JS_SetPropertyValue`. Deduct it before comparing the rest of that helper with js2's 53.7 M in `set_prop_g`. These costs expose two separate opportunities. A key-only expression can avoid a temporary string. An own writable data hit can avoid the generic receiver checks and a second geometry lookup.

After #909 the concat helper falls to 95.1 M. The key conversion still spends 97.4 M, and the normal property store spends 56.5 M. With a single precomputed key and the same 997-property dictionary, subtracting setup gives 908 instructions per turn in js2 and 539 in QuickJS. That control includes global loads, loop control and key conversion. A second control compares summing `o[k]` with summing a preloaded global value. Above the preloaded case, each read expression uses 635 instructions in js2 and 288 in QuickJS. The difference includes the extra global load for the computed key. In the original write prefix, `slot_ptr` costs 8.8 M at its own source lines. Even removing the whole slot routine would leave most of the gap. Read-side work is small on this row and matters more on workloads that read repeatedly.

### Peak RSS

The published post-#908/#909 median is 16.87 MiB for js2 versus 6.22 MiB for direct QuickJS. The published empty processes use 5.27 and 3.05 MiB respectively. Peak RSS includes code pages and allocator state, and each peak can occur at a different phase. [js2 memory follow-up at the measured snapshot](https://github.com/mrwick1/bare/blob/48d03db9/docs/progress/2026-11-22-js2-memory-peaks.md) · [Decision benchmark](progress/2026-11-21-engine-decision-benchmark.md)

My builds reproduced 21.82 MiB before the changes and 16.86 MiB after. The post-change range was 16.64–16.88 MiB from three runs. My direct QuickJS median was 6.43 MiB with a range of 6.27–6.46 MiB. The empty controls were 4.95 and 3.17 MiB respectively. I used fresh copies and a static `wait4` parent. Logs, inputs, the two launcher sources and the prefix generator are in `/data/bare-tmp/r22/gap-objects/` on archMachine.

The inline header and named properties are small. Four slots stay inline after overflow and the last inline word holds the record pointer. The final slots record has capacity 1,066 and requests 8,536 bytes. It follows 13 smaller records with 17,160 bytes of requests in total. The dict's descriptors and index take tens of KiB. Cutting the ordinary object header would save bytes on one object in this row.

The array has a separate record of 8-byte values, an 8-byte header and two 32-bit words for length and count. At 200,000 pushes, the 28 geometrically grown records request 4,977,664 bytes in total. The live record requests 1,659,072 bytes. The previous records account for up to 3.16 MiB of requests before collection, plus block rounding. `make_record` copies the prior capacity and writes holes over the new tail. QuickJS grows a 16-byte value vector with `realloc`. It retains one current vector. The allocator can retain freed pages. Thus the js2 element grows generate more retained allocation volume before GC even though the live payload is about half the size. These are layout and growth calculations. Temporal overlap at the RSS peak needs direct measurement.

After #909, all 200,000 computed keys use one short string allocation each. Only 997 canonical key strings are needed by the object. The remaining 199,003 strings account for up to 6.07 MiB of 32-byte size-class cells before collection. Avoiding that allocation at a key-only site reduces pressure before any change to the trigger. The collector's trigger and the exact overlap between string and array backing peaks are separate measurements.

## 3. How other engines do the work

QuickJS stores a 64-byte `JSObject` with a shape pointer and a separate property pointer on this x86-64 build. A `sizeof` probe also gives 56 bytes for the fixed part of a shape, 8 bytes per shape descriptor, and 16 bytes per `JSProperty` and `JSValue`. An ordinary empty object allocates 64 + 32 bytes before malloc overhead. An array allocates its element vector when it first needs it. A hash bucket uses the low atom-ID bits. The matched descriptor index also indexes the value vector. The setter tests the descriptor flags and assigns a writable own data property in place. Average lookup cost is O(1).

`add_property` probes the runtime shape hash for a shared successor. On a miss, it clones a shared shape or extends a unique shape in place. A hit compares the descriptor prefix after the hash, prototype and count match. Growth reallocates the property vector and copies the shape table as needed. A unique shape does not require a separate descriptor table for every prefix of the 997-property chain. Object reference counts release unreachable acyclic instances, their property vectors, and short strings on the drop path.

`js_array_push` guards on a fast array, an extensible receiver, a clean default prototype chain or a null prototype, writable length and length equal to the element count. It grows the element vector only when needed. It copies the arguments and updates length and element count directly. The generic case still uses normal sets. `js_array_every` drives `forEach` in C and checks presence before reading each index. QuickJS represents small numeric property atoms as tagged integers, so that fallback need not format the index as text. See `quickjs.c` functions `find_own_property`, `JS_NewObjectFromShape`, `add_property`, `can_extend_fast_array`, `expand_fast_array`, `js_array_push` and `js_array_every`. [QuickJS 2026-06-04 source archive](https://bellard.org/quickjs/quickjs-2026-06-04.tar.xz)

V8 also keeps in-object fields for small objects. It shares descriptors for fast properties and uses a per-object named dictionary for slow properties. Its fast array built-ins use map checks and an indexed-prototype protector. The forEach witness reloads array state after a callback and resumes the general loop when a guard fails. This lets the common path avoid work while honoring mutations by the callback. [V8 property layouts](https://v8.dev/blog/fast-properties) · [V8 Torque built-in example](https://chromium.googlesource.com/v8/v8.git/%2B/refs/heads/main/docs/torque/user-manual.md)

## 4. Designs

### A. Remove generic work on proved common paths

Keep inline object slots, the shape tree and dictionary. Keep 8-byte tagged elements. Cache one realm-local proof about the stock array prototype chain, together with the shape prototype epoch. Watch both prototype shapes. Check the indexed state of `Array.prototype` on every hit, because dense element writes can leave its shape unchanged. Start with an empty and unmodified indexed prototype and bail out on host or exotic links. For push, check the receiver's prototype, appendability and index bound. Use one dense backing access, one value store and one length update. Grow in the existing allocator on a capacity miss. This is O(1) amortized per inserted value. A failed proof uses the generic path before any write.

Let the compiler select a key-only add for a computed member expression whose result only serves as a key. When its operands are a short Latin-1 string and a tagged integer, format the text into a fixed stack buffer. Intern a name from those bytes. Preserve the numeric string rules. Fall back to ordinary addition for other operands. Keep both addition and property key coercion in their existing evaluation order. This takes O(units), with O(1) stack space, and has no distinct value-string allocation on common hits. Keep one own descriptor probe for computed names. For an ordinary data hit, carry the shape used for that probe through slot address calculation. Reserve the general path for misses, accessors and special receivers. A keyed cache with a few entries has little reuse at a site that cycles through 997 names.

Use an interpreter step for iterating dense own elements. Return the next present value and index directly and leave callback entry on the interpreter stack. At a hole, read-only named override, invalid guard or non-array, perform ordinary HasProperty then Get in that order. Re-read receiver state after every callback. Work is O(length) for a dense array, plus prototype depth on a fallback. The step needs no retained element pointer or object field. Arrange the value, index and receiver in the callback argument registers if the compiler can prove that layout. Preserve back-edge fuel accounting and live roots for every fallback.

The two probes together reach 603 M from 937 M. Hypothesis: production guards and a key-use opcode reach 610–630 M. Faster own-slot access and dense callback dispatch can bring the full row to 575–605 M. The estimate excludes common loop work from the separate loop study.

The change spans one small built-in PR and separate compiler/runtime PRs with narrow tests. The push guard adds a few words per realm that can initialize lazily. A property opcode or helper can increase hot loop size and startup cost. The forEach change can affect callback exceptions and closure-heavy code if it alters calls or root maps. Check those rows and arrays with indexed prototype mutations on browser journeys.

### B. Use a separate mutable property vector and dense element backing

Give each regular object a stable pointer to a separate value vector. A 16-byte minimum cell can contain an 8-byte header and the pointer, then needs a second allocation once it has named values. A dictionary maps canonical atom IDs to vector offsets. A shared shape can still assign offsets for small objects. Grow the vector geometrically and use stable offsets for live descriptors. This gives average O(1) lookup and simpler slot address calculation. Keep 8-byte values. Add an owner for the separately allocated array vector so a capacity grow can release the previous allocation immediately. Move tracing from a GC-cell scan to the owner and keep it sliceable.

This design trades header and slot branches for more allocations and link loads on small literals. It could remove the up to 3.16 MiB of obsolete dense record requests and a few tens of KiB of named record requests on this row. A dense array still does generic push checks unless it also adopts A's guard. Allocation itself is cheaper in the measured js2 controls. Hypothesis: saving one extra geometry lookup per named write gives less than 20 M instructions on this row, and reclaiming backing on grow saves at most a few more M in the same trigger regime. Plan for about 915–940 M at the post-#909 baseline and verify both effects. This design requires several PRs across object allocation, heap ownership, write paths and tracing. It raises risk for realm startup, closure and error allocations, and short-lived props objects in browser journeys.

## 5. Recommended design and implementation plan

Keep inline slots and use design A. Start with push because it has the largest measured cost and an isolated proof. Keep the phase controls. Compare the full frozen input and both scales on every step. Compare the same source in both engines. Check the completion value on every run.

| Step | Change and files | Narrow test and cost gate | Expected objects 10, M |
| --- | --- | --- | ---: |
| 1. Direct push | Add the prototype guard in `interp_rt.rs` and `shapes.rs`. Use the append from `object.rs` in `bi_array.rs`. Guard length, dense capacity and exotic receivers before any write. | Compare append, multiple arguments, holes, sparse arrays, length overflow, frozen arrays, array-likes, proxy or host hooks, and indexed setters and read-only prototype properties added after warmup. Keep the push phase below 185 M. Repeat the full row. Count allocations to confirm that hits make no index atoms. | 640–660 |
| 2. Key-only add | Select the key-use opcode in `compile_expr.rs`. Add encoding and checks in `bytecode.rs` and `verify.rs`. Implement the shared stack formatting and atom probe in `interp.rs`, `interp_rt.rs`, `string.rs` and `atoms.rs`. | Check receiver, key and RHS order, numeric and non-canonical index text, both operand orders, wide and long strings, symbols, coercion, and forced GC. Measure the write prefix, full row and concat controls. Known repeats should allocate no transient result string. | 610–630 |
| 3. Own data slot | Carry a matched descriptor's geometry through the own read and write in `interp_rt.rs`, `shapes.rs` and `object.rs`. Reuse the current slot encoding where it fits. Keep dictionary coordinates local to a single operation. Keep one probe on the miss path. | Check inline and overflow slots, dictionary deletion and re-addition, shape reuse after GC, attributes, inherited setters and host properties. Require a gain on both fixed-key controls. Check tree-shaped ordinary reads for regressions. | 600–620 |
| 4. Dense forEach step | Keep the callback on the existing interpreter stack. Lower the private step in `prelude.js`, `prelude_pre.rs`, `compile_expr.rs`, `bytecode.rs`, `verify.rs`, `interp.rs` and `interp_rt.rs`. Regenerate prelude facts. | Check HasProperty before Get for proxies and holes, a present undefined value, inherited getters, callback deletion and addition, length shrink, thisArg, exceptions, GC and fuel. Remove at least 14 M from the forEach phase. Check closures and calls. | 575–605 |

The targets are hypotheses for guarded implementations. Require a phase gain before adding a new opcode. If step 3 cannot reduce cost in both fixed-key controls, keep the existing slot path. Use the direct append and key-only changes as the first checkpoint.

Use `script_tests.rs` for the built-in cases, `compile_tests.rs` and `verify_tests.rs` for opcode shape, and `object_tests.rs` and `shapes_tests.rs` for layout and mutation. Put new regressions in topic-specific test files. For each PR, run the narrow tests through `t` and typecheck `bare-js2`. Use the relevant `interp_load` rows and the touched-subsystem rows in `tools/js/cost_rows.txt`. Apply the repo's +0.5% gate under both allocator settings or check the one-codegen-unit build. Use three fresh hardware-counter runs, retain the minimum, and record RSS with fresh binary copies and a static `wait4` parent. Include empty, closures, throws and calls as controls for any interpreter changes. Recheck `style_100k`, `churn_1m` and the paired public journey checks before declaring a browser improvement. Check Miri on affected unsafe modules through CI.

## 6. Overlap

- **GC trigger.** #908 removes allocation on index lookup misses. #909 removes the separate integer string on concatenation. This plan starts after both changes. Array backing lifetime and short-string allocation reduce pressure independently of the trigger. Run the phase and RSS controls again after the adaptive trigger lands.
- **Loop.** Both top-level loops still load and store globals and branch in the interpreter after the append and key changes. Use one measurement for any common loop or global specialization. The numbers here exclude hypothetical savings from that work.
- **Webpack.** Named own-data access and shared shape usage overlap. Check bundle compile, cold load and property transitions before changing the common helpers. Computed-name changes must earn their gain on the property phase.
- **Calls.** The forEach callback entry shares the call path. Reuse any faster direct-call entry while retaining argument, `this`, throw, fuel and root-map behavior. Avoid adding a second dispatch path for the same call.

The research ran standalone scripts and allocation controls. Browser journeys, Windows, Miri and the production guards remain to verify.

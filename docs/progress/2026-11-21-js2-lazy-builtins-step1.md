# 2026-11-21: js2 lazy built-in tables (step 1 of `design-js2-lazy-builtins.md`)

Branch `t3/lazy-builtin-tables`. Base for the cost rows: origin/main 24c05b6f. Step 0 (DECOMPOSE) was already on main (#830). This PR does not repeat it.

## What changed

The methods, statics and constants of the built-in objects are `static` tables. A realm records a table. It makes a property at the first lookup that needs it.

- `crates/js2/src/lazy.rs` (new, about 270 lines): the table entry (`Ent`, made by the const fns `m`, `num`, `indexed`, `same`, `alias`), the per-realm state (`Lazy`), and the hooks `lazy_prep` (one name) and `lazy_all` (all names). A property that is made has the same attributes, native, `length` and `name` as an eager install.
- Header flag `F_LAZYTAB` (bit 20; bit 19 is `F_DATE`). It is set while a table has entries left. A path that does not look at a name of the object pays one flag test.
- Probe: a 4,096-bit filter holds the hash of every table name. A name that is not in it costs one test. The hash of a table name is computed at compile time with the same function as `Chars::hash`.
- Each lazy object gets one hidden key (`"\0lt{n}"`) in its shape. The inline caches answer a prototype read from the shape of the receiver alone, so two objects with different names left must not share a shape. The hidden key makes the shape unique. No listing shows it.
- Entry kinds: `Method`, `Indexed` (one native for many methods, `Date.prototype`), `Num` (constants), `Alias` (`Number.parseInt` is the global `parseInt`), `Same` (`Date.prototype.toGMTString` is `toUTCString`). The functions that an alias names are kept in `Lazy::shared` (rooted), so a script that deletes `parseInt` does not change `Number.parseInt`.
- Hooks: `fn_prep`, `fn_prep_all`, `lookup_g`, `proto_setter`, `proto_target`, `get_obj_field`, `define_attrs`, `define_accessor`, `prevent_extensions`. `lookup_g` and the proto walks now take `&mut self`; `error_text` uses a read-only `lookup_ro`, which has no hook (it only reads names for a message).
- Converted to tables: `Object`, `Function.prototype`, `Array`, `String` (both files), `Number`, `Boolean.prototype`, `Math`, global functions, `JSON`, `Reflect`, `Date`, `Map`, `Set`, `WeakMap`, `WeakSet`, `Promise` (+ statics, combinators, `finally`), `queueMicrotask`.
- One line outside the lane: `atoms.rs` `Table::insert` grows the atom table at load 1/2 instead of 3/4. Reason: without it `bundle_like` was +0.70% (glibc). The cause is not lazy work. Callgrind showed `atoms::Table::find` from `atomize` at 211 instructions per call, against 54 on main (4,283 calls), because the first lookups now land in a fuller table. At load 1/2 the row is +0.10%. Cost: the atom table is up to 2x bigger. To verify: the memory effect (not measured).
- Own-key order changes: a lazy property comes in the order of first use, then the rest in table order after a listing (owner decision 2, 2026-11-20).

## Tests

`crates/js2/src/lazy_tests.rs`, 19 tests, written before the change. `a_lazy_realm_and_an_eager_realm_have_the_same_properties` is the differential test: a test-only `EAGER` switch makes every entry at once, and the test compares the own keys and the descriptors of every built-in object with a lazy realm. The other tests cover the operations: `hasOwnProperty` and `in`, `delete` (a deleted name is not made again), `set`, `defineProperty`, `getOwnPropertyDescriptor`, listings, `freeze`, `seal`, `preventExtensions`, `assign`, spread, `JSON.stringify`, the prototype chain (a nearer name hides a farther one, setters and read-only names), `super`, `instanceof` and classes, `Reflect`, aliases, collections and `Promise`, host reads, and the hash of every table name.

| Check                                                 | Result                                                                                                                   |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `cargo test --locked -p bare-js2 --lib`               | 628 passed, 0 failed                                                                                                     |
| same with `BARE_GC_STRESS=1`                          | 628 passed, 0 failed                                                                                                     |
| `cargo test --locked -p bare-js-api --features suite` | all test results ok (32, 112, 1, 5 passed)                                                                               |
| `tools/js/test262_js2.sh -u`                          | gate ok; total pass 29,731, fail 2,352, skip 755; `test262-js2-baseline.txt` is byte-identical to main, so 0 passes lost |

## Measurements

Machine: archMachine, Ryzen 9 5900HX, other sessions running. Instruction counts (`tools/js/instr.sh`, user mode, minimum of 3 runs). Base: detached worktree at 24c05b6f. New: this branch. Release build, default profile. jemalloc rows use `LD_PRELOAD=/usr/lib/libjemalloc.so.2`.

| Row                                  | glibc base    | glibc new     | change  | jemalloc change |
| ------------------------------------ | ------------- | ------------- | ------- | --------------- |
| startup (`script_run`, `var x = 1;`) | 2,160,274     | 1,896,096     | -12.23% | -5.40%          |
| `interp_load loopfn 3000000`         | 827,572,215   | 827,290,761   | -0.03%  | -0.05%          |
| `interp_load calls 1000000`          | 576,627,905   | 576,349,845   | -0.05%  | -0.07%          |
| `interp_load fib 27`                 | 255,935,263   | 255,655,407   | -0.11%  | -0.14%          |
| `interp_load closures 1000000`       | 1,949,727,345 | 1,949,201,661 | -0.03%  | -0.03%          |
| `interp_load objects 1000000`        | 705,473,284   | 705,123,178   | -0.05%  | -0.06%          |
| `interp_load loopfn 0`               | 1,071,086     | 790,390       | -26.21% | -8.57%          |
| eval_loop                            | 88,358,108    | 88,101,090    | -0.29%  | -0.31%          |
| eval_loopfn                          | 49,184,069    | 48,929,471    | -0.52%  | -0.53%          |
| eval_calls                           | 73,801,848    | 73,547,148    | -0.35%  | -0.37%          |
| eval_fib                             | 7,314,818     | 7,056,904     | -3.53%  | -2.09%          |
| eval_closures                        | 16,196,876    | 15,933,445    | -1.63%  | -1.30%          |
| eval_objects                         | 70,003,463    | 69,692,801    | -0.44%  | -0.51%          |
| eval_concat                          | 15,035,569    | 14,776,526    | -1.72%  | -1.36%          |
| eval_throws                          | 32,455,629    | 32,367,737    | -0.27%  | -0.32%          |
| eval_webpack                         | 9,903,407     | 9,661,090     | -2.45%  | -1.52%          |
| bundle_like                          | 127,548,433   | 127,676,872   | +0.10%  | +0.06%          |

Every row is inside +0.5% under both allocators, so no `codegen-units=1` run was needed. Before the `atoms.rs` change `bundle_like` was 128,441,631 (+0.70%).

The startup gain is smaller than the sum of the earlier growth (about 71,000 for `Date`, 5% for collections), because the tables still cost the filter and the table records, and some natives stay eager (below). To verify: the instruction share of each remaining eager install.

## Not tested

- Windows and Miri. No `unsafe` and no collector code changed. `lazy_make` roots the shared functions with `keep`. The GC stress run covers this.
- Peak RSS and time to ready (only instruction counts were measured).
- Own-key order against test262 beyond the existing subset. The subset shows no change.
- Edge cases of `Alias` and `Same` when a script replaces the target and then lists keys: covered for delete and replace, not for every operation.
- A host read that goes around `fn_prep` (a new place that reads `shapes.find` of a script-visible object must call `lazy_prep` or `lazy_all`; the module doc says so).

## Still eager

Error prototypes, `RegExp`, `Symbol` (it has its own coarse `F_LAZY` members), the generator and iterator prototypes, `__helper`, `__set` and `__def` on `Object`, the six cells that the collections keep (`Map.prototype.set`, `Set.prototype.add`, `WeakMap.prototype.set`, `WeakSet.prototype.add`, `Map.prototype.entries`, `Set.prototype.values`), the iterator `next`, and `Date.prototype[Symbol.toPrimitive]`.

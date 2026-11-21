# 2026-11-21: js2 lazy built-in tables (step 1 of `design-js2-lazy-builtins.md`)

Branch `t3/lazy-builtin-tables`. Base for the cost rows: origin/main 724b7a25 (measured after the last merge). Step 0 (DECOMPOSE) was already on main (#830). This PR does not repeat it.

## What changed

The methods, statics and constants of the built-in objects are `static` tables. A realm records a table. It makes a property at the first lookup that needs it.

- `crates/js2/src/lazy.rs` (new, about 270 lines): the table entry (`Ent`, made by the const fns `m`, `num`, `indexed`, `same`, `alias`), the per-realm state (`Lazy`), and the hooks `lazy_prep` (one name) and `lazy_all` (all names). A property that is made has the same attributes, native, `length` and `name` as an eager install.
- Header flag `F_LAZYTAB` (bit 21; bits 19, 20 and 23 are used by other flags). It is set while a table has entries left. A path that does not look at a name of the object pays one flag test.
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
| `cargo test --locked -p bare-js2 --lib`               | 692 passed, 0 failed                                                                                                     |
| same with `BARE_GC_STRESS=1`                          | 692 passed, 0 failed                                                                                                     |
| `cargo test --locked -p bare-js-api --features suite` | all test results ok (32, 152, 1, 5 passed) |
| `tools/js/test262_js2.sh`, this branch against a main build (724b7a25) | the per-test result lists (`new.txt`) are identical: 25,256 pass on both, so 0 passes lost and none gained |

## Measurements

Machine: archMachine, Ryzen 9 5900HX, other sessions running. Instruction counts (`tools/js/instr.sh`, user mode, minimum of 3 runs). Base: detached worktree at 724b7a25. New: this branch. Release build, default profile. jemalloc rows use `LD_PRELOAD=/usr/lib/libjemalloc.so.2`.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | --- | --- | --- | --- | --- | --- |
| `interp_load loopfn 3000000` | 887,574,863 | 887,295,258 | -0.03% | 891,082,384 | 890,696,486 | -0.04% |
| `interp_load calls 1000000` | 595,632,052 | 595,354,372 | -0.05% | 599,108,224 | 598,721,739 | -0.06% |
| `interp_load fib 27` | 263,883,807 | 263,604,351 | -0.11% | 267,358,346 | 267,004,567 | -0.13% |
| `interp_load closures 1000000` | 1,966,726,402 | 1,966,202,932 | -0.03% | 1,969,983,430 | 1,969,489,524 | -0.03% |
| `interp_load objects 1000000` | 714,476,075 | 714,127,306 | -0.05% | 717,949,232 | 717,524,656 | -0.06% |
| `interp_load loopfn 0` | 1,074,035 | 794,863 | -25.99% | 4,520,489 | 4,134,684 | -8.53% |
| eval_loop | 93,431,232 | 93,179,245 | -0.27% | 100,291,446 | 99,997,693 | -0.29% |
| eval_loopfn | 52,258,905 | 52,009,069 | -0.48% | 59,118,485 | 58,823,929 | -0.50% |
| eval_calls | 76,176,336 | 75,925,660 | -0.33% | 83,035,888 | 82,742,929 | -0.35% |
| eval_fib | 7,462,829 | 7,212,528 | -3.35% | 14,320,970 | 14,028,202 | -2.04% |
| eval_closures | 16,452,743 | 16,192,798 | -1.58% | 23,323,695 | 23,031,292 | -1.25% |
| eval_objects | 71,084,213 | 70,759,079 | -0.46% | 77,983,333 | 77,653,230 | -0.42% |
| eval_concat | 15,347,702 | 15,093,496 | -1.66% | 22,207,499 | 21,912,520 | -1.33% |
| eval_throws | 32,977,531 | 32,906,523 | -0.22% | 39,844,970 | 39,735,289 | -0.28% |
| eval_webpack | 10,010,240 | 9,768,837 | -2.41% | 16,993,795 | 16,739,287 | -1.50% |
| bundle_like | 128,927,495 | 128,943,686 | +0.01% | 135,975,463 | 135,950,110 | -0.02% |
| startup (`script_run`, `var x = 1;`) | 2,233,775 | 1,973,887 | -11.63% | 5,713,346 | 5,411,998 | -5.27% |

Every row is inside +0.5% under both allocators, so no `codegen-units=1` run was needed. Before the `atoms.rs` change `bundle_like` was +0.70% (glibc, on the earlier base 24c05b6f: 128,441,631 against 127,544,099).

The startup gain is smaller than the sum of the earlier growth (about 71,000 for `Date`, 5% for collections), because the tables still cost the filter and the table records, and some natives stay eager (below). To verify: the instruction share of each remaining eager install.

## Not tested

- Windows and Miri. No `unsafe` and no collector code changed. `lazy_make` roots the shared functions with `keep`. The GC stress run covers this.
- Peak RSS and time to ready (only instruction counts were measured).
- Own-key order against test262 beyond the existing subset. The subset shows no change.
- Edge cases of `Alias` and `Same` when a script replaces the target and then lists keys: covered for delete and replace, not for every operation.
- A host read that goes around `fn_prep` (a new place that reads `shapes.find` of a script-visible object must call `lazy_prep` or `lazy_all`; the module doc says so).

## Still eager

Error prototypes, `RegExp`, `Symbol` (it has its own coarse `F_LAZY` members), the generator and iterator prototypes, `__helper`, `__set` and `__def` on `Object`, the six cells that the collections keep (`Map.prototype.set`, `Set.prototype.add`, `WeakMap.prototype.set`, `WeakSet.prototype.add`, `Map.prototype.entries`, `Set.prototype.values`), the iterator `next`, and `Date.prototype[Symbol.toPrimitive]`.

# 2026-11-21: js2 `Map`, `Set`, `WeakMap` and `WeakSet` (collections PR 1 of 2)

Branch `t3/js2-collections-reflect`. Base: origin/main 287805fe. `Reflect` is PR 2 (its own progress file will link this one).

## What changed

The census group `Sc` is in all 50 workloads, and js2 had none of it. This PR adds the four built-ins to js2.

- `crates/js2/src/bi_collections.rs`: constructors, prototypes, iterators, `install_collections` (called at the end of `install_builtins`).
- `crates/js2/src/weaktab.rs`: the one `unsafe` wrapper, over the collector's ephemeron table (`Layout::WeakTable`). It follows the pattern of `blob.rs`.
- `crates/js2/src/object.rs`: flag bits 16 to 18 (`F_COLL`) for the kind, and `Obj::flags()`.
- `interp_rt.rs`: one `coll` field in `Realm`. `lib.rs`: the module lines.
- test262: four directories added to `test262-js2-subset.txt`, four `allow:` features in `test262-js2-skip.txt`.
- `crates/js-diff/examples/collection_load.rs`: the cost row on both engines.
- No change in `gc.rs`. The collector already has the ephemeron arm. No change in `crates/bare`.

## Layout of `Map` and `Set` (5 lines)

1. A `Map` or `Set` object is a plain object cell. One hidden slot (`\0cs`, not writable, not configurable) holds the table. An empty `Set` is 16 bytes.
2. The table is one flat `Slots` record: `[FWD, STATE, USED, SIZE, CAP | buckets (CAP/2) | entries (CAP x 3 for Map, CAP x 2 for Set)]`.
3. A hash chain runs through the `next` field of the entries, as in V8's `OrderedHashMap`. No pointer and no separate cell for an entry.
4. `delete` makes the key a hole and keeps the link. A full table is copied to twice the size, or to the same size when at most half is live. A table under a quarter full after a delete is copied to half the size.
5. A copy leaves the old table with `FWD` and `STATE` set. An iterator that finds its table replaced follows `FWD`, lowers its index by the deleted entries below it, and so never skips or repeats an entry. `clear` sets state 2, and the cursor goes to 0.

`WeakMap` and `WeakSet` use the ephemeron table of the collector: a value stays alive only while its key is alive. Keys are objects only. A symbol is a leaf cell, so the collector would never release a value that waits for it (to verify: a collector change would lift this).

## Measurements

Machine: archMachine, other sessions running (load noise). Instruction counts are user mode, the minimum of 3 runs (`tools/js/instr.sh`). Release build, default profile.

### Cost rows (js2, base 287805fe vs this branch), net count = count at N minus count at N = 0

| Row                                     | N         | base          | new           | change          |
| --------------------------------------- | --------- | ------------- | ------------- | --------------- |
| loopfn                                  | 1,000,000 | 280,501,132   | 280,500,628   | -0.0002%        |
| calls                                   | 1,000,000 | 586,500,362   | 586,500,582   | +0.00004%       |
| fib                                     | 30        | 1,100,903,303 | 1,100,903,572 | +0.00002%       |
| closures                                | 200,000   | 370,100,243   | 370,139,820   | +0.011%         |
| objects                                 | 200,000   | 143,726,724   | 143,728,580   | +0.001%         |
| `script_run` on an empty file (startup) |           | 1,965,207     | 2,033,712     | +68,505 (+3.5%) |

All five micro rows are inside +0.5% with the default build, so no `codegen-units=1` run was needed. The startup rise is the eager install of about 35 natives and 6 prototype objects. The owner accepts this until step 1 of the lazy built-ins (`docs/design-js2-lazy-builtins.md`) is built. That step moves it to the first use.

### `collection_load` (instructions per operation, N = 100,000, old engine `bare-js` vs js2)

| Row    | Operation                                           | old    | js2   | ratio |
| ------ | --------------------------------------------------- | ------ | ----- | ----- |
| map    | `set` + `get`, integer keys (per pair)              | 6,202  | 3,140 | 0.51  |
| mapstr | the same, string keys `'k' + i`                     | 12,762 | 5,321 | 0.42  |
| set    | `add` + 2 `has` (per turn)                          | 8,568  | 4,346 | 0.51  |
| setdel | `add` + `delete`                                    | 5,540  | 3,088 | 0.56  |
| weak   | `WeakMap` `set` + `get`, object keys                | 8,987  | 3,762 | 0.42  |
| iter   | `forEach` over a `Map` (per entry)                  | 4,565  | 2,442 | 0.53  |
| build  | `new Set()` with 3 adds (slope of N = 50k and 100k) | 9,653  | 4,743 | 0.49  |

js2 is 1.8 to 2.4 times cheaper on every row. Most of what is left is interpreter dispatch and method lookup, not the table (hypothesis: from a `perf` split of about 10% in the table code; not repeated on an idle machine).

### test262 (`tools/js/test262_js2.sh -j3`, 692 directories, checkout at /data/claude-tmp/test262)

| Directory         | old engine pass/fail/skip | js2 pass/fail/skip |
| ----------------- | ------------------------- | ------------------ |
| built-ins/Map     | 143 / 0 / 61              | 143 / 0 / 61       |
| built-ins/Set     | 185 / 0 / 198             | 185 / 0 / 198      |
| built-ins/WeakMap | 87 / 0 / 54               | 87 / 0 / 54        |
| built-ins/WeakSet | 73 / 0 / 12               | 73 / 0 / 12        |

(Subdirectories included.) The skips are the same on both engines. By feature, in these four directories: `set-methods` 179, `upsert` 62, `Reflect.construct` 40 (PR 2 lifts this), `symbols-as-weakmap-keys` 22, `array-grouping` 14, `BigInt` 4, cross-realm 4.

Total js2 passes: 19,738 against 19,229 in `test262-js2-baseline.txt` (+509). Lost passes against that baseline: 0. One directory showed one fewer pass, `built-ins/RegExp/property-escapes/generated` (350 against 351). The cause was `Script_-_Gurung_Khema.js` hitting the 120 s directory timeout under load. Run alone it passes in 0.4 s. The 1,542 "pass on the old engine, fail on js2" in the report are old gaps (`Array.from`, iterators, `ArrayBuffer`). I did not change the baseline file (`-u`); the main session owns it.

## Tests

- `cargo test --locked -p bare-js2 --lib` (Linux): 558 tests, 557 passed on the first run. The one failure, `compile::tests::a_scope_with_many_locals_compiles_in_linear_time`, is a wall-time test that I did not touch. It failed with other sessions loading the machine and passed alone (9 s). 12 new tests in `bi_collections_tests.rs`. Each script runs once normally and once under GC stress (`set_trigger`). The weak tests use `collect_idle` and the `last_gc` counters (cleared, released, live bytes).
- Covered: SameValueZero keys (`-0`, `NaN`), insertion order, the protocol of the iterators, an iterator over a delete, an add, a copy (grow and shrink) and a `clear`, `forEach` with changes during the loop, the constructors with iterables and bad input, the property shape of the built-ins, objects-only weak keys, a dead key that drops its entry, a chain and a cycle through weak maps, and a weak map under constant collection.

## Not tested

- Windows (no Windows run of this PR). Miri (the new `unsafe` is `weaktab.rs` only; to verify under Miri).
- A browser or journey run. `for..of` and spread over a `Map` or `Set` need the generic iterator protocol in `IterOpen` (interp.rs, step G3, outside this lane). Today they work through `forEach`, `keys()`, `values()`, `entries()` and explicit `next()` calls.
- Wall time. All timings here are instruction counts.

## Left out on purpose

`Map.groupBy`, the `Set` methods (`union` and the like), `WeakRef` and `FinalizationRegistry`. No journey in the census uses them.

# A lookup in a dictionary shape ends under add and delete churn

Date: 2026-11-25. Base: main 2f92860b.

## Problem

- A dictionary table of more than `SCAN_MAX` entries has an open-addressing index (`shapes.rs`, `Table::index`). A probe stops only at an empty slot.
- A delete marks the slot `DELETED` and pops a trailing tombstone entry. So `entries.len()` stays the same, and `push` never makes a new index.
- An add of a new key that finds no `DELETED` slot on its path takes an empty slot. After enough adds and deletes of new keys, no slot is empty.
- Then the lookup of an absent key loops forever. Microsoft Teams hit this: the page spun for 19 minutes in `Table::probe` under `get_prop` (stack samples with `eu-stack`), with no back edge, so the step budget did not stop it.

## Change

- `Table::filled` counts the slots of the index that are not empty (live or `DELETED`).
- `build` sets it to the live entries. `push` adds 1 when it takes an empty slot.
- `push` drops the index when `filled + 1` would pass half of the slots. The next probe builds a new index without the `DELETED` slots. This is the `growth_left` rule of hashbrown.
- The cost is amortized O(1) for each add. `Table` grows by one `Cell<u32>`.

## Cost

- `tools/js/instr.sh` (minimum of 3 runs of `instructions:u`), base 2f92860b, on arjun-e16, glibc.

| Row | Base | New | Change |
|---|---|---|---|
| `shape_load add 100000 20` | 49673452 | 49673648 | +0.000% |
| `shape_load find 1000000 20` | 111874731 | 111373918 | -0.448% |
| `shape_load literal 100000 20` | 53073499 | 53073681 | +0.000% |
| `interp_load objects 1000000` | 699671347 | 699670654 | -0.000% |
| `interp_load calls 1000000` | 556326142 | 556325421 | -0.000% |
| `interp_load closures 1000000` | 1740264373 | 1740262727 | -0.000% |
| `interp_load loopfn 1000000` | 267286264 | 267285212 | -0.000% |
| `interp_load fib 25` | 92937918 | 92935826 | -0.002% |

- Hypothesis: the `find` drop is code layout. Not verified.

## Tests

- New: `crates/vex/src/shapes_dict_churn_tests.rs`. 200 adds and deletes of new keys on a dictionary shape, then a lookup of an absent key. On main the lookup does not end (the test times out after 20 s). Here it passes.
- `cargo test -p bare-vex --lib -- shapes::`: 19 pass.
- `t`: all pass.
- Miri on `shapes::`: 19 pass.

## Not tested

- A live Teams run with this change: not yet.
- test262 counts: not run. The change does not change behavior other than the end of the loop.

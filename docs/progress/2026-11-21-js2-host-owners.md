# 2026-11-21: Owners, host-filled objects and lazy members in js2

Branch `t3/js2-host-owner-tracing`, rebased on `origin/main` e23fe584. The cost rows below were measured on 52b560d4 (the base before this rebase). Design: `../design-js2-host-api.md`, section 12.

## What changed

- `crates/js2/src/gc.rs`: new `Roots::owners(live, round, out)`. After the mark loop, `collect` calls it in rounds. Each round traces the owners that are marked and not traced yet, marks the values that the host gives, and drains the work list. The loop ends at a round with no new owner. `Stats.owners` counts the traced owners.
- `crates/js2/src/api_host.rs` (new): `set_host_owner`, `host_key`, `new_host_indexed`, `new_host_named`, `set_lazy`, `clear_lazy`, the owner list (`HostState`), and the property hooks for host-filled and lazy objects.
- `crates/js2/src/api.rs`: field `hs`, the dispatch of the six methods, `trace_owners`, `prune_owners`.
- `crates/js2/src/interp_rt.rs`, `bi_object.rs`, `bi_json.rs`, `object.rs`, `lib.rs`: the hooks in the slow paths of get, set, has, delete, own keys and define (outside the lane; needed so the host sees the access). New flags `F_HOST` and `F_LAZYM`.
- `crates/js-api/src/suite.rs`, `tests/engines.rs`: a generic host `Fill` and 4 shared cases. Both engines run them, with and without the stress collector.
- `.github/workflows/ci.yml`: `api_host::` added to the Miri filter, because `api_host.rs` holds `unsafe`.
- `crates/js2/examples/owner_load.rs`: a micro row for the owner rounds.

## Design change found by measurement

The first build tested a header flag `F_OWNER` in `Marker::object` and pushed the cell on a list. That cost 3 instructions for each marked object. Instructions in user mode, minimum of 3 runs (`tools/js/instr.sh`), archMachine, base 52b560d4:

| Row                       | Base          | Flag in marker | Change  |
| ------------------------- | ------------- | -------------- | ------- |
| `gc_load chain 200000 20` | 1,091,909,668 | 1,103,910,128  | +1.099% |
| `gc_load tree 200000 20`  | 840,039,765   | 852,036,248    | +1.428% |
| `gc_load chain 1000 2000` | 569,390,804   | 575,417,697    | +1.058% |

Moving the push into a `#[cold]` function changed nothing (1,103,909,764). So the final design has no test in the marker. The rounds read one mark bit for each owner that waits.

## Cost rows, final code

Same method and base. Both trees built with the default release profile.

| Row                           | Base          | New           | Change  |
| ----------------------------- | ------------- | ------------- | ------- |
| `interp_load loopfn 1000000`  | 267,382,885   | 267,383,922   | +0.000% |
| `interp_load calls 1000000`   | 572,439,895   | 572,440,835   | +0.000% |
| `interp_load fib 30`          | 1,091,075,198 | 1,091,075,246 | +0.000% |
| `interp_load closures 200000` | 366,778,970   | 366,778,784   | -0.000% |
| `interp_load objects 200000`  | 144,350,116   | 144,349,841   | -0.000% |
| `gc_load chain 200000 20`     | 1,091,902,428 | 1,091,904,125 | +0.000% |
| `gc_load tree 200000 20`      | 840,034,271   | 840,040,717   | +0.001% |
| `gc_load chain 1000 2000`     | 569,390,724   | 569,391,636   | +0.000% |
| `heap_load heap 1000000`      | 23,706,225    | 23,707,035    | +0.003% |
| `heap_load task 1000000`      | 67,275,738    | 67,276,454    | +0.001% |

No row is over +0.5%, so the `codegen-units=1` rule was not needed. The jemalloc runs were not made. `tools/js/cost_matrix.sh` has no js2 rows, so the rows were run by hand with the same N as the earlier entries. The spread of the 3 runs was not recorded (the script keeps the minimum).

## Owner cost does not grow with the heap

`owner_load H K R` builds H objects (each with a small array), then K owners that hold nothing, then R = 20 forced collections. Instructions, minimum of 3 runs:

| H       | K = 0         | K = 2000      | Difference |
| ------- | ------------- | ------------- | ---------- |
| 10,000  | 105,810,776   | 122,044,266   | 16.23 M    |
| 400,000 | 4,175,667,400 | 4,192,725,599 | 17.06 M    |

The heap is 40 times larger. The cost of 2000 owners grows by 5%. The difference also holds the 2000 `own()` calls of the script (the same for both H). This is the evidence that the loop depends on owners and rounds, not on the heap.

## Tests

- js2 lib, Linux, after the last rebase (e23fe584): 566 tests. 565 passed in the full run. `compile::tests::a_scope_with_many_locals_compiles_in_linear_time` failed in that run (a timing test, load average about 25 on the machine) and passed alone. 14 of the tests are new (`api_host_tests.rs`).
- `cargo test --locked -p bare-js-api --features suite`, Linux, after the rebase: lib 32, `engines` 100 (4 new cases, each on old, old stress, new, new stress), doc 1, other 5: all passed.
- Windows (VM, `cargo xwin` build, `crt-static`), before the last rebase: js2 lib 523 passed, 2 failed (525 tests then). Both are corpus tests (`parse::tests::corpus_status_matches_acorn`, `parse::lazy_tests::a_lazy_parse_finds_the_same_errors_as_a_full_parse_on_the_corpus`). They read the corpus from a source path that the VM does not have (`NotFound`). They do not use owners or host objects. `engines` 64 passed (then). The Windows run was not repeated after the rebase.
- `cargo check --locked -p bare -p bare-js -p bare-js-api --all-targets`, after the rebase: exit 0.
- Miri (`cargo +nightly miri test -p bare-js2 --lib`, before the last rebase, the filter of `ci.yml` plus `api_host::`): the run was stopped by its 1 hour limit. 69 tests passed and none failed, among them all 17 `gc::` tests and all 14 `api_host::` tests. The tests of `api::` after `a_rope_is_flat_when_the_host_reads_it` and some of `interp::` did not run to the end.

## Differences from the old engine

- `set_lazy(obj, true)` marks the object and every object of its prototype chain, as the trait documents. The old engine marks one object only and asks only for `on*`-style names. js2 asks the host for every missed name of a marked object.
- A new key for the same owner replaces the old key in js2. The old engine pushes a second entry.
- Writes to a host-filled object (a host index, or a name that `named_has` says is there) are rejected. The trait has no write hook (`Host::named_write` of the old engine is not in it).
- The fill step (`Pending::Fill`, `Item::Missing`) answers `Abort::Unsupported("the host fill step")`. It is not in the trait either.

## Not tested

- The cost of a property read in a realm that has a host-filled or lazy object. While one exists, each slow-path read looks at the flags of the prototype chain. Rows with no such object are unchanged. Hypothesis: the cost is small, to verify.
- An ordinary own property of a host-filled object with the name of a host name (the inline cache may serve it). `set_checked` and a few `lookup_g` paths do not call the host hooks.
- Owner chains deeper than 3 rounds under load. The rounds are O(owners) each, so the worst case is owners times depth.
- The browser (`crates/bare`) on js2, the cost matrix of the old engine rows (the old engine code did not change), and jemalloc runs.
- Windows corpus tests (path missing on the VM), and the Windows run after the last rebase.
- The rest of the Miri filter (the run stopped at the time limit).

# 2026-11-21: Fill step and named writes on the host interface (S0.4 PR 6 prerequisite)

Branch `t3/host-fill-named-write`, stacked on PR #829 (`t3/js2-host-owner-tracing`), merged once with `origin/main` 0f678c87. Design: `../design-js2-host-api.md`, section 12.4.

## What changed

- `crates/js-api/src/lib.rs`: `Host::fill`, `Host::named_write`, and the types `Write` (`Set`, `Define`, `Delete`) and `Descriptor`. `fill` defaults to `Abort::Unsupported`. `named_write` defaults to a refusal (`Ok(false)`).
- `crates/js2/src/api_host.rs`: `hs_ask` runs the fill step on `Pending::Fill` and reads again (at most 3 fills, then `Abort::Limit`). `hs_item` does the same for `Item::Missing`. `hs_write` replaces `hs_write_blocked` and calls `named_write` from `set_prop_g`, `define_own` and `delete_prop`. Both run only when `hs.active` and the object has `F_HOST`.
- `crates/js/src/eval_api.rs`, `eval_host.rs`: a realm with no old host sends `NeedsHost` and `NeedsNamed` to the interface host (`api_fill`, `api_named_write`). A realm with an old host does as before.
- `crates/bare/src/browser.rs`: the `bare_js_api::Host` impl of `Dom` forwards `fill` and `named_write` to `fill_indexed` and `named_write`. No other change in `crates/bare`.
- Shared suite (`suite.rs`): the `Fill` host has a live list (key 11) and writable names. Three new cases, each on old, old stress, new, new stress: live list with fill, named write accepted/refused/throws, named write falls through.
- `docs/design-js2-host-api.md`: new 12.4. The "Built: scripts" section that arrived from main is renumbered 13.

## Decisions

- The contract is the old one. `named_write` answers `bool`. The engine decides a fall-through: a symbol, an open object with an ordinary property of that name, a write with another receiver, a delete of a name that `named_has` denies.
- Behavior change in js2: a closed named object used to make an ordinary own property for an unknown name. Now the host decides, and the default refuses. One assertion of `api_host_tests.rs` changed.
- `Descriptor` carries all six fields. The browser reads three of them.

## Measurements

Machine archMachine. Instructions in user mode, minimum of 3 runs (`tools/js/instr.sh`). Base: the merge commit with this change reverted. Both trees: release, default profile.

| Row | Base | New | Change |
| --- | --- | --- | --- |
| `interp_load loopfn 1000000` | 282,411,662 | 282,410,722 | -0.000% |
| `interp_load calls 1000000` | 582,468,291 | 582,467,550 | -0.000% |
| `interp_load fib 30` | 1,085,718,362 | 1,085,718,148 | -0.000% |
| `interp_load closures 200000` | 369,809,312 | 369,808,952 | -0.000% |
| `interp_load objects 200000` | 144,710,616 | 144,710,520 | -0.000% |
| `owner_load 10000 2000 20` | 122,391,365 | 122,214,396 | -0.145% |
| `owner_load 400000 2000 20` | 4,190,401,647 | 4,191,135,217 | +0.018% |

`cost_matrix.sh -g 'dataset_load|attr_load'`: 18 rows, glibc, 3 runs each. Largest change +0.020% (`dataset_load read 1000 1`). No row over +0.5%, so the jemalloc and `codegen-units=1` steps did not run. Mean +0.007%.

## Tests (Linux)

- `cargo test --locked -p bare-js-api --features suite`: lib 32, `engines` 132, doc 1, other 5. All passed.
- `-p bare-js2 --lib`: 572 passed.
- `-p bare-js --lib`: 1107 passed, 1 ignored.
- `-p bare --lib`: 1720 passed, 6 ignored. New: `the_interface_hooks_forward_to_the_old_methods` (set, accessor, no value, delete, fill of a `dataset`).

## Not tested

Windows, macOS, Miri (the change adds no `unsafe`), `BARE_GC_STRESS=1` on `-p bare`, peak memory and wall time, jemalloc runs, a real `Dom<E: Engine>` (PR 6). The `Dom` forwarding runs only from the new test, because the browser still uses the old route.

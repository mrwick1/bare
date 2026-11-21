## 2026-11-21: One exclusive object-kind field in the js2 header

### The bug

Two pairs of header flags shared one bit on main:

- `F_DATE` (`bi_date.rs`) and `F_HOST` (`object.rs`) were both `1 << 19`. A `Date` took the host paths (`has_flag(F_HOST)` in `interp_rt.rs`, `api_host.rs`, `bi_object.rs`). A host object was a `Date` for `Date.prototype` methods.
- `F_AGEN` (`object.rs`) and `F_LAZYTAB` (`lazy.rs`) were both `1 << 21`. A listing of the names of an async generator (`Object.keys`, `getOwnPropertyNames`) called `lazy_all`, which cleared `F_LAZYTAB`. That also cleared `F_AGEN`, so `next()` then failed. A built-in with a lazy table passed `has_flag(F_AGEN)`.

Only one bit was free, so a bit move could not fix both.

### The fix

The kinds of object exclude each other: wrapper, promise, generator, async generator, `RegExp`, `Date`, and the 7 collection kinds. They now share one 4-bit field, `F_KIND` (bits 12 to 15). The numbers `K_MAP` .. `K_DATE` are in `object.rs`. `Obj::kind()`, `Obj::is_kind(k)` and `Obj::set_kind(k)` read and write it. `is_kind` is one mask and one compare, as cheap as `has_flag`. `set_kind` replaces the old kind, so an object cannot have two.

| Bits | Before | After |
| --- | --- | --- |
| 0 to 11 | `F_OVER` .. `F_SLOWIDX`, `F_LAZY` | same |
| 12 | `F_WRAP` | kind field, bits 12 to 15 |
| 13 | `F_PROMISE` | (kind) |
| 14 | `F_GEN` | (kind) |
| 15 | `F_REGEXP` | (kind) |
| 16 to 18 | `F_COLL`, the 3-bit collection kind (values 1 to 7) | `F_HOST` (16), `F_LAZYM` (17), `F_LAZYTAB` (18) |
| 19 | `F_HOST` and `F_DATE` | free |
| 20 | `F_LAZYM` | free |
| 21 | `F_LAZYTAB` and `F_AGEN` | free |
| 22 | free | free |
| 23 | `F_PROTO` | `F_PROTO` (same) |

Kinds: 1 to 7 are the old collection numbers (`K_SITER_STR` is 7), then `K_WRAP` 8, `K_PROMISE` 9, `K_GEN` 10, `K_AGEN` 11, `K_REGEXP` 12, `K_DATE` 13. 14 and 15 are free. Bits 19 to 22 are free (4 bits). Bits 0 to 11 and 23 did not move.

`object.rs` has a table of every bit and `ALL_FLAGS`, with a compile-time check that each flag is not zero, fits `FLAG_MAX` and shares no bit with another, and that the largest kind fits the field. `F_LAZYTAB` moved from `lazy.rs` to `object.rs`, and the kind numbers of `bi_collections.rs` and `bi_string_iter.rs` moved there too, so one file holds all of them. `F_DATE` is gone.

Sites moved to the kind field (`git diff --stat` has the list): `bi_wrap.rs`, `promise.rs`, `regexp_rt.rs`, `generator.rs`, `asyncgen.rs`, `bi_date.rs`, `bi_object.rs` (`Object.prototype.toString`), `interp_rt.rs`, `bi_collections.rs`, `bi_string_iter.rs` and the promise constructor and assertions in `object.rs`. The `F_HOST`, `F_LAZYM`, `F_LAZYTAB` and `F_PROTO` sites did not change: their names and uses are the same, only their bits differ.

### Tests

Run first against the old code, the two behavior tests failed:

- `api_host::tests::a_date_is_not_a_host_object_and_a_host_object_is_not_a_date`: a `Date` in a host realm read `undefined,5,a,b,false` for `d.x`, `getTime`, `Object.keys` and `hasOwnProperty` (it took the host path), not `3,5,x,true`. It also checks that `Object.prototype.toString` and `Date.prototype.getTime` on a host object do not treat it as a `Date`.
- `asyncgen::tests::an_async_generator_is_not_a_lazy_table_object_and_a_lazy_built_in_is_not_an_async_generator`: failed at the first case (`next()` after a listing and a `defineProperty` gave `undefined`). The second case (a lazy built-in is not an async generator: `next.call(Math)` rejects with a `TypeError`) was added to the same test; I did not see it fail separately on the old code.
- `object::tests::the_kind_is_one_exclusive_field_and_no_flag_shares_a_bit_with_it`: needs the new API, so it has no old-code run. It checks that every kind is exclusive and that no flag set or cleared changes the kind.

After the fix, on archMachine:

- `cargo test --locked -p bare-js2 --lib`: 745 pass.
- `cargo test --locked -p bare-js-api --features suite`: 32 and 152 pass (all suites that ran).
- `cargo test --locked -p bare --lib`: 1724 pass, 6 ignored.
- test262 js2: `TEST262=/data/claude-tmp/test262 tools/js/test262_js2.sh -j 4 -u` (254 s) rewrote no file of `tools/fixtures/js/test262-js2-baseline/`: no pass lost, none gained.

### Cost

Release builds, own `CARGO_TARGET_DIR` per tree, `tools/js/instr.sh` (minimum of 3 runs of `perf stat -e instructions:u`), archMachine, other sessions running. Base: main `1303414b`. New: this change. glibc and jemalloc (`LD_PRELOAD`).

| Row | glibc | jemalloc |
| --- | ---: | ---: |
| `interp_load loopfn 3000000` | +0.000% | +0.000% |
| `interp_load calls 1000000` | +0.000% | +0.000% |
| `interp_load fib 30` | +0.000% | +0.000% |
| `interp_load closures 1000000` | +0.000% | +0.000% |
| `interp_load objects 1000000` | +0.000% | +0.000% |
| `gen_load next 100000` | +0.176% | +0.172% |
| `gen_load call 100000` | +0.001% | -0.001% |
| `promise_load then 100000` | -0.201% | -0.196% |
| `promise_load new 100000` | +0.074% | +0.073% |
| `promise_load microtask 100000` | -0.213% | -0.207% |
| `promise_load await_prim 100000` | +0.436% | +0.426% |
| `promise_load thenable 100000` | -0.000% | +0.000% |
| startup (`script_run`, `var x = 1;`) | -0.03% (719,630 to 719,399) | 0.00% |

No row is over +0.5%, so no `codegen-units=1` run was needed. Startup varied by 0.1% between runs of one binary (719,399 to 720,189 over 6 runs). `gen_load forof`, `suspended` and `promise_load chain` do not exist as cases of the examples.

### Not tested

- Windows. Only archMachine ran.
- A separate "module runner": I found no such tool in `tools/js`. The module tests of `crates/js2` (`module_tests.rs`) are in the lib run.
- Miri: no unsafe code changed. The new object test runs under Miri in CI with the other `object_tests.rs` tests; I did not run it under Miri here.
- Wall time and memory (the cell size does not change).
- `BARE_JS_EXEC=new` in the browser (#851 is not on main).

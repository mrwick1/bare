## 2026-11-21: Install functions in the scope form, installed by static tables (plan step S0.4, PR 4)

### Problem
PR 4 moved the browser's install functions to the `Scope` form. The first version made one trait call for each member. A window setup made about 1,300 calls (731 `host_function`, 353 `define_named`, 216 `define_accessor_named`). The trait path cost about 60 instructions more for each call than the old direct path. That was about +89K instructions for each window and it failed the cost rule: `xhr_load 1` +0.89% (glibc) and +0.49% (jemalloc), `session_load chain 10` +1.6% and +1.6%, `preload_load font 1` +1.3%, `fetch_load bytes 1` +0.8% and +0.7%. `codegen-units=1` did not fix it.

### Changes
- `bare-js-api`: new `Def`, `DefKind` (`Method`, `Accessor`, `Both`, `Number`) and `Engine::install_table(targets, id_base, defs)`. The engine reads a `&'static [Def]` in one pass and makes the members of all rows. A row names its targets by a bit mask, and a row of two targets makes one function object for both. The host function ids are `id_base + id`, so the table holds no absolute id. `Scope::install_table` calls it. The method has no default body: both engines and the `Toy` test engine implement it.
- `bare-js` (`eval_api.rs`, `api_install_table`): interns the name of a row once and uses it as the key and as the function name; builds `get NAME` and `set NAME` in one reused buffer; makes no local for a function.
- `bare-js2` (`api.rs`): same shape. The name is an atom that serves as the key and as the function name. A host function is made by `host_cell`, which `host_function` also uses.
- `bare-js2` (`interp.rs`, `api.rs`): a setter that is a host function was a `TypeError` ("a setter is not a function") because the interpreter entered only script setters. A getter already fell back to a nested call. The setter now does the same through a cold function, `Realm::call_setter_slow`. This was a gap for the browser on js2: most DOM setters are host functions.
- `bare` (`browser_scoped.rs`): `Row`, `Table` and the `table!` macro build the table and the handler list at compile time (`const fn`), so a window pays for neither. `Dom::install_rows` adds the handlers to `Dom::scoped` and makes one `install_table` call.
- The big tables moved to rows: `DEFS` of `browser.rs` (200 rows, 22 targets) and `DEFS` of `browser_fetch.rs` (51 rows). All runs of `def_get_at`, `def_method_at`, `def_get_l` and `def_method_l` with a literal name became tables in 19 files (`browser_navigation.rs` has 11 tables). All lazy globals (`browser_lazy::install`, now removed) became `Row::both` rows in 22 files.
- Shared suite tests (`crates/js-api/src/suite.rs`, run on both engines, plain and stress): `a_table_installs_methods_accessors_and_constants` (name, length, attributes, host id plus base, one object for two targets, getter and setter names, a getter without a setter, a constant, a lazy global, key order) and `two_realms_install_the_same_table`.

### Calls for each window (console_load flood 1, debug build, backtrace of each call)
| | before (per call) | after |
|---|---|---|
| `host_function` | 731 | 174 |
| `define_named` | 353 | 230 |
| `define_accessor_named` | 216 | 55 |

150 of the 230 `define_named` calls are `link_s` (3 for each of 50 interfaces).

### Cost (archMachine, release, `cost_matrix.sh`, instructions of `perf stat -e instructions:u`, minimum of 3 runs for each row)
Base: origin/main `2f12c3f6`. New: this branch.

| row | glibc | jemalloc | before (per call, from the brief) |
|---|---|---|---|
| `xhr_load 1` | 7,679,814 to 7,597,935 (-1.07%) | -0.76% | +0.89% / +0.49% |
| `session_load chain 10` | 39,098,636 to 38,079,110 (-2.61%) | -2.76% | +1.6% / +1.6% |
| `session_load chain 1` | 5,295,385 to 5,197,404 (-1.85%) | -1.88% | |
| `preload_load font 1` | 5,943,596 to 5,843,070 (-1.69%) | -1.15% | +1.3% |
| `fetch_load bytes 1` | 7,941,430 to 7,852,290 (-1.12%) | -0.79% | +0.8% / +0.7% |

The subset `xhr_load|session_load|preload_load|console_load|fetch_load|attr_load|dataset_load|browser_load` (58 rows, both allocators): mean -0.354% (glibc) and -0.320% (jemalloc). The largest growth is `attr_load churn 10` +0.450% and `attr_load react 10` +0.388%. No row is over +0.5% under both allocators.

The full matrix (817 rows, glibc, 215 s) is a check beyond the gate. Mean -0.041%. 5 rows were over +0.5% under both allocators: `intl_load nf_locale 1` and `10`, `intl_load nf_locale_opts 1` and `10`, `scheduler_load timers 10`. Both trees built with `RUSTFLAGS="-C codegen-units=1"` give: `intl_load` rows between -0.000% and +0.009%, `scheduler_load timers 10` -0.053% (glibc) and -0.104% (jemalloc). They pass by the codegen-unit rule. `parse_load deep 1` and `10` abort with a stack overflow on base and on the new tree: not caused by this change.

js2 rows (`host_call_load`, `interp_load`; base `2f12c3f6` against this branch; the setter change is in the interpreter):

| row | base | new | change |
|---|---|---|---|
| `host_call_load new host 100000` | 77,092,977 | 76,593,034 | -0.65% |
| `host_call_load new native 100000` | 72,304,282 | 71,402,898 | -1.25% |
| `host_call_load old host 100000` | 232,404,127 | 232,404,008 | 0.00% |
| `interp_load loopfn 100000` | 28,947,131 | 28,947,325 | +0.001% |
| `interp_load calls 100000` | 59,603,900 | 59,704,394 | +0.169% |
| `interp_load fib 25` | 100,225,190 | 100,710,995 | +0.485% |
| `interp_load closures 100000` | 165,609,949 | 165,510,922 | -0.060% |
| `interp_load objects 100000` | 72,542,920 | 71,442,495 | -1.517% |

A first version of the setter change (the nested call inline in the macro) gave `interp_load loopfn` +1.73% and `calls` +0.50%. The cold function removed that. `fib 25` at +0.485% is close to the limit; it is code layout, as the other rows moved both ways.

### Tests
- Linux, `cargo test --locked -p bare-js-api -p bare-js -p bare-js2 -p bare --lib`: 32, 1107 (1 ignored), 546 and 1719 (6 ignored) pass, 0 fail. `cargo test --locked -p bare-js-api --features suite`: 32 and 56 pass (56 = the suite on both engines, plain and stress; 8 of them are the new tests).
- Windows 11 VM (`cargo xwin test`, exe through `winssh`): `bare-js-api` 32 pass, `engines` (suite on both engines) 56 pass, `bare --lib` 1719 pass, 0 fail.

### Install functions that are not tables yet (for PR 6)
Left on the per-call path: 174 `host_function`, 230 `define_named` and 55 `define_accessor_named` calls for each window (459).
- `link_s` and `interface_s` of `browser_scoped.rs` (interface objects: `prototype`, `constructor` and the global; 150 + 13 calls). The values are runtime objects, so a static table cannot hold them. A second trait method is the next step if these rows matter.
- Loops over a name array: `browser_loader::install`, `browser_scroll::install`, `browser_perf::install`, `browser_attr::install`, `browser_canvas.rs` (the `noop` loop), `browser_streams::install`, `browser_scheduler` (`install_scheduler`), `browser_dom_old::install_late`, `browser_web::install`, `browser_xhr::install`, `browser_encoding::install`, `browser_element::install`, `browser_nodelist::install`.
- Functions that a handler makes at run time: `lazy()`/`ensure` families, `browser_custom::interface_ctor_s`, `browser_console::make_console` (19 calls).
- Old form (`Interp`, no scope): `browser_remote_window::install_up`, `browser_textarea::install`, `browser_frames::install`, `browser_handlers::install_onabort`.
- Rows that need a Symbol key (`entries` as `[Symbol.iterator]`) stay one `define` after the table (`browser_fetch.rs`, `browser_navigation.rs`).

### Not tested
- The browser on js2 (`BARE_JS_EXEC=new`): the table path runs on js2 through the shared suite only.
- `Number` rows of `install_table` have a suite test but no browser user yet.
- Timing (wall clock) and memory (RSS) of the window setup.
- Miri and the `BARE_GC_STRESS` / `BARE_ALL_API` runs of the `bare` tests.

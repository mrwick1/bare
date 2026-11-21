# 2026-11-21: S0.4 PR 6c-1, `crates/bare` is generic over the engine (old engine only)

Branch `t3/generic-engine-js2-browser`, on origin/main 74e542c5 (PR 6a #841 and PR 6b #843 are in it). Owner decision 2026-11-21: design B.

## What changed

- New `crates/bare/src/eng.rs`. It has the trait `Eng` and the trait `Ops<E>`.
  - `Eng` is implemented by a zero-size marker type. Its associated types are `Vm` (the engine), `Obj`, `Raw` (copy), `Str`, `Sym`, `Key` and `Iter`. Its functions make and read values: `kind`, `undef`, `null`, `bool_raw`, `num`, `obj_raw`, `str_raw`, `realm`.
  - `Ops<E>` is implemented by `E::Vm`. It has the 103 heap and call operations that the handlers use (`h_get`, `define_data`, `call_o`, `host_scope_o`, `promise_status`, `module_*_o`, ...). A name that `bare_js_api::Engine` also has gets the suffix `_o`. The list is the source of `eng.rs` and `eng_old.rs`; both were made from one table.
  - Neutral types replace the old ones in every signature: `Fault<E>` (`Throw`, `Unsupported`, `Limit`, `Heap`), `Val<E>`, `Prop<E>`, `Desc<E>`, `Slot<E>`, `PState<E>`, `NamedOp<E>`, `Got<E>`. `bare_js_api::Pending` replaces `HeapError` in the hooks.
- New `crates/bare/src/eng_old.rs`: the marker `Old`, `impl Eng for Old` (`Vm = Interp<'static>`, `Obj = ObjId`, `Raw = Value`), `impl Ops<Old> for Interp` (every op forwards with `#[inline(always)]`) and the conversions between `Error` and `Fault<Old>`.
- `ObjId` becomes `E::Obj` and `Value` becomes `E::Raw` in `crates/bare/src` (about 100 files). `Sc`, `HS`, `Table` and `table!` are generic. `Dom<E>` and `Browser<E>` are generic and are made with `Old` only. `Dom` and `Browser` have the default type `Old`, so the test files and the top-level callers need no type argument.
- `Browser::make` builds a page on any `E` (`E::realm` makes the engine). `Browser::new` is the `Old` form. `browser_child.rs` uses `make`, so a child frame uses the engine of its parent.
- `page.rs`, `control.rs`, `browser_session.rs` and `images.rs` use `Browser<Old>`. 6c-2 puts the switch for `BARE_JS_EXEC=new` there.
- `crates/js/src/eval_api.rs`: the bounds of `set_api_sources` and `ApiSource` now say `Host<Interp<'static>>` (a plain type instead of a generic one). `crates/drive/src/run.rs`: `thrown` takes `&OldFault`. Both are one-line changes that the new error type needs.
- `crates/bare/old_engine_files.txt`: 90 entries down to 40.

### Deviation from the brief

The brief says `Dom<E: Engine>`. A bound on the engine type makes `#[derive(Default, Clone, Copy)]` ask for `Default`, `Clone` and `Copy` on the engine, and 65 structs have such derives. So `E` is a zero-size marker with `trait Eng`, and `E::Vm` is the engine. The cost is that `Sc<'s, E>` (`Scope<'s, E::Vm>`) does not tell the compiler what `E` is. A free function that takes only a scope needs `::<E>` at the call site.

## Census (lines in `crates/bare/src/*.rs`, no `*_tests.rs`; files in brackets)

| Item                        | Before (origin/main) | After        |
| --------------------------- | -------------------- | ------------ |
| `ObjId`                     | 835 (63)             | 54 (8)       |
| `Value`                     | 874 (74)             | 114 (27)     |
| `Interp`                    | 666 (81)             | 119 (13)     |
| `it: &mut Interp`           | 534 (78)             | 0            |
| `bare_js::`                 | 227 (89)             | 60 (31)      |
| `.it` field                 | 65 (15)              | 62 (15)      |
| `old_engine_files.txt`      | 90 entries           | 40 entries   |

`ObjId` is left in `eng_old.rs` (the marker) and in 7 files that name the old engine by design: `browser.rs`, `browser_scoped.rs` (the `Old` glue of `table!`), `browser_controls.rs`, `browser_port_hub.rs`, `browser_sources.rs`, `browser_streams.rs`, `browser_xhr.rs`. Most `Value` hits are CSS and form values, not the engine type. The `.it` field is the same field. Its type is `E::Vm` now.

The other `bare_js::` uses are pure functions (`to_uint32`, `number_to_string`, `regexp`, and so on) and the limit types (`EvalLimits`, `HeapLimits`, `LimitKind`, `WallClock`). They need no engine state. Moving them is a rename for a later PR.

## Owner paths of the maps that are keyed by `E::Obj`

I found 48 fields of type `IdMap`, `HashMap` or `HashSet` with `E::Obj` as the key (a line scan of `crates/bare/src`). 45 have a `remove`, `retain` or `clear` call on the field. A script confirms that. `Dom::prune` reaches those calls through the owner kind of the object (`Owner::*`, or `Owner::Source` for the host-filled objects, which `browser_sources::prune` sends on). Three have no such call. They are not a hole:

- `custom.ints` (`ElementInternals` objects) and `media.lists` (caption lists): `trace_roots` adds every key as a root, so the object does not die while the entry exists. The tables are capped (`max_wrappers`).
- `custom.by_ctor`: the definition of the element keeps the constructor. The entry lives as long as the definition, and a definition is never removed.

One more entry, `child.wins` (`Win` of a child frame, keyed by a window object), is removed when the frame goes (`retain` in `browser_child.rs`) and not by `prune`. Its values hold `Persistent` handles, so the key stays alive until then.

I did not run a collector that moves objects. The js2 collector does not move cells, so a key stays valid while its object lives.

## Tests

| Command                                                                     | Result                                                                                                                                                                                                                                                                                                                                                     |
| --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo test --locked -p bare --lib` (full)                                  | 1724 passed, 6 ignored. A first run had 7 failures: my rename changed the text of 7 messages (`Value` became `E::Raw`, `Error` became `Fault<E>`, `Media` became `Media<E>`). Fixed. I compared every string literal, raw string and comment of the diff against origin/main. No other text changed. The 9 tests that cover the 7 cases and the gate pass. |
| `cargo test --locked -p bare-js-api --features suite`                       | pass (32 + 152 + 5 + 1)                                                                                                                                                                                                                                                                                                                                    |
| `BARE_GC_STRESS=1 cargo test --locked -p bare --lib browser_owners_tests::` | 3 passed                                                                                                                                                                                                                                                                                                                                                   |
| `cargo test --locked -p bare --test window_memory --test fetch_https`       | 4 + 5 passed                                                                                                                                                                                                                                                                                                                                               |
| `cargo test --locked -p bare-drive --lib`                                   | 9 passed                                                                                                                                                                                                                                                                                                                                                   |
| `old_engine_gate`                                                           | pass                                                                                                                                                                                                                                                                                                                                                       |
| `cargo xwin check -p bare`                                                  | pass                                                                                                                                                                                                                                                                                                                                                       |
| `cargo check --workspace --all-targets`                                     | pass                                                                                                                                                                                                                                                                                                                                                       |

## Cost rows

Tool: `tools/js/cost_matrix.sh`. BASE is origin/main 74e542c5, NEW is this branch. Instructions of user mode, minimum of 3 runs, machine archMachine under load (instruction counts hold under load). Rows: the 176 rows of the `bare` examples (`attr`, `browser`, `console`, `cssom`, `dataset`, `fetch`, `focus`, `fragment`, `geometry`, `handler`, `loader`, `observer`, `preload`, `selection`, `sheets` and the other `*_load` families). The `bare-stress` rows are layout rows. This PR does not touch layout, so they are not run.

**Result: the rule fails. 11 rows stay over +0.5% under both allocators at `codegen-units=1`.**

Default profile, 176 rows: mean +0.32% (glibc). 13 rows are over under one allocator only. 27 rows are over under both. The raw table is in the PR description.

Second run, `RUSTFLAGS="-C codegen-units=1"` on both trees, for the families of the 27 rows (42 rows): 11 rows are still over under both.

| Row                          | glibc base | glibc new  | change | jemalloc change |
| ---------------------------- | ---------- | ---------- | ------ | --------------- |
| browser_load event 1         | 32184369   | 32472668   | +0.90% | +0.77%          |
| browser_load event 10        | 274558625  | 277336355  | +1.01% | +0.99%          |
| browser_load lookup 1        | 84463182   | 85614577   | +1.36% | +1.25%          |
| browser_load lookup 10       | 789578938  | 800840346  | +1.43% | +1.41%          |
| focus_load dispatch 1        | 34596417   | 34854064   | +0.75% | +0.69%          |
| focus_load dispatch 10       | 296719128  | 299254525  | +0.85% | +0.89%          |
| focus_load focus 1           | 46047050   | 46737512   | +1.50% | +1.02%          |
| focus_load focus 10          | 406417311  | 412012871  | +1.38% | +1.29%          |
| observer_load mo 1           | 81618043   | 82174277   | +0.68% | +0.60%          |
| observer_load mo 10          | 1021044634 | 1029004347 | +0.78% | +0.72%          |
| preload_load script 10       | 16088185   | 16180589   | +0.57% | +0.86%          |

Rows of the `loader`, `handler`, `selection`, `sheets` and `cssom` families pass at `codegen-units=1`.

`dataset_load` (the rows that #841 failed): all 12 rows pass in the default profile, glibc, `dataset_load keys 1000 50` +0.36% at most, `read 10000 50` -0.00%. No row is over +0.5%, so jemalloc was not needed.

Cause (callgrind, profiling build, `codegen-units=1`, `browser_load lookup 1`, `browser_load event 1`, `focus_load focus 1`). Hypothesis: the compiler inlines less in generic code.

- `lookup`: `Dom::query_s` costs 17.48M against 16.57M. The loop `for sel in sels.iter().filter(subject_matches)` runs 9 instructions per turn against 6 (302,500 turns). The code of the loop is the same text.
- `event` and `focus`: `RawTable::find` of the listener map was inline in `Dom::fire` and is now a call (+1.38M in `focus`), and `fire` is 0.98M cheaper. Net +0.4M in `focus`.
- Other differences are below 0.1M each. The calls through `Ops` are flat: all of them are `#[inline(always)]`.

The MIR inliner does not inline across calls that depend on `E` before monomorphization. I did not test a fix. I did not find a cause for `observer_load mo` and `preload_load script`.

## Not tested

- Windows tests on the VM (`cargo xwin check` only).
- The full cost matrix. Main runs it after about 6 merged PRs.
- Any engine other than the old one. 6c-2 adds js2.
- The `BARE_JS_EXEC` switch. It does not exist yet.

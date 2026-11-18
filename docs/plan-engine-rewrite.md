# Implementation plan: the new JavaScript engine

Status: plan, for review by the owner with the design. Date: 2026-11-18. Base: main `d89f0299`.

This plan turns `design-engine-rewrite.md` (the design) into pull requests. The design says what to build and why. This plan says in which order, in which files, and how each step is checked. Section numbers such as "6.11" point into the design.

## 1. Scope and rules

- Workload: the daily journeys. They are the staging journey, the typing page (`type_load`) and the public journeys of `tools/bench/journeys.py` (design 6.12).
- Priorities: CPU instructions, then peak RSS, then time to ready.
- The bars:
  - S2: `eval_load` instructions at or below QuickJS.
  - S5: lower than today's engine (`auto`) and Chrome on the daily journeys; `eval_load` peak RSS at or below QuickJS.
- One PR is one concern, at most about 400 changed lines of code (tests not counted). A larger task is split.
- Every PR runs `cargo test --locked -p <crate>` for the crates that it touches, and the cost rows of the files that it touches (`tools/js/cost_matrix.sh -g`). CI runs the whole workspace.
- Every PR that changes a number records it in `docs/progress.md`: the machine, the inputs, the run count and the spread. Instruction counts are the minimum of 3 runs. Wall time counts only on an idle machine.
- Every stage ends with a gate (design section 7) and a learning page in `docs/learn/` that explains the stage in plain words.
- The old engine stays the default until S5 passes. `BARE_JS_EXEC=new` selects the new engine.
- One `Closes #N` per line in a PR body.

## 2. Order and dependencies

```
P0 (today's engine) ─┬─► S0 host interface ─► S1 heap ─► S2 core ─► S3 language ─► S4 built-ins ─► S5 browser ─┬─► S6 baseline JIT (if the profile says so)
                     │                                                                                          └─► S7 remove old engine ─► S8 speculating JIT (if the profile says so)
                     └─ measurement tools (M) run beside every stage
```

- P0 and M can start at once and in parallel.
- S0 and S1 can run in parallel after P0.1 (the feature census): S1 does not need the host interface until S2's browser slice.
- S3 and S4 overlap: a built-in group can start when the language features it needs exist.
- S6 and S7 are independent. S6 runs only if the S5 profile shows hot code. S8 runs last.

## 3. Measurement tools (M)

Each tool goes in `tools/js/`. The scratch versions of 2026-11-18 are in `/data/bare-tmp/quickjs-baseline-2026-11-18/` and `/data/bare-tmp/voicebot-e2e-2026-11-18/subsystem-profile/`.

| Id  | Task                                                                                                                                                                                                                                      | Check                                                                      | Size |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ---- |
| M1  | `engine_compare.sh`: run the `eval_load` cases on Bare (`tree`, `auto`, `new`), QuickJS and Node. Instructions (`perf stat`), peak RSS (a small C wrapper, `wait4`), 3 runs. Builds QuickJS from a pinned tarball with a checked SHA-256. | Its table matches the numbers of `research-quickjs-reference.md` within 1% | S    |
| M2  | `subsystem_profile.sh`: `perf record` of a daily journey and the split by subsystem (`classify.py`, `jscat.py`), with the collector counted by any frame on the stack (design 3.1).                                                       | Matches the 2026-11-18 split of the staging journey within the run spread  | S    |
| M3  | `layout_census`: peak bytes by layout (objects, shapes, elements, strings, bytecode, feedback, value slots) in `auto` and in `new`. A counter in each engine, printed at exit with `BARE_JS_STATS=1`.                                     | The sum is within 5% of the heap bytes that massif shows                   | M    |
| M4  | Long-session row: the daily journeys repeated 20 times in one session through `journeys.py`, peak RSS per round.                                                                                                                          | Runs on today's engine; the slope per round is recorded                    | S    |
| M5  | Ready time: the time until the first journey step can act, through `bare-drive`, on a replayed network cache (`journeys.sh run`). All processes of the browser count.                                                                     | Spread under 5% on 5 runs on an idle machine                               | S    |

## 4. P0: measurements and small changes in today's engine

These land in `crates/js` before S1. They give real numbers early, and the daily journeys get faster now (design 10, question 12).

| Id   | Task                                                                                                                                                                                                                    | Files                                       | Check                                                                                                                                      | Size                                                                            |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| P0.1 | Feature census: count each syntax kind that compiles or runs, each built-in object and each built-in method called. Behind a cargo feature `census`, printed at exit. Run it on the daily journeys and record the list. | `eval*.rs`, `bc_gen*.rs`, a new `census.rs` | The list is in `docs/progress.md`; with the feature off, the cost rows do not move                                                         | M                                                                               |
| P0.2 | Collector counters: objects made since the last collection that survive it, and the largest number of bytes allocated within one task.                                                                                  | `eval.rs` (`gc_due`, `collect`), `heap.rs`  | The survival rate and the largest task allocation of the staging journey and the typing page are in `docs/progress.md`                     | S                                                                               |
| P0.3 | `bc-hist`: a `GetEnv` hop-count histogram, and the reads kept in the environment only because of `lazy_inner` (`bc_gen_rf.rs:51`).                                                                                      | `bc_vm.rs`, `bc_gen_rf.rs`                  | Histogram of the staging journey in `docs/progress.md`                                                                                     | S                                                                               |
| P0.4 | Constant operands on either side of a compare: swap the operands for `==`, `!=`, `===`, `!==`, and mirror `<`, `>`, `<=`, `>=` (design 6.11 item 5).                                                                    | `bc_gen_rf.rs` (about line 297)             | A test per operator with a constant on the left; `bc-hist` on the staging journey: fewer `LoadNull`, `LoadI`, `LoadK`; cost rows not worse | S                                                                               |
| P0.5 | Effect context for `&&`, `                                                                                                                                                                                              |                                             | `and`?:`in`expr_void`, and for the non-last parts of a comma expression (design 6.11 item 6).                                              | `bc_gen_rf.rs` (`expr_void`, about line 345), `bc_gen_expr.rs` (about line 183) | Tests for each form; fewer `StrictNe` then `JmpIfNot` pairs on the staging journey; cost rows not worse | S   |

P0.4 and P0.5 test the design's claim that the compiler can remove a large share of ops. If they remove much less than the hypothesis, the design is checked again before S2.

## 5. S0: host interface and GC contract

Today `crates/bare` uses `bare_js` in 184 files (`bare_js::eval` 111 uses, `value::Value` 82, `heap::HeapLimits` 82, `Interp` 41). S0 puts one interface between them, so the browser can run on either engine.

| Id           | Task                                                                                                                                                                                                                        | Check                                                               | Size       |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- | ---------- |
| S0.1         | Inventory: list every `bare_js` item that `crates/bare`, `crates/drive` and `crates/mcp` use, grouped by kind (values, objects, calls, wrappers, microtasks, modules, limits). Write it in `docs/design-host-interface.md`. | The owner reviews the list                                          | S          |
| S0.2         | A new crate `crates/js-api` (`bare-js-api`): the host interface as traits and handle types. Handle scopes and persistent handles of design 6.8. No engine code.                                                             | `cargo check`; doc tests of the handle rules                        | M          |
| S0.3         | The old engine implements `bare-js-api` (an adapter in `crates/js`).                                                                                                                                                        | Unit tests of the adapter                                           | M          |
| S0.4 to S0.n | Move the browser to `bare-js-api`, one subsystem per PR (DOM wrappers, events, timers and microtasks, fetch, modules, forms, CSSOM, navigation).                                                                            | Each PR: the journeys of that subsystem pass; cost rows within 0.5% | L in total |
| S0.x         | A stress mode: collection forced at every allowed safepoint of the old engine (between tasks).                                                                                                                              | The daily journeys pass under it                                    | S          |

Gate S0: every journey passes on the old engine through `bare-js-api`; the instruction change is under 0.5%; the stress mode passes; the census of P0.1 is recorded.

## 6. S1: values, heap, collector, strings, shapes and objects

New crate `crates/js2` (`bare-js2`). Only `heap` and `value` modules may hold `unsafe` code.

| Id    | Task                                                                                                                                                                        | Check                                                               | Size |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- | ---- |
| S1.1  | Crate skeleton, `BARE_JS_EXEC=new` switch (it fails with a clear error until S2), CI job with Miri on `value` and `heap`.                                                   | CI green                                                            | S    |
| S1.2  | `Value`: 64-bit NaN-boxing, the address invariant, exposed provenance, NaN canonicalization (design 6.1).                                                                   | Unit tests and Miri; a micro row for encode and decode              | M    |
| S1.3  | Allocator: size-class blocks with lazy sweep; large objects in their own pages; pages given back to the OS (design 6.10 item 15).                                           | Allocation micro rows; live against committed bytes after churn     | M    |
| S1.4  | Cell header: compare a 32-bit shape id plus flags with a raw 8-byte shape pointer (design 6.11 item 12). Keep the winner.                                                   | Allocation and mark micro rows; the result in `docs/progress.md`    | M    |
| S1.5  | Collector: mark with a chunked work list, mark bit before push, typed trace per layout, ephemerons, weak tables (design 6.9 item 3, 6.11 item 3). Roots walked in place.    | Unit tests; stress test with a random object graph; no leaks        | L    |
| S1.6  | In-task collection: the allocation slow path collects when a task passes the trigger. Root maps are a data structure here; S2 fills them from the compiler.                 | A test that collects inside a task with live values in a fake frame | M    |
| S1.7  | Strings: one-byte and two-byte, inline short strings, ropes for long concatenations, atoms from source shared by realms, run-time atoms weak (design 6.10 items 13 and 14). | Unit tests; concat micro rows against QuickJS                       | M    |
| S1.8  | Shapes: transition tree, shared descriptor tables, dictionary mode, per-site slack (design 6.4, 6.11 item 15).                                                              | Unit tests; shape census on a synthetic React-like trace            | L    |
| S1.9  | Objects: shape header, inline slots, overflow record only for objects that grow, elements in one tagged layout (design 6.4, 6.9 item 8, 6.11 item 14).                      | Unit tests; layout census (M3) on synthetic objects                 | M    |
| S1.10 | Young generation (sticky marks) as an experiment: kept only if the P0.2 survival rate and S1 rows show a gain.                                                              | A/B on allocation rows; barrier cost measured                       | M    |

Gate S1: the M3 census in `auto` exists before S1.4 and S1.10 decide; allocation and collection micro rows; no leaks under the stress test; Miri clean.

## 7. S2: front end, compiler and interpreter

| Id   | Task                                                                                                                                                                                                                                                               | Check                                                           | Size |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- | ---- |
| S2.1 | Lexer: port today's streaming lexer with borrowed spans (design 6.9 item 9).                                                                                                                                                                                       | Today's lexer tests; `js_lex_load.sh` rows not worse            | M    |
| S2.2 | Pre-parser: a check-only mode that records the captures of each inner function; nested skip ranges (design 6.9 items 4 and 9).                                                                                                                                     | Tests on nested closures; each function pre-parsed at most once | M    |
| S2.3 | Parse-ahead: claim the (source identity, mode) entry before the copy; one worker publishes (design 6.11 item 16).                                                                                                                                                  | Tests for duplicate submits and cancel                          | S    |
| S2.4 | Bytecode format: 32-bit words, constant operands for numbers, atoms, `null`, `undefined` and booleans; constants split by kind; loop-header table (design 6.10 items 1 and 7).                                                                                     | Encoder and decoder tests; a verifier                           | M    |
| S2.5 | Compiler: a per-function tree dropped after compile; scope analysis (registers, context slots, globals); value, effect and test contexts; destination-driven registers; `switch` tables; literal fill in one op; root maps per safepoint (design 6.5, 6.10, 6.11). | Compiler tests per construct; `bc-hist` on `eval_load`          | L    |
| S2.6 | Interpreter: one lean loop, one frame record on a JS stack, function cells, per-realm feedback vectors, caches on second use (design 6.10 items 3, 8 and 9; 6.11 item 11).                                                                                         | Interpreter tests; `eval_load` instructions per op              | L    |
| S2.7 | Core built-ins for test262: `Object`, `Function`, `Array`, `Error`.                                                                                                                                                                                                | test262 core subset                                             | M    |
| S2.8 | Browser slice: the typing page and a frozen copy of the staging scripts run through `bare-js-api`.                                                                                                                                                                 | Load, 50 keys and first usable input measured against `auto`    | M    |
| S2.9 | Superinstructions from the S2 histogram (design 6.11 item 8).                                                                                                                                                                                                      | Fewer dispatches; `eval_load` rows not worse                    | S    |

Gate S2: test262 for the core census features at a pinned revision; `eval_load` instructions at or below QuickJS (M1); the browser slice measured.

## 8. S3: the language features in the census

One PR per feature group, in the order the census ranks them by use. Candidates: classes and private names; destructuring and spread; generators and async (resumable frames in a heap object); modules; `for-in` with the shape cursor (design 6.11 item 10); `eval` and dynamic scopes, only if the census shows them. A feature outside the census throws a `TypeError` that names it and logs it (design 6.12).

Check per PR: test262 for that group, test by test against the old engine (no test that passes today fails); the differential runner (same script on both engines) is clean. Gate S3: one public journey runs on the new engine end to end.

## 9. S4: the built-ins in the census

One PR per built-in group, ranked by the census: likely `String`, `Array` methods, `Object`, `JSON`, `Promise`, `Map` and `Set`, `Math`, `Number`, `RegExp`, `Date`. Built-ins are created on first use from static recipes. Groups outside the census wait. Check per PR: test262 for the group against the old engine; the built-in rows of the cost matrix not worse than `auto`.

## 10. S5: the browser on the new engine

| Id   | Task                                                                                                                       | Check                                                     | Size |
| ---- | -------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- | ---- |
| S5.1 | DOM wrappers on request only, tracing through nodes without wrappers, host tracers per owner (design 6.11 items 4 and 17). | Tests: detached cycles freed, expandos of live nodes kept | M    |
| S5.2 | All daily journeys on `BARE_JS_EXEC=new`.                                                                                  | Every journey passes                                      | L    |
| S5.3 | Measure: M1, M2, M4, M5 against `auto` and Chrome; cold against cold and warm against warm.                                | The S5 gate of the design                                 | S    |
| S5.4 | Profile of the daily journeys on the new engine. Decide if S6 runs.                                                        | Decision in `docs/progress.md`                            | S    |
| S5.5 | Make `new` the default.                                                                                                    | Owner decision                                            | S    |

## 11. S6, S7 and S8

- S6, only if S5.4 shows hot code: a JIT platform spike on the three targets, then the baseline JIT (x64, then arm64) with calls as entry; OSR only if the profile shows long loops. Gate: design section 7.
- S7: remove `crates/js` when every daily journey passes on all three platforms. The `bare-js-api` adapter of the old engine goes with it.
- S8, last: profile the daily journeys on the finished engine. Build the speculating JIT only if hot loops are a large share.

## 12. Risks for the plan

- S0 is large: 184 files of the browser use the engine directly. Moving one subsystem per PR keeps each step small and checked by journeys.
- The census can miss a feature that a rare path of a journey uses. The `TypeError` with the feature name and the old engine as a fallback until S7 cover this.
- The P0 compiler changes can show a smaller gain than the design assumes. Then the design is checked again before S2, not after.
- Two engines drift. The differential runner runs on every PR from S2.

## 13. First PRs

1. M1 (`engine_compare.sh`), so every later step can be compared with QuickJS and Node.
2. P0.1 (feature census) and P0.2 (collector counters): they decide the scope and the collector.
3. P0.3 (hop histogram), then P0.4 and P0.5 (compiler changes in today's engine).
4. S0.1 (host inventory) and S1.1 (crate skeleton) in parallel.

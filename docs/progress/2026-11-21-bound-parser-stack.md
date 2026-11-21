# 2026-11-21: The parser stops at the stack it has

`parse_load deep 1` and `parse_load deep 10` aborted with a stack overflow (see [full-matrix](2026-11-21-full-matrix.md)). The cause: the recursive parser used more stack than a 2 MiB thread has, and nested classes did not count toward `max_depth`.

## Change

- `parse_class` counts as one level of depth in `crates/js/src/parse.rs` and `crates/js2/src/parse.rs`. The `enter` and the `depth -= 1` bracket `class_inner`, as in `parse_stmt`. An error inside `class_inner` returns through `r`, so the depth is always restored.
- `enter` checks the depth against `depth_soft`, which starts at `min(64, max_depth)`. At that depth the cold function `deepen` asks `stacker::remaining_stack()` how many levels fit (1920 bytes per level in release, 20 KiB in debug, 64 KiB reserve). It sets `depth_soft` to `depth + levels`, at most `max_depth`. If no level fits, `enter` returns `ErrorKind::Limit(LimitKind::Depth)`.
- `parse_cache` does not store a depth-limit error. A caller with more stack can parse the same source.
- `bare-js2` gets the `stacker` dependency (`bare-js` has it already).
- Tests: `deep_classes_respect_the_caller_stack` in both `parse_tests.rs` (2 MiB thread, 5000 nested classes, expects the depth error).

## Why the check is lazy

First version: call `remaining_stack()` once per `parse_counted` and per `Parser::new` (js2 makes a parser for each lazy function). The first call on a thread is expensive. Measured with a test program (1000 calls, instructions:u): first call on the main thread 75,000 instructions (glibc reads `/proc/self/maps`), first call on a spawned thread about 1,000, each later call 26. That version raised small rows: `parse_load full stmts 1` +0.69% glibc and +0.81% jemalloc, `script_run x=1` +12.1% glibc. The lazy check costs nothing for a program that nests less than 64 levels.

## Measurements

Machine: archMachine, 16 threads, other agents active. BASE: main `1303414b`. NEW: this branch. Release builds, own `CARGO_TARGET_DIR` for each tree. Each count is the minimum of 3 runs of `perf stat -e instructions:u` (`tools/js/instr.sh`).

`tools/js/cost_matrix.sh -J -g 'parse_load|eval_load'` (bare-js rows, both allocators):

- 16 `eval_load` rows: change between -0.061% and +0.014%.
- 8 `parse_load` rows (`stmts`, `chain`, `bodies`, `malformed`, scale 1 and 10): change between -0.264% and -0.137% (glibc and jemalloc). Layout change, not new work.
- `parse_load deep 1` and `deep 10`: BASE aborts with a stack overflow. NEW prints `errors_ok=4` (parens, blocks, arrows and classes all return the depth error) and exits 0. NEW counts: 6,884,395 and 7,091,438 instructions (glibc). The matrix cannot compare them because BASE crashes.

js2 rows (`parse_load MODE CASE 1` of `crates/js2/examples/parse_load.rs`, `script_run`), glibc / jemalloc change:

| Row | glibc | jemalloc |
| --- | --- | --- |
| `script_run` `var x=1;` | -0.151% | -0.026% |
| `script_run bundle_like.js` | -0.002% | -0.004% |
| `script_run eval_webpack.js` | 6,503,401 → 6,498,265 | not run |
| `script_run eval_calls.js` | 71,130,475 → 71,129,519 | not run |
| `parse_load full stmts` | -0.157% | -0.155% |
| `parse_load full chain` | -0.209% | -0.181% |
| `parse_load lazy bodies` | -0.124% | -0.098% |
| `parse_load lazy stmts` | -0.244% | -0.096% |
| `parse_load fns bodies` | -0.150% | -0.142% |
| `parse_load fns chain` | -0.211% | -0.180% |
| `parse_load fns stmts` | -0.171% | -0.169% |

No row is over +0.5%. The `codegen-units=1` check was not needed.

## Tests

- `cargo test --locked -p bare-js --lib parse`: 91 passed (includes `parse_cache`).
- `cargo test --locked -p bare-js2 --lib parse`: 102 passed.

## Not tested

- Windows and macOS. `stacker::remaining_stack()` uses OS calls there.
- A debug build below 2 MiB of stack. `LEVEL_BYTES` for debug (20 KiB) comes from the earlier measurement of about 15.5 KiB per nested class.
- The `bare` browser crate and the full test suites.
- `eval_load` rows of js2 (`script_run --eval`): the expected values were not set, so only plain runs of the data files were counted.

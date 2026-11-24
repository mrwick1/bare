# Rename js2 to Vex

Date: 2026-11-24. Branch: `t3/rename-js2-to-vex`. No behavior change.

## What changed

- `crates/js2` moved to `crates/vex` with `git mv`. The package is `bare-vex`. The Rust path is `bare_vex::`.
- `Cargo.toml`, the three dependent crates and `Cargo.lock` use the new name. The lock diff only renames the package and moves its entry.
- `crates/vex/src/prelude_facts.rs` is not tracked. `build.rs` writes it to `OUT_DIR`. No regeneration was needed.
- `tools/bench/classify.py` and `jscat.py` match `bare_vex::`. A substring check `'bare_js' in first` used to catch `bare_js2`. It now names both crates.
- `BARE_JS_EXEC=vex` selects Vex. `new` still works: `vex_from` gives Vex for every value except `tree`, `auto` and `byte`.
- `js2_from` is now `vex_from`. `SessionConfig::js2` is now `vex`. Two test files and two test functions changed their names.
- Comments, tool text and `CLAUDE.md` say Vex.

## Left on purpose

- Names of engine types in `crates/bare` (`eng_new.rs`, `New`, `Browser<New>`).
- Example binaries `js2_gen_load`, `js2_lex_load`, `js2_parse_load`, `js2_promise_load` (`tools/js/cost_rows.txt` names them).
- `tools/js/test262_js2.sh` and `tools/fixtures/js/test262-js2-*.txt`, and the reasons in `tools/fixtures/js/diff-known.txt`.
- References to `docs/` file names that contain `js2`.
- `tools/code-order/bare-drive.order`: the symbols embed `bare_js2` and a crate hash. Generate the file again with the next build.
- `crates/drive/tests/chunked.rs`: the local `js2_before_up` names a script `/js?2`. It is not the engine.

## Checks

- `cargo check --locked --workspace --all-targets`: exit 0.
- `t`: pass. `cargo test -p bare-vex --lib` for `regexp::`, `bi_uri_tests` and `script_tests`: pass.
- `python3 -I tools/bench/classify_test.py`: 2 pass.
- New case `vex_from("vex")`: the test did not compile before the rename, passes after.
- Release `bare-drive`, `tools/drive/public/paulgraham-home.steps`: pass with `BARE_JS_EXEC` unset and with `BARE_JS_EXEC=vex`.

## Not tested

Windows, the other public journeys, the benchmark rows, miri, a full test run, `BARE_JS_EXEC=new` end to end.

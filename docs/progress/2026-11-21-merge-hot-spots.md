## 2026-11-21: Remove the merge hot spots of parallel PRs

Three shared spots made parallel PRs conflict. No behavior change.

- test262 baseline: `tools/fixtures/js/test262-js2-baseline.txt` is now one file for each area (first two path parts), `tools/fixtures/js/test262-js2-baseline/<area>.txt`, for example `built-ins/Promise.txt`. 37 files. Same header and line format. `tools/js/test262_js2.sh -u` writes only the files whose content changed and removes the files of areas that are gone. The old file and the 37 files hold the same lines (compared sorted). The tool only writes the baseline; no tool reads it. `test262_check.sh` has its own file (`test262-baseline.txt`, old engine); I did not change it.
- Install list: each built-in already had its own `install_*` function and one line in `install_builtins`. The lines are now sorted, with the comment "Add one line, keep sorted". `object`, `function` and `error` stay first. The rest depends on them.
- Flags: `object.rs` has a table of every `F_*` bit and its owner, and a compile-time check that all flags are distinct powers of two inside the 24 flag bits of `CellHeader`. Bits 16 to 23 are free (8 bits). No flag is renumbered.

Checks (archMachine):
- `tools/js/test262_js2.sh -g 'built-ins/Symbol'` runs. A full `-j 8 -u` run (279 s) on the unchanged tree leaves the 37 files identical to the migrated ones.
- `cargo test --locked -p bare-js2 --lib`: 545 pass. `cargo check -p bare-js2`: pass.
- Startup, `script_run` on `var result = 1;`, `perf stat -e instructions:u`, 3 runs each, base vs this tree: 2,011,652 vs 2,012,591 mean, +0.047%. Run spread 0.1%.

Not tested: Windows; the full cost matrix; test262 after the final merge with main.

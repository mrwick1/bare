## 2026-11-21: Remove the merge hot spots of parallel PRs

Two shared spots made parallel PRs conflict. No behavior change.

- test262 baseline: `tools/fixtures/js/test262-js2-baseline.txt` is now one file for each area (first two path parts), `tools/fixtures/js/test262-js2-baseline/<area>.txt`, for example `built-ins/Promise.txt`. The 50 files contain the same 779 data rows as the baseline on main at the merge. `tools/js/test262_js2.sh -u` writes only the files whose content changed and removes the files of areas that are gone. `test262_check.sh` uses its own baseline for the old engine.
- Install list: each built-in already had its own `install_*` function and one line in `install_builtins`. The lines are now sorted, with the comment "Add one line, keep sorted". `object`, `function` and `error` stay first. The rest depends on them.
- At the merge, `object.rs` uses the kind field and flag table from main.

Checks (archMachine):

- `tools/js/test262_js2.sh -g 'built-ins/Symbol'` runs. A full `-j 8 -u` run (279 s) on the unchanged tree leaves the 37 files identical to the migrated ones.
- `cargo test --locked -p bare-js2 --lib`: 545 pass (552 after the merge with main). `cargo check -p bare-js2`: pass.
- Startup, `script_run` on `var result = 1;`, `perf stat -e instructions:u`, 3 runs each, base vs this tree: 2,011,652 vs 2,012,591 mean, +0.047%. Run spread 0.1%.

Not tested: Windows and the full cost matrix. At the latest merge, a sorted comparison verified the split against the baseline on main.

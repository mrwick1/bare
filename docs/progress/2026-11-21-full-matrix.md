# 2026-11-21: Full cost matrix after 26 merged PRs

The cost gate (owner, 2026-11-21) says the full matrix runs on main after about every 6 merged PRs. This entry is the first run. No code changed.

## Setup

- Machine: archMachine, AMD Ryzen 9 5900HX (16 threads), Linux 7.2.15-zen1, other agents active (instruction counts do not depend on load).
- BASE: `8875c60a` (main before the 2026-11-21 work). NEW: `852c7036` (origin/main at the start of this run, merge of #841). 26 first-parent merges lie between them.
- Build per tree (own `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=4`): `cargo build --release --examples -p bare-js -p bare` and `cargo build --release -p bare-stress`. `interp_load` comes from `cargo build --release -p bare-js2 --example interp_load`, built last.
- Matrix: `tools/js/cost_matrix.sh -j4 -J` on all 819 rows of `tools/js/cost_rows.txt`. Each row is the minimum of 3 runs of `perf stat -e instructions:u` (`tools/js/instr.sh`), under glibc and jemalloc.
- Rows over +0.5% under both allocators were measured again by hand: 3 runs each, default build and `RUSTFLAGS="-C codegen-units=1"` on both trees. The spread is the largest max-minus-min of the 3 runs.
- js2 micro rows: `interp_load CASE N` minus `interp_load CASE 0`, 3 runs, both allocators.
- Build note: At the measured commits, `-p bare-js2 --examples` overwrote the bare-js examples `gen_load`, `lex_load`, `parse_load` and `promise_load`. The first matrix run used that build. The reported run built `interp_load` alone after the bare-js examples. The [example rename](2026-11-21-js2-example-names.md) removes the clash in later builds.

## Result of the matrix

817 rows measured, 2 rows failed. Mean change: glibc -0.263%, jemalloc -0.237%. No row is over +0.5% under one allocator only.

`parse_load deep 1` and `parse_load deep 10` abort with a stack overflow on both BASE and NEW (the `parse_load` thread has a 2 MiB stack). This is not a change between the trees. Hypothesis: the toolchain or a frame size moved before BASE. It needs its own fix.

## Rows over +0.5% under both allocators (default build)

| Row                           | glibc base    | glibc new     | glibc change | jemalloc change | spread  | glibc cgu=1 | jemalloc cgu=1 |
| ----------------------------- | ------------- | ------------- | ------------ | --------------- | ------- | ----------- | -------------- |
| `dataset_load keys 10000 50`  | 3,136,831,545 | 3,206,025,704 | +2.206%      | +2.204%         | 233,561 | +1.993%     | +2.001%        |
| `dataset_load keys 1000 50`   | 330,321,305   | 337,123,536   | +2.059%      | +2.011%         | 9,268   | +1.862%     | +1.817%        |
| `dataset_load read 10000 50`  | 179,008,115   | 180,093,418   | +0.606%      | +0.560%         | 30,442  | +0.558%     | +0.513%        |
| `dataset_load write 10000 1`  | 118,828,169   | 120,089,296   | +1.061%      | +0.941%         | 30,430  | +1.001%     | +0.875%        |
| `dataset_load write 10000 50` | 150,186,676   | 151,296,992   | +0.739%      | +0.665%         | 11,180  | +0.670%     | +0.619%        |
| `intl_load nf_locale_opts 1`  | 148,775,109   | 149,546,108   | +0.518%      | +0.533%         | 21,508  | -0.000%     | +0.000%        |
| `intl_load nf_locale_opts 10` | 1,461,965,602 | 1,469,676,713 | +0.527%      | +0.555%         | 186,966 | -0.000%     | +0.008%        |

- `intl_load nf_locale_opts`: passes at `codegen-units=1` (0.000% on both). The growth is a codegen-unit split, not new work. No action.
- `dataset_load` rows: still fail at `codegen-units=1`. Bisect below.

The known exception rows `dataset_load keys 10000 1` and `dataset_load read 10000 1` were not over +0.5% in this run.

## Bisect of the `dataset_load` rows

I built `dataset_load` at commits of the first-parent history and counted `keys 1000 50` and `write 10000 1` (glibc, default build, 3 runs).

| Commit                 | `keys 1000 50` | `write 10000 1` |
| ---------------------- | -------------- | --------------- |
| `8875c60a` (BASE)      | 330,321,305    | 118,828,169     |
| `05a6acf1` (#830)      | 330,037,451    | 118,732,348     |
| `724b7a25` (#825)      | 330,215,025    | 118,602,888     |
| `c810d21e` (#842)      | 330,215,289    | 118,602,526     |
| `74e542c5` (#843)      | 337,124,239    | 120,089,843     |
| `852c7036` (#841, NEW) | 337,123,536    | 120,089,296     |

The first first-parent merge over the limit is `74e542c5` (#843, PR 6b). #843 was stacked on #841 (PR 6a), so its merge brought in the commits of #841. Inside that merge:

| Commit                                                                                                 | `keys 1000 50` | `write 10000 1` |
| ------------------------------------------------------------------------------------------------------ | -------------- | --------------- |
| `8c9e784b` (merge before 6a code)                                                                      | 330,214,755    | 118,602,428     |
| `aa99a2e9` "Remove the old Host impl of Dom; install the textarea and onabort members from tables"     | 337,224,063    | 128,881,435     |
| `c8b7c1cc` "Write dataset and storage names without a second intern; stack buffer for the named write" | 337,121,515    | 120,087,670     |
| `44a58e42` (6b gate, a test and a list only)                                                           | 337,122,110    | 120,088,130     |

Cause: the PR 6a commit `aa99a2e9` (PR #841). It adds +2.1% to `keys` and +8.6% to `write 10000 1`. The next 6a commit `c8b7c1cc` takes back most of the `write` growth and none of the `keys` growth. #843 only adds a test and a list. This matches the owner-accepted exception of #841 for the `dataset_load` rows. I did not run callgrind. Hypothesis for the `keys` path: the dataset name write now goes through the table install path. Re-check after PR 6c.

## js2 micro rows (`interp_load`, net of N = 0)

js2 is not in the browser yet. These rows fall under the #757 rule. They are listed here only.

Default build:

| Row                | glibc base    | glibc new     | glibc change | jemalloc change | spread |
| ------------------ | ------------- | ------------- | ------------ | --------------- | ------ |
| `loopfn 3000000`   | 799,501,156   | 886,500,204   | +10.882%     | +10.881%        | 1,334  |
| `calls 1000000`    | 571,500,777   | 594,500,938   | +4.025%      | +4.024%         | 1,929  |
| `fib 27`           | 257,347,397   | 262,113,345   | +1.852%      | +1.865%         | 2,242  |
| `closures 1000000` | 1,940,723,213 | 1,964,214,673 | +1.210%      | +1.214%         | 13,890 |
| `objects 1000000`  | 719,355,500   | 712,260,812   | -0.986%      | -0.982%         | 1,424  |

With `RUSTFLAGS="-C codegen-units=1"` on both trees:

| Row                | glibc base    | glibc new     | glibc change | jemalloc change | spread |
| ------------------ | ------------- | ------------- | ------------ | --------------- | ------ |
| `loopfn 3000000`   | 847,500,704   | 850,500,574   | +0.354%      | +0.354%         | 997    |
| `calls 1000000`    | 578,500,885   | 565,500,466   | -2.247%      | -2.247%         | 1,518  |
| `fib 27`           | 250,673,289   | 248,766,108   | -0.761%      | -0.748%         | 2,192  |
| `closures 1000000` | 1,942,212,317 | 1,918,269,472 | -1.233%      | -1.230%         | 14,676 |

All four rows pass the rule at `codegen-units=1` (within +0.5%). The default-build swings (+10.9% to -2.2% between builds) are a codegen-unit split of the interpreter loop. They are not new work. `objects` was under the limit and was not re-run.

## Verdict

- Rows that fail the rule: the 5 `dataset_load` rows. Cause: PR #841 (PR 6a, commit `aa99a2e9`), which arrived on main through the merge of #843. Owner-accepted exception; re-check after 6c.
- Rows that pass at `codegen-units=1`: 2 `intl_load nf_locale_opts` rows, 4 js2 micro rows (js2-only, #757 rule).
- Broken rows: `parse_load deep 1` and `parse_load deep 10` (stack overflow on BASE and NEW).

## Not tested

- Windows.
- Callgrind of the `dataset_load` growth.
- The bisect used `keys 1000 50` and `write 10000 1` only, not all five rows.
- Wall time (instruction counts only).

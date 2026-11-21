# 2026-11-21: merge tooling

## Prelude facts

[PR #904](https://github.com/mrwick1/bare/pull/904) moves the generated Rust tables to Cargo's output directory. The build script compiles the current lexer, parser and RegExp syntax checker for the host. The engine still reads static tables. A runtime parse would add work at the first prelude use. A merge driver would retain a tracked file and still require a refresh.

On archMachine, the release builds used separate empty targets, four Cargo jobs and the existing compiler cache. The base was `cf3d1bed`. One build per tree of `script_run` and `interp_load` took 70.93 s on the base and 68.83 s with generation. Other jobs ran on the machine, so these wall times give a rough comparison.

Startup ran `script_run` with `var x = 1;`. The table gives the minimum of three runs per allocator and build. The counter was `perf stat -e instructions:u`, with `glibc.malloc.tcache_count=65535` and `libjemalloc.so.2` for the jemalloc runs.

| Allocator | Base min | Base max | New min | New max | Change |
| --- | --- | --- | --- | --- | --- |
| glibc | 731,362 | 732,694 | 731,638 | 731,787 | +0.038% |
| jemalloc | 4,109,004 | 4,110,554 | 4,109,426 | 4,110,185 | +0.010% |

The five `interp_load` rows were within 0.001% under both allocators. The inputs were `loopfn 3000000`, `calls 1000000`, `fib 27`, `closures 1000000` and `objects 1000000`.

All 16 tests filtered by `prelude_` passed. For the old JS input, the generator reproduced the checked-in Rust apart from the file header. Cargo rejected a syntax error added to the prelude and passed `cargo check --locked -p bare-js2` after the restore.

## Merge keeper

Run `tools/merge-keeper.sh --dry-run` to try each merge and report conflicts. The script uses a temporary worktree and target directory per PR. It selects open PRs into `main` with auto-merge enabled and state `DIRTY`. It skips fork branches. For a supported conflict it takes main's generated files and keeps debt rows from both branches. This also restores a conflicted baseline that the PR deleted if main still has it. The script refreshes prelude facts only if the merged tree still tracks the file. It checks both crates, verifies that the PR still qualifies, and pushes only if the remote head still has the expected commit. The worktree and target directory are removed after each attempt.

The local Git fixture covered selection, dry run, successful push, both versions of prelude generation, conflict rejection, a failed Cargo check, a change in PR eligibility and cleanup. The fixture used a disposable remote and stubbed GitHub and Cargo. The real Cargo command `cargo check --locked -p bare-js2 -p bare --tests` passed. A live dry run resolved the baseline conflict in #863 and #833. It reported source conflicts in #889 and #827 and skipped their updates.

The install list in `builtins.rs` already has one call per line. Most lazy tables already live next to the built-ins that own them. `lazy.rs` holds the shared lookup and materialization code. Reordering table entries can change the order of own keys when a script lists them. New install calls can still conflict when both branches add a call at the same position.

No live PR heads were updated by the script. Windows, Miri, test262 and the full suite did not run locally.

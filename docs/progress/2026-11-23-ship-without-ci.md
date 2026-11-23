# Ship without CI

Owner decision, 2026-11-23: remove CI and merge and release with one local command.

## Why

CI almost never gave a result. Of the last 200 PR runs, 104 were cancelled, 83 skipped and 12 failed. Of the last 40 runs on main, 39 were cancelled: each merge cancelled the run of the merge before it (`cancel-in-progress`). The `check` job took about 11 minutes.

## Change

- `.github/workflows/ci.yml` is removed. Main has no required status checks (`check`, `guard`, `windows`).
- `tools/ship.sh`: on a branch, it merges origin/main, runs `cargo check --all-targets` of the changed crates, `t` and the scan of `tools/portable_check.sh`. Then it pushes, opens the PR if there is none, and merges it. `--release` also runs `tools/release.sh`.
- `tools/release.sh`: it tags origin/main with the next patch version, or with the version you give. The tag starts `release.yml` (build of 3 targets, offstage gate, publish).
- `CLAUDE.md`: the merge rules name `tools/ship.sh`.

## What is lost

- Miri on the unsafe modules of js2. Run it by hand for a change to such a module.
- The full workspace test run. `t` runs only the tests of the changed files.
- A Windows build for each PR. `release.yml` still builds Windows before it publishes.
- The `guard` job: an autofix branch can now change `.github/`.

## Tests

- This PR merged through `tools/ship.sh`.
- The version step of `tools/release.sh` on the tag `v0.1.17` gives `v0.1.18`. No tag was pushed.

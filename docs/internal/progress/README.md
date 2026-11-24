# Progress entries

Each PR writes its result to its own file in this folder. Two PRs never edit the same progress file, so they do not conflict.

- File name: `YYYY-MM-DD-<slug>.md`, for example `2026-11-21-js2-promise-api.md`. Use the date of the work and the topic of the PR.
- Content: the same as the old entries of `../progress.md`. Give what changed, the measurements (machine, inputs, run count, spread), the test results, and what was not tested.
- One file per PR. A follow-up PR adds a new file and links the old one.
- `../progress.md` holds the entries up to 2026-11-21. It is not appended to any more. Its section "Engine rewrite: status" is a short index that the main session updates at each milestone.

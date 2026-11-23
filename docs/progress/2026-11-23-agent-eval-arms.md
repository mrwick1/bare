# Agent-eval arms: compare snapshot formats per task (#964)

## Change

- `bare-mcp` reads `BARE_MCP_SNAPSHOT_MODE` (`interactive` or `all`) once. It is the default snapshot mode when a call gives no `mode`. Any other value means `interactive`. A `mode` in the call wins.
- `tools/agent-eval/run.py`: `fixture --arm` and `smoke --arm` (`interactive` default, or `all`). `fixture` prints `arm` and `mcp_env` in its JSON line. The arm goes into `manifest.json`, `state.json` and `result.json`. `score` takes the arm from `state.json`.
- `report` adds `by_arm` to its JSON (stdout) and a table (stderr): per task and arm, runs, success rate, median turns (tool calls) and median total tokens. Tokens come from `--tokens-json`. Without it, tokens are tool-output characters / 4, labelled `estimate`.
- `transcript.metrics` adds `response_chars`. `tasks.VERSION` is unchanged: no prompt, fixture or grader changed.
- `docs/agent-evaluation.md`: new "Arms" section with the commands for an external run.

## Results

- Rust: `snapshot_default_mode_tests` (4 tests). Before the change `the_env_sets_the_default_mode_to_all` failed. After it, 4 of 4 pass.
- `python3 -m unittest discover -s tools/agent-eval/tests`: 70 tests, OK (new file `test_arms.py`).
- Scripted smoke run, one run per arm, release `bare-mcp` built from this branch: `interactive` 8/8 completed, `all` 8/8 completed. `report` shows 16 rows (8 tasks x 2 arms). Median tokens are estimates. Example: `download` 136 (`interactive`) and 132 (`all`); `nav_stale_ref` 373 and 392; the other six tasks differ by 0 tokens.
- This is a **scripted** result. A scripted sequence knows the page. It shows the plumbing. It does not show the effect of an arm on an agent.

## Not tested

- No model agent run. No arm result for success, turns or tokens of a real agent.
- No Windows run. The code uses stdlib paths and one env var. The new Rust test starts `bare-mcp` through `CARGO_BIN_EXE_bare-mcp`.
- `report` over a mix of results with and without an arm (they group as `none`) is covered by a unit test only.
- The `snapshot` tool schema text still says "interactive (default)". It does not mention the env var.

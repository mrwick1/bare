# bare-mcp metrics line (#965)

## What changed
- `crates/mcp/src/metrics.rs` appends one JSON line per finished tool call: `ts`, `tool`, `ms`, `out_chars`, `ok` and, on a failure, `code`.
- The hook is in `Shared::finish`. `Job` has a new field `t0`. Internal jobs (no request id) and cancelled calls write no line.
- A line holds no URL, page text, typed text or credential. A tool name that is not plain ASCII is written as `unknown`.
- File: `BARE_MCP_METRICS=<path>`. Unset, it is `mcp-metrics.jsonl` in `bare_os::cache_dir()` (Linux `~/.cache/bare`, Windows `%LOCALAPPDATA%\bare\cache`). `off` or empty switches it off. An empty `BARE_CACHE_DIR` also switches it off.
- Rotation: at 5 MiB the file moves to `<file>.1` (one old file) and a new file starts.
- The file opens once, in append mode (mode 0600 on Unix). Each call does one `write`. An error is ignored.
- `bare-mcp --stats [FILE]` prints per tool: calls, p50, p95, max ms (nearest rank) and the error share.
- `bare-mcp` now depends on `bare-os` (a workspace crate; `Cargo.lock` has one new line).

## Cost
Machine: archMachine, release build, loopback page, 1000 sequential `snapshot` calls after one `open`. 9 runs per mode, interleaved.

| Logging | Median | Runs (ms per 1000 calls) |
|---|---|---|
| off | 43.7 | 49.2, 42.7, 42.3, 39.2, 43.2, 44.1, 44.4, 43.7, 44.6 |
| on | 50.8 | 52.2, 50.8, 49.3, 50.6, 49.4, 44.6, 53.2, 53.3, 55.5 |

Added cost: about 7 µs per call (0.007 ms; the spread of one mode is about 10 ms per 1000 calls). The limit was 0.1 ms. The log has 1001 lines for the 1001 calls. The time that the log records for a `snapshot` is 0.02 ms.

## Tests
- `crates/mcp/tests/metrics_tests.rs`: two lines for `open` and a failing `act`, right fields, no page text, no query string, no host; the off switch; `--stats` numbers. 3 passed.
- `t`: 104 passed, 1 ignored (existing), 2 passed.

## Not tested
- Windows (the code uses `cache_dir` and a remove-then-rename rotation for Windows, but it did not run there).
- Rotation at 5 MiB (the code path has no test).
- Many sessions that write at once (one mutex guards the file).
- The test that fails before the change: not run without the code.
- No cost-matrix rows: the change touches no engine code.

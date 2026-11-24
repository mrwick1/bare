# The session_load example runs on js2

Date: 2026-11-23. Machine: archMachine.

## Error before

`BARE_JS_EXEC=new session_load chain 1` panicked at `crates/bare/src/browser_any.rs:347`: "this browser runs on js2".
The example calls `b.stats()` on the `AnyBrowser` of the session. `AnyBrowser` had no `stats`, so the call went through `Deref` to the old-engine view, and that view panics on js2.
`BARE_JS_EXEC=auto` passed.

## Fix

One line: `fn stats()` is forwarded in the `fwd!` list of `browser_any.rs`. `Browser::stats` is generic, so both engines have it.

## Tests

- New: `browser_any_stats_tests::stats_work_on_a_js2_browser`. It failed with the same panic before the fix and passes now.
- `session_load chain 10` and `heavy 10` print OK with `BARE_JS_EXEC=new` and `auto`.
- `t`, `cargo test -p bare --lib old_engine_gate` and `cargo clippy -p bare --all-targets` (no warning in the changed files) pass.

## Not tested

- Scales 1 and 100 and the caps of `tools/js/session_load.sh` on js2. Only 10 (and 1 before the fix) ran.
- The cost matrix rows. No cost measurement.
- Other uses of the `Deref` to the old engine in other examples and tests.

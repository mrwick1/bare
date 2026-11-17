# Engine monitoring

Monitoring is opt-in. Performance acceptance is reported below separately from functional coverage.

## Invocation and ownership

Build with symbols: `cargo build -j3 --profile profiling -p bare-drive`.
The profiling profile inherits release, enables debug symbols, and disables stripping.
The normal release profile is unchanged.

Example: `BARE_OBSERVE=trace BARE_OBSERVE_FILE=/tmp/journey.jsonl target/profiling/bare-drive --quiet journey.steps`.
Use `metrics` for process samples and engine counters. Use `trace` to add durations.
The default is `off`. Off mode creates no capture or monitoring threads.
Each executable owns initialization and shutdown. One enabled owner may start per process.
A rejected second start creates no file or thread and does not reset the existing owner.
Initialization publishes the global mode only after collector and sampler creation succeed.
Normal returns drain the collector. The bare and bare-drive exit paths drain before `process::exit`.
Configuration and file-open errors print to stderr and exit with code 2.
Writer errors print to stderr at shutdown. MCP stdout remains protocol output.
A crash, forced termination, or blocked sink can prevent shutdown completion.

Suite workers append `.pid-PID` to `BARE_OBSERVE_FILE`.
The suite parent uses the exact configured path.
All captures use create-new semantics and Unix mode 0600.
This rule also applies when the suite launches workers through `perf`.

See [the collector contract](../crates/observe/README.md) for all controls and the JSONL schema.
`BARE_OBSERVE_INTERVAL_MS` controls process sampling and writer flush intervals.
The sampler stops after collector cap exhaustion or failure, at its next wake.
Category and level filters precede span clock reads and counter construction.
Context guards restore numeric attribution only on the thread that entered the scope.

## Measurements and coverage

Categories are `process`, `engine`, `html`, `js`, `gc`, `style`, `layout`, `paint`, `scheduler`, `network`, `loader`, and `driver`.
Trace spans use `duration_ns` and info level.
Paint has three events. `paint/build` times one display-list build. `paint/draw` times one raster pass. `paint/paint` is the headless span, and it encloses a `build` and a `draw`. The window path writes `build` and `draw` only, and it writes no `build` for a redraw with a retained list. To total paint time, sum `paint` or sum `build` and `draw`. Never sum all three.
HTML feed and finish, JS parse and execution, GC, retained and incremental style, layout, paint, scheduler pumps and individual tasks have duration hooks.
Scheduler pump results include executed tasks, the cap flag, queued tasks, message count and message bytes.
Driver readiness includes idle, stopped, load-failed and deadline flags. Arbitrary error text is excluded.
Loader milestones emit at their source without consuming legacy trace records.

Process samples have no session or document attribution.
They include optional RSS, peak RSS, allocator bytes, CPU microseconds and thread count.
Linux supports procfs counters. Allocator accounting requires glibc.
Other platforms return null for unsupported counters.
Allocator live bytes, resident bytes and JS accounted bytes measure different scopes.
No complete subsystem memory total is provided.

Owning-thread browser counters emit after scripts, microtask drains and explicit collections.
They include node, wrapper, listener, event, text-byte and attribute-slot counts.
They also include JS accounted bytes, live objects, collection count and slab capacity bytes.
Script entry also emits JS counters. These snapshots do not force collection or layout.
They read existing scalar state and capacities, with no heap traversal.
Process samples continue independently during long scripts.

Driver steps carry process-local numeric session and step IDs.
Network worker jobs and window handoffs copy these IDs into scoped thread-local guards.
Browser work adds document IDs. Requests add request IDs.
Some asynchronous records still lack document attribution. Process samples never inherit page IDs.

## Network timing semantics

DNS, TCP, TLS and request-header spans use the shared clock.
HTTP/1 `ttfb` measures the wait from entry into response-header reading to the first positive transport read.
It excludes request writes and connection setup. It emits only if a byte arrives.
HTTP/2 `h2_response_headers` measures the request call through delivery of the complete decoded response headers.
It includes request writes and stream-slot waits. It is not a first-frame or first-byte measurement.

`first_body_byte` measures from body construction after header parsing to first positive caller read.
It includes caller delay and can include bytes already buffered with the headers.
The `body` record emits when the wire body is dropped. It preserves its original numeric IDs.
`bytes` counts payload delivered before content decoding, excluding HTTP framing.
`status` is the numeric HTTP status. `outcome` is 0 for abandonment, 1 for observed EOF, or 2 for read failure.
`body_duration_ns` covers construction through drop in trace mode and is null in metrics mode.
It includes caller pauses and retention after EOF. It is not socket-active time.
Memory-cache and replay bodies have no wire-body record.
Body records add no buffering and do not enable legacy trace storage.

Names are fixed literals and field values are scalars.
Default hooks emit no URLs, query values, headers, bodies, source, console text or typed input.
Legacy switches retain their behavior. The driver still enables its own legacy network records independently.

## Bounds

The queue holds 256 records. Each record is limited to 4096 bytes and 32 fields.
Queued serialized payload is bounded by 1 MiB, excluding producer-local records and allocator overhead.
The default file cap is 64 MiB, with room reserved for terminal status.
Overflow drops records. Cap exhaustion stops recording.
Terminal status reports dropped records and the stop reason.
An I/O failure can leave an incomplete final line. Readers must discard it.
Shutdown waits for sink I/O. Flush does not imply fsync durability.

## Verification

Local fixtures cover real long-script sampling, concurrent sessions and documents, GC, headless paint, streaming bytes and outcomes, worker request attribution, seeded header/query/source/console/input secrets, suite per-PID captures, normal/error exit flush, mode distinctions and MCP protocol stdout.
Collector tests cover partial writes, failed writers, bounded overflow, filtering and file protection.
Process tests cover disabled thread absence, rejected duplicate ownership, failed initialization and sampler exit at the file cap.
A compile-fail doctest checks thread-affine context guards.

Commands:

```
cargo test -j3 -p bare-os -p bare-observe
cargo test -j3 -p bare-drive --test monitoring
cargo test -j3 -p bare-mcp --test monitoring
cargo test -j3 --workspace
cargo check -j3 --workspace
git diff --check
```

No GUI, non-Linux runtime or authenticated traffic is required or tested.
HTTP/2 first-frame timing and error-before-headers outcome records remain unavailable.
Memory attribution does not account for all engine subsystems or isolate logger allocator usage.

## Performance acceptance

Run `python3 tools/monitoring/accept.py "$PWD/target/profiling/bare-drive" /tmp/bare-monitor-accept-UNIQUE` on a committed candidate.
The script runs a bounded loopback HTML/JS journey 20 times per process, three runs per mode.
It records user-mode instructions with `perf stat`, child-tree peak RSS with Linux `wait4`, record counts and terminal drops.
Artifacts include the exact HTML, steps, commit, machine, raw perf output and protocol output.
No wall-time claim is made under machine load.
The provisional instruction budgets are disabled <=1%, metrics <=3%, trace <=10%.
A baseline off binary is required to accept the disabled budget separately.

Measured candidate: `006ce7f7413cd00062864302a85e2b1c36472425`.
Machine: archMachine, AMD Ryzen 9 5900HX, Linux `7.2.15-zen1-1-zen`.
Binary: profiling build, ELF debug_info present, not stripped.
Command: `python3 tools/monitoring/accept.py "$PWD/target/profiling/bare-drive" /tmp/bare-monitor-accept-process-rss`.
The exact HTML is stored in `results.json`. It has one div, a 100000-iteration JS loop and one timer.
The journey opens the loopback URL and checks that the div is visible.
Each measured process repeats the journey 20 times. Modes run in off/metrics/trace order for each of three rounds.
Counters include the `perf` workload and descendants. Browser peak RSS uses the driver’s procfs VmHWM readings after repetitions.
The artifacts also retain wait4 child-tree peak RSS, which can include perf and is not browser-only memory.

| Mode | Instructions in runs 1, 2, 3 | Mean overhead against candidate off | Peak RSS KiB in runs 1, 2, 3 | Records in runs 1, 2, 3 | Drops |
|---|---|---|---|---|---|
| off | 3238715621, 3238763872, 3238598090 | reference | 16928, 16700, 16856 | 0, 0, 0 | 0 |
| metrics | 3250270250, 3250149526, 3250053028 | +0.354% | 16764, 16952, 17040 | 343, 343, 343 | 0 |
| trace | 3258476653, 3258290136, 3258671925 | +0.611% | 16824, 16996, 17212 | 744, 743, 743 | 0 |

Instruction spreads (max minus min) are 165782 off, 217222 metrics and 381789 trace.
Metrics and trace meet the provisional instruction budgets on this one local journey.
This does not establish performance on other journeys or sampling intervals.
The default 1000 ms sample interval was used. Record-count variation comes from interval samples.
All enabled captures ended with terminal status and zero dropped records.
No off capture was created. No wall-time result is accepted under concurrent load.
The queued serialized logger payload limit is 1 MiB; separate logger allocator usage was not measured.
Raw artifacts remain at `/tmp/bare-monitor-accept-process-rss` on archMachine.

The continuation did not verify disabled overhead. The parent subsequently completed this comparison; see Parent verification below.

## Functional gate results for the continuation

- `cargo test -j3 -p bare-os -p bare-observe`: exit 0. Nine OS tests, twelve collector tests, four process tests and the thread-affinity compile-fail doctest pass.
- `cargo test -j3 -p bare-drive --test monitoring`: exit 0. Four tests pass.
- `cargo test -j3 -p bare-mcp --test monitoring`: exit 0. Protocol and error-exit checks pass.
- `cargo check -j3 --workspace`: exit 0. Existing warnings remain.
- `git diff --check`: exit 0.
- `cargo test -j3 --workspace`: exit 101 on two attempts. The bare library reports 1521 passed, one failed and six ignored.

The failing test is `browser_session_tests::a_pending_meta_refresh_is_queued_work`.
It fails at `crates/bare/src/browser_session_tests.rs:684` on `(5000.0..5010.0).contains(&b.next_scheduled_due().unwrap())`.
An isolated rerun also fails. The test source is unchanged from `00e701b0`.
Hypothesis: its absolute 10 ms allowance includes setup time on this loaded machine. This cause has not been verified against a baseline binary.
No scheduling behavior or unrelated test assertion was changed.
The workspace test gate is not accepted.

Supplemental gates:

- `cargo test -j3 -p bare --test fetch_https --test window_memory`: exit 0. Four synthetic TLS tests and five headless memory tests pass.
- `cargo test -j3 --workspace --exclude bare --no-fail-fast`: continues beyond individual target failures to check the remaining crates.
- `cargo test -j3 -p bare-drive --test chunked -- --test-threads=1`: exit 101. Both unchanged fixtures hit `time limit` at lines 147 and 173 after 10 seconds each. The workspace run hit the same limits.

The remainder run also reports `a_driver_click_step_returns_and_the_expect_waits_for_the_reply` at `crates/drive/tests/nonblocking.rs:241`.
Its recorded step times were `[3.339915, 136.263362, 2.810418, 0.27709, 526.075154]` ms.
The chunked and nonblocking test files are unchanged from `00e701b0`.
These failures prevent a clean workspace gate. Their cause is unverified without a baseline reproduction.
Do not interpret the local instruction-budget results as acceptance of the entire workspace.

The supplemental remainder run was stopped before GUI targets.
`crates/drive/tests/window_arenas.rs` launches Cage and a real browser window when Cage is installed.
Cage is installed on this machine. The task prohibits GUI execution.
The run was still in `task_budget` when its own process tree was stopped. That test has no accepted result.
No GUI target was launched by this continuation.
The opt-in real-compositor test in `crates/window/tests/live.rs` remains disabled because `BARE_WINDOW_LIVE` is unset.

Before that stop, `a_page_that_aborts_task_after_task_fails_the_step_with_the_reason` also failed at `crates/drive/tests/survive_limit.rs:59`.
It reached the 120000 ms step deadline after three script-budget aborts, before the expected fatal page stop.
The report records 133346.0 ms for the expect step and `time limit 120000 ms`.
That test file is unchanged from `00e701b0`. No runtime or test limits were changed.

The remaining-crate run found additional failures in unchanged test files:

- `bare-layout`: `keep::tests::a_kept_layout_equals_a_full_layout_after_random_changes` panics at `crates/layout/src/keep.rs:387` with `attempt to multiply with overflow`. The test computes `seed * 0x9E37_79B9_7F4A_7C15` for seeds through 12 in a debug build.
- `bare-mcp --test server`: `a_hidden_iframe_download_is_saved` sees a download still running with 0 of 8 bytes at line 2353.
- The same target reports `a_click_under_a_cookie_banner_fails_fast_and_names_the_banner` at line 1567 with 2377 ms for a 1000 ms covered-element wait.
- The same target reports `a_snapshot_of_5000_elements_takes_under_30_ms` at line 608 with a best snapshot time of 339 ms.

These assertions and the layout test seed expression were not edited. No unrelated test or engine fix was made.

`bare-net --test https_openssl` reports 15 passed and two failed.
`an_untrusted_or_wrong_name_peer_gets_no_request_and_no_body` fails at line 307 with `certificate verification failed: BadSignature`.
`a_redirect_from_a_trusted_server_to_an_untrusted_one_sends_nothing_there` fails at line 342 because the returned error does not contain `NoIssuer`.
The test file is unchanged from `00e701b0`. Certificate verification and expected errors were not changed.
These are negative synthetic-certificate fixtures. No untrusted peer was accepted to make them pass.

`cargo test -j3 --workspace --exclude bare --exclude bare-drive --no-fail-fast` completed with exit 101 and five failed targets: layout lib, MCP server, net HTTPS, TLS client and TLS 1.2 fixtures.
The TLS client fixture reports four failures at lines 108, 119, 157 and 232, all with `certificate verification failed: BadSignature`.
The TLS 1.2 fixture reports `certificate_checks_are_the_same_as_for_tls13` at line 133 with the same error.
Both test files are unchanged from `00e701b0`. No TLS implementation file was changed by monitoring.
The complete output is `/tmp/monitor-remaining-crates.log` on archMachine.
Other gate outputs are `/tmp/monitor-foundation-final.log`, `/tmp/monitor-drive-final.log`, `/tmp/monitor-mcp-final.log`, `/tmp/monitor-bare-integration.log`, `/tmp/monitor-workspace-retry.log` and `/tmp/monitor-workspace-check.log`.

Final status: partial acceptance. Monitoring-specific functional checks and the measured metrics/trace instruction budgets pass.
The workspace gate, GUI execution and non-Linux runtime are not accepted. The parent subsequently accepted the disabled-overhead budget on the local fixture.
HTTP/2 first-frame timing, pre-header failure outcomes and complete subsystem memory attribution remain limitations.

## Parent verification

The parent reran the five driver/MCP monitoring fixture tests successfully.
The parent reran all 25 OS, collector and process tests plus the thread-affinity doctest successfully.
The parent reran `cargo check -j3 --workspace` successfully.

On original baseline `00e701b089a9abe9a399bbe69150c5d772b5c4cc`, the parent ran:

```
cargo test -j3 -p bare --lib browser_session_tests::a_pending_meta_refresh_is_queued_work -- --exact
```

It failed at the same line 684 assertion. This failure predates the monitoring changes.
This reproduction does not establish the causes of other workspace failures.

The parent also ran this test on the original baseline:

```
cargo test -j3 -p bare-layout --lib keep::tests::a_kept_layout_equals_a_full_layout_after_random_changes -- --exact
```

It failed with the same multiplication overflow at `crates/layout/src/keep.rs:387`.
The meta-refresh and layout-overflow failures both predate this change. Other reported failures remain unclassified.


### Disabled-overhead comparison

The parent built original baseline `00e701b089a9abe9a399bbe69150c5d772b5c4cc` in the existing main checkout. No source changes were made.
Command: `CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=false cargo build -j3 --release -p bare-drive`.
These settings match the candidate profiling optimization and symbol settings.
The candidate binary contains code from `006ce7f7`; candidate HEAD `df2bbb87` adds documentation only.
The parent used the same loopback fixture as the enabled-mode measurements.
Baseline and candidate ran alternately, both off, three runs each, 20 journeys per process.
Machine: archMachine. No wall-time claim is made under concurrent load.

Baseline instructions: `[3238639776, 3238542109, 3238515880]`. Spread: 123896.
Candidate instructions: `[3238628071, 3238586943, 3238466294]`. Spread: 161777.
Mean instruction difference: -0.000169%. The disabled <=1% budget passes on this fixture.
This near-zero difference does not establish a speed improvement or performance on other inputs.
Baseline browser peak RSS: 16856, 16732, 16804 KiB. Candidate: 17128, 16920, 16980 KiB.
No capture was created in either mode.
Raw outputs, exact input, commit metadata, and binary SHA-256 hashes are in `/tmp/monitor-parent-baseline-20261116/results.json` and its sibling files.
The comparison script is `/tmp/monitor-parent-compare-20261116.py`.
All three provisional instruction budgets pass on the measured local fixture.
The full workspace gate remains unaccepted. GUI, non-Linux execution, other workloads and separate logger allocator usage remain untested.


## GUI verification, 2026-11-17

The parent ran one pass on archMachine at code head `943272e9`.
Offstage provided an isolated Cage screen. The screen was destroyed after the tests.
The MCP launch lacked `XDG_RUNTIME_DIR`; the CLI succeeded with `/run/user/1000`.

All three existing GUI tests passed without skips:

- `bare-window --test live`: `opens_draws_three_frames_and_closes`, with `BARE_WINDOW_LIVE=1`.
- `bare-drive --test window_wait`: `a_window_step_sees_a_reply_at_once`, through its inner compositor path.
- `bare-drive --test window_arenas`: `a_window_run_holds_the_window_and_little_more_than_a_headless_run`. This test runs 50 journeys per mode internally.

A local windowed journey with tracing also passed. It typed into an input, clicked a button, verified the changed DOM, and waited for a delayed fetch reply.
The first fixture attempt omitted the HTML Content-Type and failed. The corrected fixture passed; no browser source was changed.
The parent inspected the rendered window through OCR and a screenshot.
The corrected capture contains 383 records, including 150 process samples, and terminal status reports zero drops.
Network, HTML, JS, loader, engine, scheduler, style, layout and driver records are present.
This capture had no paint records despite visible rendering, because the window path bypassed the only paint span. GUI paint events (`paint/build` and `paint/draw`) were added afterwards.
The parent reran the same windowed journey in an isolated Cage screen on 2026-11-17, once. It passed. The capture has 406 records, 4 `paint/build`, 19 `paint/draw` and zero drops. Every paint record has `session_id`. Evidence: the session scratchpad `gui/` directory (temporary).
The test-only pass made no engine changes. Non-Linux runtime remains untested.

Evidence: `/tmp/monitor-gui-20261117/` contains the runner, journey, test logs, results, corrected JSONL capture and `trace-screen.png`.

## Workspace failure classification (2026-11-17)

The parent ran each unclassified failure on original baseline `00e701b0` and on candidate `9197b6e2`.
Machine: archMachine, idle at the start (load 0.68). Debug test profile. Three runs per tree, alternating base and candidate, `--test-threads=1`.
The baseline used a separate target directory, `/data/bare-tmp/base/00e701b089/target`.

Fail on both trees, 3 of 3 runs each, with the same panic line and message:

- `bare-drive --test chunked`: `a_big_document_parses_across_ticks_and_takes_input_during_the_parse` (line 147) and `a_chunked_parse_gives_the_order_and_the_dom_of_a_one_step_parse` (line 173). Both report `time limit`.
- `bare-drive --test survive_limit`: `a_page_that_aborts_task_after_task_fails_the_step_with_the_reason` (line 59). Base runs took 143.5, 129.7 and 163.9 s. Candidate runs took 150.2, 142.0 and 126.6 s.
- `bare-mcp --test server`: `a_snapshot_of_5000_elements_takes_under_30_ms` (line 608). Best snapshot time: base 138, 138, 136 ms; candidate 139, 136, 135 ms.
- `bare-net --test https_openssl`: `an_untrusted_or_wrong_name_peer_gets_no_request_and_no_body` (line 307, `BadSignature`) and `a_redirect_from_a_trusted_server_to_an_untrusted_one_sends_nothing_there` (line 342).
- `bare-tls --test client_openssl`: 7 passed, 4 failed on both trees. The failed tests are `a_root_that_did_not_sign_the_leaf_is_refused`, `a_root_that_the_peer_sends_is_not_trusted`, `an_empty_trust_set_trusts_nothing` and `an_rsa_chain_with_the_wrong_root_is_refused` (lines 108, 119, 157, 232).
- `bare-tls --test tls12_openssl`: 6 passed, 1 failed on both trees, at line 133.

Pass on both trees, 3 of 3 runs each:

- `bare-drive --test nonblocking`: `a_driver_click_step_returns_and_the_expect_waits_for_the_reply`.
- `bare-mcp --test server`: `a_hidden_iframe_download_is_saved` and `a_click_under_a_cookie_banner_fails_fast_and_names_the_banner`.

Hypothesis: these two groups failed earlier because the workspace run loaded the machine. This is not verified.

Result: every failure that remains also fails on the baseline. The monitoring branch introduces no failure in these targets.
With the meta-refresh and layout-overflow failures above, all known workspace failures predate this branch.
Causes are not classified. The TLS failures are negative tests: the peer is still refused, but the error differs from the expected one. Hypothesis: the synthetic certificate fixtures or the local OpenSSL version cause `BadSignature`.
Logs: the session scratchpad `classify/` directory (temporary).

## Windows runtime check (2026-11-17)

The parent cross-built candidate `ad970b02` with `RUSTFLAGS="-C target-feature=+crt-static" cargo xwin test --target x86_64-pc-windows-msvc --no-run`.
The tests ran once on the local Windows 11 VM (`/data/vms/winssh`), debug profile.
The driver and MCP tests start `env!("CARGO_BIN_EXE_...")`, a Linux path. On Windows that path resolves under `C:\`. The parent copied `bare-drive.exe` and `bare-mcp.exe` to that path.

All pass:

- `bare-os` lib: 6 passed. Linux runs 9; the Linux-only parser tests do not compile on Windows.
- `bare-observe` lib: 12 passed. `bare-observe --test process`: 4 passed.
- `bare-drive --test monitoring`: 4 passed, `--test-threads=1`.
- `bare-mcp --test monitoring`: 1 passed.
- `bare` lib `paint_trace_tests`: 2 passed.

A manual `bare-mcp.exe` capture with `BARE_OBSERVE=metrics` and a 5 ms interval wrote 23 records, 22 samples, zero drops and a `collector_status` record with reason `shutdown`.
Every sample field is `null`: RSS, peak RSS, allocator bytes, CPU times and thread count.
This matches the design: unsupported platforms return unavailable fields. Native Windows and macOS metrics are not built.
macOS runtime is untested. The Mac was offline (ssh timeout) during this check.

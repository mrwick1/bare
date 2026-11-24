# Engine monitoring implementation plan

Date: 2026-11-16.
Baseline: `00e701b089a9abe9a399bbe69150c5d772b5c4cc`.
Status: monitoring implementation extended; functional and performance acceptance remain partial.

## Goal

Explain what the browser does, where time goes, and which memory remains.
Support `bare`, `bare-drive`, and `bare-mcp`. Keep protocol stdout unchanged.
Monitoring must not change page scheduling, force collection, or force layout.

## Verified starting points

- `crates/net/src/trace.rs` stores request records and loader marks. `take()` clears marks. `dump()` prints them to stderr.
- `crates/bare/src/force.rs` records style and layout callers. Instruction readings can return zero when unavailable.
- `crates/js/src/eval_bc.rs` prints execution counters on interpreter drop with `BARE_JS_STATS`.
- `crates/js/src/eval_stack.rs` can print source excerpts with `BARE_TE_DUMP`.
- `crates/bare/src/main.rs` prints window statistics after the window run returns.
- `crates/drive/src/main.rs` samples RSS, peak RSS, and glibc heap after each journey repetition.
- `Browser::stats`, `Interp::stats`, and heap statistics expose existing counters.
- The release profile strips symbols. The workspace already uses `serde_json`.

## Design choice

Option A: extend independent stderr logs. This needs fewer initial edits but leaves incompatible formats and no shared lifecycle.
Option B: add a small shared collector and connect existing counters. This provides one schema, filters, bounded storage, and one clock.
Option C: adopt a general tracing framework. This adds dependencies and does not itself provide engine memory accounting.
Choose B. Reuse `serde_json`. Add no external service or dashboard in this change.

## Controls and record contract

Proposed controls are new; they do not exist on the baseline.

- `BARE_OBSERVE=off|metrics|trace`, default `off`.
- `BARE_OBSERVE_FILE=PATH`, required when enabled. Open a new file; do not overwrite an existing capture.
- `BARE_OBSERVE_INTERVAL_MS`, default 1000, positive and bounded.
- `BARE_OBSERVE_CATEGORIES`, optional comma-separated category filter.
- `BARE_OBSERVE_LEVEL=error|warn|info|debug|trace`, default `info`.
- `BARE_OBSERVE_MAX_BYTES`, default 64 MiB. Stop recording at the cap and expose the reason.

Preserve legacy switches and their current output. Do not silently change presence-based semantics.
A legacy switch alone does not enable a file collector. Document that distinction.
Use versioned JSONL with a monotonic elapsed timestamp, PID, thread identifier, level, category, event name, and typed fields.
Include session, document, request, and step identifiers when known. Never invent attribution for a process-wide sample.
Use explicit start/end or duration records for work. Include units in field names.
Unknown measurements are null or absent, never a fabricated zero.

## Phase 1: independent foundation tasks

### A. Collector: GPT-sol agent, isolated worktree

Own `crates/observe/**` and the required `Cargo.lock` change.
Create `bare-observe` with configuration parsing, category/level filters, a bounded nonblocking queue, and a file writer.
Keep the disabled path free of formatting, allocation, clock reads, and worker threads.
Use lazy event field construction. Limit individual record size as well as queue length.
Expose dropped record counts and writer failure state. Flush periodically and on explicit shutdown.
Ensure each emitted line is complete JSON. Reserve room for a terminal status when enforcing the byte cap.
Use restrictive file permissions on Unix. Do not accept raw page source, bodies, cookies, headers, or typed values as default fields.
Provide an explicit collector owner and cloneable producer handles. Integration will own initialization and shutdown.
Test filtering, escaping, malformed configuration, overflow, byte caps, writer failure, shutdown, and disabled laziness.
Gate: `cargo test -p bare-observe` and `cargo check -p bare-observe`.

### B. Process measurements: GPT-sol agent, isolated worktree

Own `crates/os/src/metrics.rs` and its module export in `crates/os/src/lib.rs`.
Expose a current-process snapshot with optional RSS bytes, peak RSS bytes, CPU microseconds, thread count, and allocator live bytes.
Use Linux process information and glibc accounting where supported. Keep OS calls in `bare-os`.
Keep allocator bytes distinct from JS accounted bytes and resident memory.
Unsupported platforms must compile and return explicit unavailable fields. Native non-Linux metrics are a follow-up.
Do not add a sampling thread here. The integration owns its lifecycle.
Test parser fixtures, missing fields, unit conversions, malformed values, and a current-process smoke check.
Gate: `cargo test -p bare-os` and `cargo check -p bare-os`.

## Phase 2: integration after A and B

Review both diffs and rerun their gates before integration. Resolve the public API once.
Delegate implementation to GPT-sol from the integrated local base.
Initialize one collector per process in the three entry points. Stop it cleanly.
Sample OS metrics on a dedicated interruptible thread, independent of page progress.
Collect engine counters only at safe points on the owning thread. Mark their timestamps and scope.
Connect network records and loader marks without clearing observations needed by another consumer.
Add correlated durations for HTML parse, JS parse and execution, scheduler tasks, collection, style, layout, and paint.
Include queue depths, readiness milestones, and stop reasons where existing state provides them.
Reuse existing counters before adding accounting. Do not claim complete subsystem byte totals when capacity is unmeasured.
Do not send page strings to the collector. Use stable numeric IDs and fixed event names.
Gate: targeted crate tests, JSONL fixture journeys, and `cargo test --workspace`.

## Phase 3: acceptance and documentation

Use local fixtures and recorded public journeys. Do not collect authenticated live traffic.
Verify disabled mode creates no capture or monitor thread.
Verify metrics continue while the page thread executes a long script.
Verify repeated journeys expose both process memory and engine counters without retention from the logger.
Verify concurrent sessions and network workers retain correct IDs.
Verify a slow or failed writer does not block the page thread. Exercise partial writes and disk-full errors with an injected writer.
Verify seeded secrets in query strings, headers, source, console text, and inputs do not enter default captures.
Verify MCP stdout remains valid protocol data.
Document exact switches, file schema, supported metrics, legacy behavior, and known coverage gaps in repository docs.
Add a profiling build profile with symbols without changing the normal release profile.

Measure off, metrics, and trace on the same frozen commit. Run each case at least three times.
Record machine, inputs, instruction counts, peak RSS, run count, and spread. Report wall time only on an idle machine.
Provisional budgets: disabled instructions within 1%; metrics within 3%; trace within 10% on the selected suite.
These are acceptance targets, not measured claims. Investigate failures before calling the feature complete.
Record logger memory separately from engine memory. Record unsupported counters and dropped events.

## Boundaries and handoff

No UI work, server, deployment, merge, or new external package beyond the already used JSON library.
No instruction-by-instruction JS trace or complete allocation stack profiler in this first implementation.
No arbitrary memory reduction changes. Do not enable stress collection during baseline measurements.
Children commit on their prepared branches. They do not push, create PRs, merge, or delegate further.
The parent reads diffs and reruns checks. Integration starts only after both foundation tasks settle.

## Validation status

Source audit completed. No runtime tests or performance measurements have run for this plan.

## Foundation result: process metrics

Commit: `06c0ddb09f277483af745494bbb7d804a34af4da`.
The parent reviewed both changed files and reran `cargo test -p bare-os` (9 passed), `cargo check -p bare-os`, and the diff whitespace check.
The implementation provides `bare_os::metrics::snapshot() -> Snapshot` with six optional counters.
No integration has occurred. Non-Linux execution and monitoring overhead remain untested.
The collector task remains in progress. Integration waits for its result.

## Foundation result: collector

Commit: `dece49dfd5becf37aeb2a1487bedcad1d73c9e8c`.
The parent reviewed the collector, tests, manifest, lock entry, and README.
The parent reran `cargo test -p bare-observe` (12 passed) and `cargo check -p bare-observe`.
The integration agent must combine both foundation commits on its prepared branch and copy this plan into that branch.
The collector uses scalar fields only. Integration must choose metrics versus trace events; the transport does not choose them.
A failed partial write can leave an incomplete final line. Readers must discard it. Shutdown waits for sink I/O to return.

## Phase 2 integration result (partial)

The integration branch is `t3/integrate-engine-monitoring`.
The scoped files matched baseline before integration.
Both foundation commits applied without conflicts.
The foundation gate passed: 9 bare-os tests and 12 collector tests.
The process facade adds an independent interruptible sampler and scoped numeric contexts.
The three process entry points own initialization and shutdown.
Narrow engine hooks and a profiling profile are present.
See `docs/monitoring.md` for current coverage and limitations.

Implementation stopped at the required repeated-verification limit.
The journey fixture first failed on an unquoted type argument.
After that scoped fix, the journey ran but failed its style-event assertion.
The current fixture remains failing. Phase 2 is not complete.
The MCP protocol fixture and process-facade fixture passed.
Adding the collector dependency exposed three ambiguous empty-vector assertions in JS tests.
Those assertions now specify their existing u16 element type.
Workspace verification and phase 3 acceptance must be completed by the parent.
No overhead measurement or authenticated live traffic ran.

Final checks on the partial integration:

- `cargo test -p bare-os -p bare-observe`: exit 0, including two process-facade integration tests.
- `cargo test -p bare-mcp --test monitoring`: exit 0.
- `cargo test -p bare-drive --test monitoring`: exit 101; missing style events after the step-syntax fix.
- `cargo test --workspace`: first attempt failed to compile the three ambiguous JS assertions. The retry was canceled during compilation after the verification stop.
- `cargo check --workspace`: exit 0. Existing dead-code warnings remain unchanged.
- `git diff --check`: exit 0.

The worktree has no uncommitted implementation changes.

## Completion work on 2026-11-16

This section supersedes the partial integration stop above.
The dependency range through `87859cb9` applied without scoped drift.
The continuation adds thread-affine context guards, transactional startup ownership, duplicate-start protection and sampler termination after capture stop.
Filtered spans now check category and level before they read the clock.
Network bodies retain numeric context and record delivered wire bytes, status, EOF/error/abandonment and trace timing without buffering page data.
HTTP/1 first-byte wait and HTTP/2 complete-header timing have distinct semantics.
Scheduler tasks have individual spans. Pump results expose existing queue counts and cap state.
Driver readiness exposes fixed boolean state.
Browser counters now sample after scripts, drains and explicit GC, including existing heap and capacity counters.

Local fixtures exercise real scripts, concurrent sessions, headless paint, GC, seeded response headers, suite-worker output paths and error flushing.
`tools/monitoring/accept.py` provides a repeatable three-mode local measurement.
Final gate results and performance limitations are recorded in `docs/monitoring.md`.
HTTP/2 first-frame timing remains a follow-up. Complete subsystem memory accounting remains out of scope.

## Parent acceptance result

The parent reviewed the final instrumentation and reran monitoring fixtures, foundation tests, and workspace compilation. All passed.
The parent reproduced the meta-refresh and layout-overflow failures on original baseline `00e701b0`. Other suite failures remain unclassified.
The parent measured disabled overhead against the original baseline. All three instruction budgets pass on the single local fixture.
See `docs/monitoring.md` for exact commands, counts, artifacts and limitations.
Implementation is complete for the documented coverage. Full workspace acceptance, GUI and non-Linux runtime validation remain open.

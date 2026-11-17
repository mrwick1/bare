# Browser monitoring handover

Updated: 2026-11-17. Work stopped at the owner's request.

## Resume instructions

Do not resume implementation until Arjun asks. He requested documentation and a handover prompt, then a stop.
The last agreed sequence was to address the remaining items one at a time, starting with missing GUI paint events.
The investigation after that agreement was read-only. No paint fix or regression test was written.
No agents, builds, test servers or test windows from this task remain running.

## Repository and branch

- Machine: archMachine.
- Main checkout: `/home/mrwick/coding/personal/projects/bare` (physical path `/data/coding/personal/projects/bare`).
- Active implementation worktree: `/home/mrwick/.t3/worktrees/bare/t3-8c2efbd2-c14ee879afda9867`.
- Branch: `t3/complete-browser-monitoring`.
- Implementation and GUI-report head before this handover: `6ae0c87bb0cca40e44c94728246a261e47a4a65d`.
- Original baseline: `00e701b089a9abe9a399bbe69150c5d772b5c4cc`.
- No push, PR or merge occurred. Use the active worktree, not main or an older child worktree.
- Main has the task's untracked `docs/design-engine-monitoring.md` with a final handoff pointer. Do not mistake it for the authoritative implemented plan.
- Read `docs/design-engine-monitoring.md` and `docs/monitoring.md` in the active worktree. They contain the design, exact test commands, measurements and historical limitations.

## Implemented

`bare-observe` provides opt-in bounded JSONL, category/level filters, numeric context IDs, scalar fields, lazy disabled emission, file caps, drop counts and writer status.
`bare-os::metrics::snapshot()` provides optional current-process RSS, peak RSS, allocator bytes, user/system CPU time and thread count.
`bare`, `bare-drive` and `bare-mcp` own initialization and shutdown. An independent thread samples process counters.
Engine hooks cover parsing, JS, GC, style, layout, scheduler tasks, network phases, loader milestones and existing memory counters.
Context crosses network-worker and window handoffs. Process samples have no page attribution.
Default fields exclude page text, headers, URLs, bodies, console strings and typed values. Legacy switches retain their behavior.
The profiling build retains symbols. `tools/monitoring/accept.py` measures a local fixture.

Use `BARE_OBSERVE=metrics|trace`, `BARE_OBSERVE_FILE=NEW_PATH`, and optionally `BARE_OBSERVE_INTERVAL_MS`.
Off is the default. Existing files are not overwritten. Suite worker files append `.pid-PID`.
See `docs/monitoring.md` for every control, field scope and timing definition.

## Verified

The parent reviewed the implementation and independently reran:

- OS, collector and process tests: 25 tests plus the thread-affinity compile-fail doctest passed.
- Driver and MCP monitoring fixtures: five tests passed.
- `cargo check -j3 --workspace`: passed.
- JSONL artifact parsing and terminal zero-drop checks: passed.

Three runs per mode, 20 loopback journeys per process, passed the provisional instruction budgets on one fixture.
Metrics overhead was 0.354%; trace overhead was 0.611%, relative to candidate off.
The parent built the original baseline with equivalent optimization and symbols. Disabled instructions differed by -0.000169%; this passes the 1% budget but does not establish a speed improvement.
No wall-time or broader-workload performance claim is accepted.

GUI tests ran once on 2026-11-17 in isolated Cage screens:

- `bare-window --test live`: opens, draws three frames and closes; passed without a skip.
- `bare-drive --test window_wait`: window reply wakeup; passed.
- `bare-drive --test window_arenas`: repeated-window memory bound; passed. The test internally runs 50 journeys per mode.
- A windowed tracing journey typed, clicked, checked the changed DOM and awaited a delayed fetch; passed after correcting the fixture's missing HTML Content-Type.
- The corrected capture has 383 records, 150 process samples and zero drops. It has no paint events despite visible rendering.

Full workspace acceptance remains open. Two failures were reproduced on the original baseline: meta-refresh scheduling assertion at `browser_session_tests.rs:684`, and random-layout seed overflow at `layout/src/keep.rs:387`.
Other reported failures involve timing/deadlines, MCP fixtures and synthetic TLS certificates. Their sources are unchanged, but their causes remain unclassified. Do not call them baseline failures without reproducing them.
Non-Linux runtime remains untested. OS fallback compilation was checked by the foundation agent for macOS and Windows; this is not runtime validation.

## Done: GUI paint trace (2026-11-17)

Fixed. `DisplayList::make` writes `paint/build`. `Painter::draw_view` writes `paint/draw`. Test: `crates/bare/src/paint_trace_tests.rs`. The windowed journey now has paint records. See `docs/monitoring.md`.

## Former first item: GUI paint trace

Verified code path:

- `crates/paint/src/lib.rs`: `Painter::paint` has the only `paint/paint` span.
- `Painter::build`, `build_band`, `draw` and `draw_view` currently lack spans.
- `crates/bare/src/page.rs`: `Page::paint_at` calls `painter.build_band` and `painter.draw_view` directly. It bypasses `Painter::paint`.

Hypothesis: the bypass explains the absent GUI paint events. This has not yet been proved with a failing automated regression test.
Write that test first. Cover normal redraw and retained-display-list redraw. Preserve numeric context and disabled-mode laziness.
Choose event boundaries that distinguish display-list construction from raster work and avoid double-counting the headless `Painter::paint` path.
Then make the smallest hook change and rerun the relevant tests plus the windowed tracing journey.
Do not change rendering behavior to fix instrumentation.

## Remaining sequence

1. Fix and verify GUI paint trace coverage.
2. Classify the other workspace failures against the baseline. Do not widen into unrelated engine fixes without agreement.
3. Test non-Linux execution when requested and an appropriate environment is available.
4. Optional coverage: HTTP/2 first-byte/frame timing, failure outcomes before response headers, and complete subsystem/logger memory attribution.
5. Prepare a PR after the agreed acceptance work. Read the PR-writing and identity instructions first. Do not merge without authorization.

Arjun first requested GPT-sol models. On 2026-11-17 he changed this to Claude models for delegation. Previous agents used `gpt-6.1-sol` with high reasoning through T3 `delegate_task` in isolated worktrees.
No review agents are needed for small findings. Read the shared routing and delegation instructions before new delegation.

## Evidence and operational notes

- GUI evidence: `/tmp/monitor-gui-20261117/`. Includes runners, steps, test logs, `results.json`, `trace-corrected.jsonl`, and `trace-screen.png`.
- Enabled-mode measurements: `/tmp/bare-monitor-accept-process-rss/`.
- Disabled comparison: `/tmp/monitor-parent-baseline-20261116/`; script `/tmp/monitor-parent-compare-20261116.py`.
- Other test logs and exact failures are listed in `docs/monitoring.md`. Temporary artifacts can expire.
- Offstage MCP lacked `XDG_RUNTIME_DIR`. CLI worked with `XDG_RUNTIME_DIR=/run/user/1000`. Cage was already installed; no setup was required.
- Test screen `hc-bare-monitor-gui` was destroyed. CLI first reported `not_owner` while listing `mine=true`; after verifying ownership, `destroy --force` succeeded. Final list showed no screen.
- Two offstage reports were queued locally because no beta key was present. No credential setup or external report submission was performed.

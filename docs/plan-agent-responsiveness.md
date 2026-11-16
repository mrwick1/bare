# Responsive independent sessions and cancellation

Plan destination in the child PR: `docs/plan-agent-responsiveness.md`.

## Why
The current server reads requests on another thread but Server::handle synchronously runs them. A long tool call pauses Server::tick_live for all pages; run_suite explicitly documents this. Agents must be able to inspect or close another session and cancel a request while one page is slow.

## Current state and owned files
crates/mcp/src/main.rs: main reader/dispatch/writer loop, Server.live, Live lifecycle, run_suite and session routing. New private modules under crates/mcp/src/ for dispatch/worker ownership as needed; do not rewrite unrelated tool implementations. crates/drive/src/run.rs: narrowly scoped cancellation plumbing and bounded wait loops. crates/drive/src/suite.rs: cancellation/owned worker cleanup only if required. crates/drive/src/snap.rs: settle cancellation points only, no ref/diff changes. crates/bare/src/page.rs or control.rs: narrow existing wait-loop cancellation hook only if driver hooks cannot suffice; explain why. Add crates/mcp/tests/responsiveness.rs and, if needed, crates/drive/tests/cancel.rs. Own docs/plan-agent-responsiveness.md. No README edits, failure-policy redesign, guide/evaluation code or unsafe cross-thread browser state.

## Chosen direction and constraints
Compare per-session owner threads with cooperative cancellation, subprocess actors, and cooperative continuations. Record CPU/memory/latency costs and limits. Implement owner threads with bounded per-session work queues and a responsive transport coordinator unless actual Send constraints or input APIs prove this unsafe. Construct Session/Page inside its owner thread; move only owned JSON/command data between threads. No mutex may be held across a blocking browser call. Reuse existing network-client sharing only through supported safe APIs; preserve shared cache and each session's own cookies/storage. Do not use one thread per tool call on the same Session.

The coordinator accepts requests while work runs, routes by session, writes each complete JSON-RPC reply atomically, and preserves request IDs. Independent sessions can complete out of order. Mutating calls for one session must remain ordered; document how cancellation and close interact with queued calls. Keep initialize/ping/tools/list responsive. Snapshot/act/step ordering for one session must not race. Lifecycle operations must not drop a live session from a map before its owner and captures are accounted for. Suite workers stay isolated from live sessions.

Implement MCP notifications/cancelled handling from the current official protocol (verify the primary spec before coding). A cancellation can arrive without an id and must reach the active request. Check a request-local atomic cancellation token between driver attempts, waits and cooperative engine ticks. Do not claim immediate interruption of a native operation or unbounded JS execution; report that limitation. Do not use unsafe thread termination. Ensure cancellation does not turn unknown dispatched input into 'not_started' or automatically rerun it. Cancel suite work by terminating and reaping only its owned worker processes, with partial result/capture handling documented. Reader EOF and server exit must close owned channels, stop/reap workers and avoid leaked processes. No new daemon or listening service.

## Implementation sequence and checks
A. Save the algorithm/ownership design. Write a regression that starts a slow request in session A without waiting for its reply, then pings and interacts with session B; prove on the old code that responses block. Use readiness barriers/fixture events, not a narrow wall-time performance threshold. Gate cargo test -p bare-mcp --test responsiveness fails for old blocking behavior.
B. Add coordinator/session ownership without changing action semantics. Verify separate-session progress, same-session FIFO, shared-cache behavior and cookie/storage isolation. Keep request responses exactly once.
C. Add cancellation delivery and cooperative checks. Test cancellation before execution, during a condition wait, cancellation of an unknown/finished request, close while queued, suite cancellation and EOF cleanup. Assert owned child processes end and session B still works. Send cancellation through actual stdio notifications, not a direct internal helper only.
D. Run cargo test -p bare-mcp --test responsiveness, cargo test -p bare-drive --test cancel if added, cargo test -p bare-mcp, cargo check --workspace and git diff --check. All new tests must pass; report full-suite failures precisely. Compare responsiveness and idle CPU/RSS on frozen before/after builds for at least three runs on the same fixtures; do not infer wall-time gains under concurrent machine load.
E. Open one PR. Document protocol behavior, cancellation limits, resource ownership, shutdown and integration hunks in main.rs/run.rs. Do not silently replace the architecture with process-per-call if blocked: report the evidence.

## Done criteria
A slow call cannot block an unrelated live session or the transport control path. Calls on the same session are ordered. Cancellation is observed at documented cooperative points. Shutdown reaps only owned workers. No unsafe ownership bypasses, dropped replies, orphan workers, lost session state or false retry guarantees. All new gates pass.

## Shared execution rules

The user explicitly requests five implementation agents and five PRs. You own one PR. Do this task yourself; do not create more agents. Use only the isolated checkout supplied by T3. Do not create or switch checkouts. Independent base: the tool-prepared default branch. Record the starting SHA. Main was inspected at 00e701b089a9abe9a399bbe69150c5d772b5c4cc. Compare the named symbols if the base has moved. Stop only on a material contract mismatch; normal line shifts are not blockers.

Read repository CLAUDE.md and applicable AGENTS.md, ~/.agents/providers/claude.md, and ~/.agents/docs/pr-writing.md. Read ~/.agents/docs/git-identity.md if identity mechanics fail. Design decisions and planning below were made by the parent. First save this supplied plan to the stated docs path. Record any implementation deviation and evidence in that document. Do not delegate planning. Compare existing implementations and stdlib before adding code. No new dependencies, broad refactors or unrelated fixes. Preserve redaction, accessibility, shared network cache behavior and the user input path. No unsafe Send/Sync implementations. Never fix an engine gap by bypassing user input with eval.

PR #614 (https://github.com/mrwick1/bare/pull/614, head 02b7f5622ec49a8f3049284e5a5abb3aa4cd6b17) adds act action_status/page_status/verification and optional expect. It was OPEN when assigned. Do not merge it, cherry-pick it, or duplicate its implementation. Your independent PR must work on its prepared base. If it is already present, preserve its fields and assertions. Note integration hunks in your PR so later integration preserves both features. The parent will handle cross-PR integration; do not merge sibling branches. Suggested integration order: #614, observations, recovery, responsiveness, guidance, evaluation. Each branch must pass its own checks before opening its PR.

Use a separate CARGO_TARGET_DIR for this checkout, for example "$PWD/target"; never share the primary checkout's target or another task's target. Shared artifacts previously produced an invalid baseline comparison. Limit build concurrency with CARGO_BUILD_JOBS=2. Define checks before production edits. Add and run a failing regression first for bugs; run the same check after the fix. Capture exact commands, exit statuses and failed names. Do not weaken timing tests or declare timing failures harmless. Known baseline evidence: cargo test -p bare-mcp on clean 00e701b0 had 93 integration passes, 2 failures (a_hidden_iframe_download_is_saved; a_snapshot_of_5000_elements_takes_under_30_ms), 1 ignored window test. These failures require comparison, not assumptions. cargo test --workspace previously ended with exit 143 during compilation, so workspace success is unverified. Do not run repeated full workspace builds across all five agents; run targeted gates, one relevant broader suite, and cargo check --workspace with limited jobs when production Rust changes. Report every incomplete check.

These verified repository commands exist: cargo test -p bare-mcp; cargo test -p bare-drive; cargo check --workspace; git diff --check; python3 -m unittest discover. Inspect existing Cargo.toml and CLI help before using new flags. Commands listed for NEW tests are acceptance targets to create, not claims they exist on main. Match current commit style, for example 'Expose MCP action outcomes and optional postconditions'. Keep changes in scope. Read the final diff before committing. Before pushing run gh auth switch --user mrwick1. The user authorizes committing, pushing and opening a PR; this overrides the generic brief template's 'do not push/open PR'. Open one PR against main. Link it with link_pull_request as the next call after creation. Do not merge or watch the PR. Do not start a server except a temporary loopback fixture owned and shut down by your test. Do not use real credentials, live writes, external accounts or a paid model API. No GUI is required. If GUI verification becomes necessary, read the ui and offstage skills first and follow the active harness rules.

STOP and report if a required change is outside the listed source scope, needs unsafe cross-thread browser state, exposes credentials, removes existing validation, or requires an external service. A negative finding is a valid result; do not fabricate completion or open an empty PR. Fix only what is described. List other findings instead of changing them. If blocked on a required verify gate twice after a reasonable correction, report exact evidence and leave a reviewable committed draft if useful.

Final report: at most 400 words. State COMPLETE or STOPPED; list changes, exact checks/results, untested behavior, base/head SHAs and any integration overlaps. End with: PR <full URL>, head <SHA>, checks and reviews <state>. If no PR, say why. PRs must describe final behavior and limitations, with no attribution footer.

---

# Implementation record

Starting SHA: `00e701b089a9abe9a399bbe69150c5d772b5c4cc` (main as inspected; no symbol drifted).

## Options compared

| Option | CPU / memory / latency | Limit |
| --- | --- | --- |
| Owner thread per session, cooperative cancel (chosen) | One parked thread per session: idle CPU 0, about 0.3 to 0.5 MiB RSS per thread in debug builds. A reply of one session never waits for another. | Cancellation is cooperative: a native call that does not return is not interrupted. |
| Subprocess per session | A full process per page: tens of MiB, a new network client (no shared cache), a pipe protocol for every tool. Kill gives hard cancellation. | Breaks the shared reply cache and connection reuse that `open` relies on. Large rewrite of 20 tools. |
| Cooperative continuations on one thread | No new thread. | A page and the engine have no yield point inside a script or a native wait, so one slow call still stalls all pages. Needs a rewrite of `Session::step`. |

Chosen: owner threads. `Page` is not `Send`, so each `Session` is built on its owner thread and never moves. Only JSON, `StorageState`, `Options`, `Redactor` and the network client (it is `Send`; it is forked under a short lock) cross threads. No `unsafe` and no `Send`/`Sync` impl was added.

## Ownership

- Main thread: reads parsed lines, routes them. It answers `initialize`, `ping`, `tools/list`, unknown methods and `notifications/cancelled` itself. It never runs a page.
- Writer thread: owns stdout. Each reply is one `writeln!` plus flush, so lines never interleave.
- Reader thread: reads stdin, sends `Eof` at the end.
- Session worker (`worker`): one per live session name. It owns the `Server` with that one `Live`. It runs its bounded queue (64 jobs) in arrival order, and ticks its page between jobs as before (#471).
- One-shot thread per `run_steps`, `run_suite`, `sessions` call (at most 16 at a time). It has an empty `live`, so a suite or a `run_steps` never touches a live session.
- Tab sessions (`BASE~N`) are workers too. The opener reserves the name under the map lock, the new worker builds its own `Session` and loads the page, and the opener waits for the first result.

Locks: `sessions` (name to queue sender), `inflight` (request id to token), `saved`, `net`, `threads`. Each guards a short map access or `Client::fork`. None is held across a browser call, a `recv` or a `join`.

## Ordering and lifecycle

- Calls on one session are one FIFO queue, so snapshot, act and step never race. Calls on different sessions finish in any order. Every reply keeps its request id.
- `open` and `ensure_session` create the worker if none exists. Any other tool for an unknown session gets the old error `no live session NAME: call open first` at once.
- A queue sender is used only under the `sessions` lock, and a worker removes its slot under the same lock (`retire`). So no job is lost between the two. The jobs that wait behind a worker that retires are routed again in order: an `open` after a `close` starts a new worker, other calls get the no-session error.
- `close` runs on the worker after every earlier call. The slot leaves the map only after `Session::finish` returned, so the session and its captures are accounted for first.
- A worker whose session is gone (`close`, failed first `open`, failed tab load) ends. A full queue gives an error reply at once.
- A panic in a call becomes an `isError` reply; the thread goes on.
- `sessions` asks an idle session for its late tabs and dialogs (internal tool `sessions_probe`, not in the tool list). A busy session is not asked: it shows the address of its last finished call. So `sessions` never waits for a slow call.

## Cancellation (MCP `notifications/cancelled`)

Spec checked: https://modelcontextprotocol.io/specification/latest/basic/utilities/cancellation (2026-07-28 revision). `params.requestId` names the request. On stdio the client MUST send the notification. A server SHOULD stop the work, free resources and not send a response; it MAY ignore an unknown or finished request. The notification itself has no JSON-RPC `id`, so it is read on the main thread while a call runs.

- Each tool call has an atomic token in `inflight`. The notification sets the token of its `requestId` (matched as JSON text, so `1` and `"1"` differ). Unknown ids, finished requests and malformed notifications are ignored.
- A call cancelled before it runs is skipped and gets no reply. A call cancelled while it runs gets no reply: the worker drops the result.
- The token is set on the owner thread for the time of a job (`bare_drive::run::set_cancel`). The driver reads it (`cancelled()`) at these points only:
  1. the top of every try of `Session::step` (between tries, never inside one);
  2. the sleep between tries (20 ms slices; a windowed page wakes every 100 ms);
  3. the promise wait and the navigation follow of `Session::eval`;
  4. the loop of `snap::settle`;
  5. between step lines in `run_live`, the download wait of `act`, and after the action of `act` (no settle, snapshot or download wait follows);
  6. a suite: every 25 ms in `exchange`, and before each test starts.
- Not interrupted: a blocking network read, `Page::wait_load` (up to 10 s), a script that does not end, layout or paint in progress, a `hyprctl` call. The call ends when that native work returns. There is no thread termination. Nothing promises immediate interruption.
- Input that the driver already sent to the page is not undone and not run again. A cancelled step is never reported as `not_started` (no reply is sent at all), and nothing retries it. The page state after a cancel is the real page state: the next call sees it.
- A cancelled `run_suite` kills and reaps only the worker processes of that suite (`Child::kill` then `wait` in `retire`), gives `Err("cancelled")` and no partial report; the capture folder is removed (as for a passed suite). A cancelled windowed session is only checked at the 100 ms wake.

## Shutdown

At end of input (or a failed stdout write) the main thread sets every token, empties the `sessions` map (closing every queue, so each worker sees `Disconnected` and drops its page), and waits up to 10 s for the threads. A suite sees its token within 25 ms, kills and reaps its workers. Then the writer is flushed. A thread stuck in a native call that does not return is left to the end of the process; no new daemon or listener exists.

## Regression gate (step A)

`cargo test -p bare-mcp --test responsiveness` on the old code (`00e701b0` plus the new test file): 3 passed, 6 failed. The failures were `no reply for id N: the server is blocked` (ping, `tools/list` and session b did not answer while session a waited for the slow page), the suite cancel test, and the end-of-input test (the server did not exit while a suite ran). `calls_of_one_session...` and the cookie and unknown-id tests pass on the old code by design: they guard behavior that must not change. A first run had 7 failures because the test passed `timeout_ms` to `act`, which the schema refuses (test bug, fixed before the gate result above).

## Results

See the pull request for the exact commands and numbers. Frozen builds for the resource comparison: base `00e701b0` built from `git archive` in a scratch folder with its own target; new = this branch. Same debug profile, same loopback fixture, 3 runs each, machine not idle (load average 4), so only idle CPU and memory are compared, not wall time.

| Sessions | Build | Idle CPU in 5 s | RSS (KiB), 3 runs | Threads |
| --- | --- | --- | --- | --- |
| 1 | old | 0.00 s | 35440, 35728, 35472 | 2 |
| 1 | new | 0.00 s | 36192, 35996, 35776 | 4 |
| 3 | old | 0.00 s | 37216, 37300, 37420 | 2 |
| 3 | new | 0.00 s | 38612, 38492, 38988 | 6 |

Cost: one more fixed thread (the writer) and one thread per session, about 0.3 to 0.5 MiB each in a debug build. Idle CPU is unchanged. A `ping` is answered in 0.15 to 0.2 ms in both.

## Deviations and limits

- Cancellation replies: none are sent for a cancelled request (the spec's SHOULD). A client that cancels and waits for a reply will wait; per the spec it must ignore any reply.
- Duplicate request ids in flight are not supported (the id is the key); JSON-RPC requires unique ids.
- `crates/bare` was not changed: the existing driver hooks were enough, so no hook was added in `page.rs` or `control.rs`.
- No `crates/drive/tests/cancel.rs`: suite cancellation is covered through stdio in `crates/mcp/tests/responsiveness.rs` (a worker script that sleeps stands in for `bare-drive`).

## Integration notes for the parent

- `crates/mcp/src/main.rs`: the `Server` struct, `main` and `handle` are replaced (`Shared`, `worker`, `Job`). The tool methods keep their bodies; the changes inside them are `self.saved`, `self.net`, `self.runs`, `self.workspace` going through `self.shared`, `sessions`, `open_tab`, and one `cancelled()` check after the step in `act`. PR #614 changes `act` (action and page status, `expect`): keep both. Its edits sit in the body of `act` after `run_live`, next to the check added here.
- `crates/drive/src/run.rs`: `set_cancel`, `cancelled`, `current_cancel` (thread local), the check at the top of the try loop in `Session::step`, `Host::sleep`, two `eval` loops, and no failure capture for kind `cancelled`. Recovery or observation work in `Session::step` must keep the `cancelled()` check before `tries += 1`.
- `crates/drive/src/suite.rs`: `Config::cancel`, `exchange(w, name, cfg)`.

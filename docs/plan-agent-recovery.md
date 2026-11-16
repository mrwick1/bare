# Structured failures and recovery advice

Plan destination in the child PR: `docs/plan-agent-recovery.md`.

## Why
The driver already produces typed failure kinds, but MCP consumers often get only prose. Agents need stable failure codes, evidence and an appropriate next action without assuming that a failed tool call means input never ran. Preserve action-outcome fields from #614 if present.

## Current state and owned files
crates/drive/src/run.rs: Fail {kind,message}, Try and Report.failure; one/found_in already distinguish not_found, duplicate, hidden, disabled, covered, detached and stale_ref. Step reports hold driver evidence. crates/mcp/src/main.rs: tools/call response construction, run_live and act failure propagation only. Add crates/mcp/src/recovery.rs for the small mapping if useful, and crates/mcp/tests/recovery.rs. Own docs/plan-agent-recovery.md. No observation/ref algorithms, session scheduling, README edits, guide/evaluation files, or changes to the existing server.rs tests.

## Contract and design
Use driver Fail.kind directly. Never extract machine codes by parsing 'FAILED ...' or English error text. Preserve existing kinds as stable codes; distinguish invalid_arguments, missing_session and unknown_tool only where their source is explicit. Keep a documented fallback for unclassified internal errors. Do not mislabel every page failure as unsupported_browser_feature; report that only when actual engine evidence supports it. A generic page/load error remains generic.

Return an additive structured failure object for execution errors: code, redacted message/evidence, and next_action from a small documented set such as inspect_page, refresh_observation, refine_target, dismiss_overlay, wait_for_condition, diagnose or correct_arguments. Include whether a check may be retried only if the driver provides that fact. Recovery advice must never automatically repeat a click, type, submission, upload or navigation. An ambiguous dispatched operation stays unknown. A stale reference asks for a new observation; duplicate matches ask for a narrower target; covered controls identify the covering element when the driver supplies it. Do not add a global automatic retry layer.

Carry typed failures from the driver through MCP response construction with minimal interface changes. Compare a typed per-call result with a mutable 'last error' slot; choose the typed result to prevent a later call receiving an earlier call's failure. If #614 is absent, add the recovery object independently without duplicating its execution/verification machinery. If it is present, extend the act output schema and preserve existing status fields on every error path. Document integration hunks for its wrapper.

## Implementation sequence and checks
A. Define the exact code-to-advice table and schema in the plan. Write failing protocol tests for duplicate targets, stale ref, persistent overlay, hidden/disabled target, bad arguments and missing session. Test a failed call followed by a valid call to catch leaked failure state. New gate: cargo test -p bare-mcp --test recovery must fail for missing structure before production edits.
B. Propagate typed failure data; add the mapper and schema. Redact every dynamic evidence value using the current session Redactor and safe_url. Preserve text receipts, JSON-RPC request IDs, isError and session survival. Handle errors before dispatch without returning a previous failure.
C. Add secret-redaction and unsupported-feature-evidence tests, plus a case where input ran but a postcondition failed if #614 is already in the base. If #614 is absent, document that cross-PR case as integration work rather than importing it.
D. Run cargo test -p bare-mcp --test recovery (all new tests pass), cargo test -p bare-drive --lib, cargo test -p bare-mcp, cargo check --workspace and git diff --check. Record exact failures and known baseline comparison. Open one PR with representative failure JSON and the remaining integration case.

## Done criteria
Failures carry source-derived stable codes and useful redacted advice. No English-string classification, automatic side-effect retries, stale error leakage or unsupported-feature guesses. New tests pass. Existing action receipts and status fields remain intact where present.

## Shared execution rules

The user explicitly requests five implementation agents and five PRs. You own one PR. Do this task yourself; do not create more agents. Use only the isolated checkout supplied by T3. Do not create or switch checkouts. Independent base: the tool-prepared default branch. Record the starting SHA. Main was inspected at 00e701b089a9abe9a399bbe69150c5d772b5c4cc. Compare the named symbols if the base has moved. Stop only on a material contract mismatch; normal line shifts are not blockers.

Read repository CLAUDE.md and applicable AGENTS.md, ~/.agents/providers/claude.md, and ~/.agents/docs/pr-writing.md. Read ~/.agents/docs/git-identity.md if identity mechanics fail. Design decisions and planning below were made by the parent. First save this supplied plan to the stated docs path. Record any implementation deviation and evidence in that document. Do not delegate planning. Compare existing implementations and stdlib before adding code. No new dependencies, broad refactors or unrelated fixes. Preserve redaction, accessibility, shared network cache behavior and the user input path. No unsafe Send/Sync implementations. Never fix an engine gap by bypassing user input with eval.

PR #614 (https://github.com/mrwick1/bare/pull/614, head 02b7f5622ec49a8f3049284e5a5abb3aa4cd6b17) adds act action_status/page_status/verification and optional expect. It was OPEN when assigned. Do not merge it, cherry-pick it, or duplicate its implementation. Your independent PR must work on its prepared base. If it is already present, preserve its fields and assertions. Note integration hunks in your PR so later integration preserves both features. The parent will handle cross-PR integration; do not merge sibling branches. Suggested integration order: #614, observations, recovery, responsiveness, guidance, evaluation. Each branch must pass its own checks before opening its PR.

Use a separate CARGO_TARGET_DIR for this checkout, for example "$PWD/target"; never share the primary checkout's target or another task's target. Shared artifacts previously produced an invalid baseline comparison. Limit build concurrency with CARGO_BUILD_JOBS=2. Define checks before production edits. Add and run a failing regression first for bugs; run the same check after the fix. Capture exact commands, exit statuses and failed names. Do not weaken timing tests or declare timing failures harmless. Known baseline evidence: cargo test -p bare-mcp on clean 00e701b0 had 93 integration passes, 2 failures (a_hidden_iframe_download_is_saved; a_snapshot_of_5000_elements_takes_under_30_ms), 1 ignored window test. These failures require comparison, not assumptions. cargo test --workspace previously ended with exit 143 during compilation, so workspace success is unverified. Do not run repeated full workspace builds across all five agents; run targeted gates, one relevant broader suite, and cargo check --workspace with limited jobs when production Rust changes. Report every incomplete check.

These verified repository commands exist: cargo test -p bare-mcp; cargo test -p bare-drive; cargo check --workspace; git diff --check; python3 -m unittest discover. Inspect existing Cargo.toml and CLI help before using new flags. Commands listed for NEW tests are acceptance targets to create, not claims they exist on main. Match current commit style, for example 'Expose MCP action outcomes and optional postconditions'. Keep changes in scope. Read the final diff before committing. Before pushing run gh auth switch --user mrwick1. The user authorizes committing, pushing and opening a PR; this overrides the generic brief template's 'do not push/open PR'. Open one PR against main. Link it with link_pull_request as the next call after creation. Do not merge or watch the PR. Do not start a server except a temporary loopback fixture owned and shut down by your test. Do not use real credentials, live writes, external accounts or a paid model API. No GUI is required. If GUI verification becomes necessary, read the ui and offstage skills first and follow the active harness rules.

STOP and report if a required change is outside the listed source scope, needs unsafe cross-thread browser state, exposes credentials, removes existing validation, or requires an external service. A negative finding is a valid result; do not fabricate completion or open an empty PR. Fix only what is described. List other findings instead of changing them. If blocked on a required verify gate twice after a reasonable correction, report exact evidence and leave a reviewable committed draft if useful.

Final report: at most 400 words. State COMPLETE or STOPPED; list changes, exact checks/results, untested behavior, base/head SHAs and any integration overlaps. End with: PR <full URL>, head <SHA>, checks and reviews <state>. If no PR, say why. PRs must describe final behavior and limitations, with no attribution footer.

## Implementation record

Base: `00e701b089a9abe9a399bbe69150c5d772b5c4cc`. PR #614 is not in the base.

### Schema

A failed `tools/call` result keeps `content` (the text receipt) and `isError`. It gains `structuredContent.failure`:

```json
{"code": "duplicate", "message": "...", "next_action": "refine_target", "input_dispatched": "unknown"}
```

`message` is the driver message after the session `Redactor`. `input_dispatched` is `no` for a call rejected before dispatch and `unknown` for a driver failure. The server never claims that input did not run after a dispatch. A `step` call whose step failed is not an error: it keeps its report and gains the same object, with no `isError`.

### Code to advice

| Source | code | next_action |
| --- | --- | --- |
| driver `stale_ref`, `detached` | same | `refresh_observation` |
| driver `duplicate`, `not_editable`, `no_focus`, `focus_lost` | same | `refine_target` |
| driver `covered` | `covered` | `dismiss_overlay` (the driver message names the cover) |
| driver `disabled` | `disabled` | `wait_for_condition` |
| driver `not_found`, `hidden`, `expect` | same | `inspect_page` |
| driver `page`, `cookie`, `state`, `shot` | same | `diagnose` |
| argument check, unknown argument, unknown op | `invalid_arguments` | `correct_arguments` |
| unknown tool name | `unknown_tool` | `correct_arguments` |
| session lookup of a session tool | `missing_session` | `open_session` |
| any error with no typed source | `internal` | `diagnose` |

`open_session` is added to the suggested set. No advice says to repeat an action.

### Deviations and limits

- The driver gives no retry fact and no engine-feature evidence, so there is no `retry_safe` field and no `unsupported_browser_feature` code. A generic page error stays `page`.
- Only `act`, `step`, session lookup, argument checks and tool lookup are typed. Other tools keep text errors and get `internal`.
- `missing_session` is checked before dispatch for the session tools, so a bad-argument call with no session now reports the session first.
- The failure is built from the per-call report of `run_live_typed`. No server-level "last error" slot exists.

### Integration with #614 (not done here)

- Its `act` wrapper must keep the `Err(Failure { text: out, ..f })` return so `isError`, `structuredContent` and its `action_status`, `page_status` and `verification` fields all stay on every error path.
- The case "input ran but the `expect` postcondition failed" gives driver kind `expect` here (`inspect_page`, `input_dispatched: unknown`). It needs a test after #614 lands.

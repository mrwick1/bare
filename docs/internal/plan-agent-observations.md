# Reliable page observations

Plan destination in the child PR: `docs/plan-agent-observations.md`.

## Why
An agent must distinguish the current document from an earlier document and learn when a control disappeared. Current Refs::sync resets its nodes, and mint starts numbering at one. This can reuse an old textual ref after navigation or in another live session. snapshot diffs compare current rows with prior hashes but do not emit deletions. Fix the identity and observation contract without adding another DOM tree.

## Current state and owned files
crates/drive/src/snap.rs: Refs::{sync,mint,get}, Seen, snapshot, budget, diff, header. Refs keeps doc, nodes, ids, last and old. snapshot uses last.take for global observations and updates it even when output is truncated. crates/drive/src/locate.rs: Locator::Ref and parsing/display if needed. crates/drive/src/steps.rs: ref locator parsing only if needed. crates/drive/src/run.rs: Memo.refs and found_in/one integration only. crates/mcp/src/main.rs: ref_arg, snapshot_diff, find/read/links reference plumbing and their schema/descriptions only. New tests: crates/mcp/tests/observations.rs and drive unit tests inside snap.rs. Own docs/plan-agent-observations.md. Do not edit README.md, the existing MCP server.rs test file, error policy, dispatch scheduling or evaluation tools.

## Contract and design
1. Give each document incarnation an opaque identity unique across live sessions within one server lifetime, and an observation sequence. Define precisely when a revision advances; it is an observation version, not proof that the DOM was unchanged between calls. Navigation and close/reopen change identity. Hash navigation and same-document changes do not pretend to create a new document unless the engine changes its generation.
2. Keep ref= syntax and e-prefixed handles where possible. Prefer globally monotonic opaque IDs with a document-local ownership map over IDs that can alias. Do not make node lookups linear. Compare that with compound session/document refs in the plan, including parser migration and allocation costs. Use the smallest design that rejects a ref from another session even if both pages have a first button. Never infer ownership from the number alone.
3. Explicitly emit removed refs for a full-page diff. Distinguish removal from a page being replaced, output truncation, a root-scoped read, and mode changes. A truncated response cannot silently mark unshown rows as delivered. Scope/cursor changes must not corrupt the full-page diff baseline. If a baseline is incomplete or incompatible, return a clearly labelled full/reset observation rather than a false 'no change'.
4. Keep output bounded with existing max_chars and continuation behavior. Identify doc/revision and any truncation/reset in machine-readable data or an unambiguous header without breaking existing receipt text. Do not duplicate the entire DOM. Use existing per-row hashes and O(n) full observation traversal; root queries should not need a second retained DOM.

## Implementation sequence and checks
A. Write the plan's algorithm comparison and define revision/removal semantics. Add failing tests for stale refs reused after navigation, a ref passed to another session, and remove-only diffs. Verify cargo test -p bare-mcp --test observations fails for the intended missing behavior.
B. Implement ref ownership and document IDs, then observation versions and removal output. Preserve find/read/links consistency. Add regressions for remove+add, remove after truncation, repeated no-change observations, mode switches, scoped snapshots, pagination and same-document navigation.
C. Run cargo test -p bare-mcp --test observations (all new tests pass), cargo test -p bare-drive --lib (pass), cargo test -p bare-mcp (record all results), cargo check --workspace and git diff --check. Inspect any full-suite failure against baseline without weakening it.
D. Record CPU/memory implications without claiming a gain. If comparing performance, use frozen baseline/new binaries, identical inputs, at least three runs and report machine/spread. Open the one PR. List exact overlapping functions for integration with recovery and responsiveness.

## Done criteria
An old or cross-session ref cannot target a different element; deletion-only updates are visible; bounded/scoped observations do not lose changes; all new tests pass; no unrelated files change. No claims about actual agent success or a live site.

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

Base: 00e701b089a9abe9a399bbe69150c5d772b5c4cc (main; #614 not present).

## Algorithm comparison (step A)

| Design | Lookup | Allocation | Parser change | Verdict |
|---|---|---|---|---|
| Compound ref `s2.d3.e7` | map by (session, doc, n) | none | `ref_arg`, `Locator::Ref`, step parser, every display | rejected: changes the ref type in four crates for no extra safety |
| Per-doc counter plus a doc check on `get` | O(1) | none | none | rejected: `e2` of session A is `e2` of session B; ownership is inferred from the number alone |
| One process-wide monotonic counter, per-session `HashMap<u32, NodeId>` | O(1) | one relaxed atomic add per new ref | none (`e7` stays `u32`) | chosen |

A ref is valid only when the session's current document owns its number. A number of another session or of an older document is not in
the map, so it cannot name another element. A range of the older refs of the session only picks the error text ("older page").
Overflow of the `u32` counter (4.29 billion refs in one server lifetime) is not handled.

## Semantics

- Document id `dN`: from a process-wide counter, new at each change of the engine generation (navigation, reload, close and reopen).
  A hash navigation does not change the generation, so it keeps the id and the refs.
- Revision: the number of full-page snapshots (no `root`) of the document, including the one in the reply. `read` and `links` show the
  latest value and do not advance it. It counts observations. It does not prove that the DOM was the same between two of them.
- Header: `doc: dN rev M` follows `url:` and `title:` (snapshot, read, links). A diff ends with that line. `find` has no header.
- Baseline (`Refs.last`) moves only over delivered rows. A row that `offset` skipped or a cut hid keeps its old hash (or none), so
  the next diff lists it. A `root` snapshot leaves it alone. A baseline of another mode is dropped.
- `removed: e4, e9`: nodes of the baseline that are not in the full outline now (gone, or hidden). At most 20 per reply; the rest stay
  in the baseline and the line says `N more removed`. A full snapshot (no diff) does not list removals.
- `reset: <reason>; this is the full outline`: a diff with no usable baseline (new document, first snapshot, mode change). It never says
  `no change` then.

## Deviations and trade-offs

- After a first snapshot that the cut truncated, the next act diff lists the rows that were never delivered as new, until they are
  delivered. This is correct but can cost more characters than before on a large page.
- Interactive diffs show at most five supplemental text lines. Unshown lines remain pending for a later diff or focused read.
- `snapshot`, `no change (N lines)` gained a second line `doc: ...`. Existing assertions (`no change`, `len < 80`) still hold.

## CPU and memory

No gain is claimed. Cost added per full snapshot: one `Vec<bool>` of the row count, one `HashSet<NodeId>` of the rows in diff mode, a
sort of the removed refs. A ref costs a `HashMap<u32, NodeId>` entry in place of a `Vec` slot (about 8 bytes more). No second DOM.
`a_snapshot_of_5000_elements_takes_under_30_ms` fails on the base and on this branch on a loaded machine (load average about 44): base
293/331/444 ms, new 205/208/215 ms, one run each of best-of-5. Wall time under load is not evidence of a gain or a loss.

## Overlap for integration

- #614: `Server::act` in `crates/mcp/src/main.rs` (the snapshot call after the action; this branch does not touch it) and the act
  description. The `snapshot_diff` text now ends with a `doc:` line; an `expect` check that parses the diff must ignore it.
- Recovery: `Refs::get` error texts (`stale_ref` hints in `compact`) and `found_in` / `one` in `crates/drive/src/run.rs` (unchanged here).
- Responsiveness: `snap::snapshot`, `snap::budget` (now returns the shown flags) and `snap::header` (takes `&Refs`).

## Checks run (load average 35-45, so wall-time tests are not trustworthy)

- Before the fix: `cargo test -p bare-mcp --test observations`: 11 failed (missing doc line, equal refs across sessions and documents, no
  `removed:`, a truncated diff followed by `no change`, a paged snapshot that hid a change).
- After: observations 11 passed; `cargo test -p bare-drive --lib` 9 passed; `cargo check --workspace` ok; `git diff --check` clean.
- `cargo test -p bare-mcp`: 94 passed, 1 failed, 1 ignored. The failure is `a_snapshot_of_5000_elements_takes_under_30_ms` (also fails on
  the base). One earlier full run also failed `a_click_under_a_cookie_banner_fails_fast_and_names_the_banner` (a 1500 ms wall limit);
  it passed 3 of 3 alone. `a_hidden_iframe_download_is_saved` failed on the base run and passed here: not investigated.
- `cargo test -p bare-drive`: `tests/chunked.rs` fails 2 tests ("time limit") on this branch and on the base snap.rs; it does not use `snap`.
- Not tested: a live site, the Windows build, `find` doc identity, behavior over 4 billion refs.

## Parent review

The initial 11 observation tests passed on rerun. Two additional regressions then failed: a partial full-page snapshot discarded unreported removals, and supplemental text beyond the five-line excerpt vanished from the next diff. Both are fixed. All 13 observation tests and all 9 drive library tests pass. `cargo check --workspace` and `git diff --check` pass. The broader MCP server suite has 94 passes, 1 failure and 1 ignored test. The snapshot timing failure also occurred on the baseline; no performance gain is claimed. Integration with #614, live browsing, Windows and counter exhaustion remain untested.

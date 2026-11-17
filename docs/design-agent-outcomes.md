# Agent action outcomes

## Goal

An agent must distinguish a completed operation from an unverified result.
A page observation failure must not invite a second form submission.

## Current behavior

`act` returns a text receipt and an optional snapshot diff.
It preserves success when the operation completes but the snapshot fails.
Other errors after execution can still return `isError` without an execution status.
The agent must interpret text to decide whether to retry.

## Options

1. Add more text markers. This keeps the interface small but requires text parsing.
2. Add structured outcome fields beside the existing receipt. This preserves clients and gives agents explicit states.
3. Replace the receipt with JSON. This breaks clients that consume the current text.

Use option 2. Reuse the driver and its assertion syntax.
The wrapper adds constant state and no extra DOM tree.
A readiness check uses the current layout and document load state. It adds no load wait.
Optional assertions incur the existing driver assertion cost.
No performance improvement is claimed.

## Contract

Every `act` result includes `structuredContent` with these fields:

- `action_status`: `not_started`, `completed`, or `unknown`.
- `page_status`: `ready`, `pending`, or `unknown`.
- `verification`: `not_requested`, `passed`, `failed`, `not_run`, or `unknown`.
- `failure`: optional typed recovery evidence when the call fails.

`completed` means the driver operation succeeded. It does not prove a business result.
`unknown` means execution was attempted but completion is not established.
Never infer that an unknown operation is safe to repeat.
`ready` means a layout exists and document loading has ended.
It does not prove that application data is complete.

An optional `expect` string contains one or more existing `expect` steps.
Validate all assertions before dispatch. Reject actions and JavaScript assertions.
Run assertions only after the operation succeeds.
A failed assertion sets `isError` but keeps `action_status: completed`.
A check that starts but returns no result has verification `unknown`.
Keep the current text as the first content block. Add serialized outcome fields as the second block.

## Acceptance checks

Test completed actions with successful and failed assertions.
Test navigation whose stylesheet stalls after a completed click.
Test rejected assertion input before a click can change the page.
Test a covered target and a missing session.
Check redaction in both text and structured output.
Run the MCP integration suite. Record timing failures without assuming their cause.

## Later work

Observation revisions, removed controls, scheduling, cancellation and cross-session reference identity remain separate changes.

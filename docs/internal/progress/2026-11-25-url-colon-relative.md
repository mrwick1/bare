# URL: a colon in the first segment of a relative reference

## Problem

The Teams data worker stopped about 12 s after it started. The chat list, the app bar and the presence stayed empty. The worker made no `aggregatedEntitlements`, `definitionsv2`, `ups` or chat message requests.

The cause was a fatal error in the worker: `Unsupported("colon in the first relative path segment")`. Teams resolves relative URLs that start with a user id, such as `8:orgid:<id>`. `relative` in `browser_navigation_url.rs` refused every relative reference with a colon in its first segment.

## How it was found

A probe build logged the stop reason of each worker turn (`kids_turn`). The failure was in `Turn::diags`, which no driver prints.

## Change

`relative` no longer refuses the input. `parse` already takes a real scheme (a letter first, then letters, digits, `+`, `-` or `.`). So a colon left in the first segment is part of the path (WHATWG URL, "scheme start state"). The code joins `./<input>`, so that `join` does not read the segment as a scheme. WHATWG gives the same URL for both forms.

## Tests

- New: `crates/bare/src/browser_navigation_url_colon_tests.rs`. It compares 4 inputs with Node's `URL`. It failed before the change with the gate error.
- `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run, saved session)

| | Before | After |
|---|---|---|
| `aggregatedEntitlements` | 0 | 3 × 200 |
| `ups` presence and endpoints | 0 | 4 × 200/201 |
| Chat message requests (`conversations/19:…`) | 0 | 8 × 200 |
| Screen | empty panes | chat header, presence, app bar icons |

## Cost

No cost rows. The change touches only a URL form that failed before.

## Not tested

- The chat list is still empty. The worker now stops on the next limit: `Limit("dom-listeners-per-target")`. That is a separate change.
- Only one live run.

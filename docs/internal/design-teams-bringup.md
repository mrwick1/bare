# Teams bring-up: status and the open blocker

Goal: run authenticated Microsoft Teams (`https://teams.microsoft.com/v2/`) in `bare`, as one step of
replacing a logged-in Chrome profile (Gmail, Teams, Outlook) with `bare`. Teams is first. This file is
the running record. Update it as the work moves.

Last updated: 2026-11-24.

## How to run the live journey

The login + shell-render journey is `docs/internal/teams/teams-login.steps`. It needs the Microsoft
password for `arjun@qilinlab.com` in a secrets file as `PW`.

Password source: the vault item `qilin/microsoft` (`rbw get qilin/microsoft`). Note (2026-11-24): that
item lost its password and could not be rewritten because `vault.mrwick.dev` was unreachable (flaky
WiFi, `os error 110`). When the vault is empty, pass the password through a one-shot secrets file.

Build and run:

```sh
cd ~/coding/personal/projects/bare
cargo build --release -p bare-drive
OUT=/tmp/teams-out            # any writable dir
./target/release/bare-drive --out "$OUT" \
  --secrets <(printf 'PW=%s\n' "$(rbw get qilin/microsoft)") \
  docs/internal/teams/teams-login.steps
```

Read the result:

```sh
jq -r '.failure' "$OUT/result.json"
# error / exception tally, worker lines collapsed:
jq -r '.console[] | select(.level=="error" or .level=="exception") | .text' "$OUT/result.json" \
  | sed -E 's/[0-9a-f]{8,}/<id>/g' | sort | uniq -c | sort -rn
```

`bare-drive` opens http(s) only. To probe local HTML, serve it (`python3 -m http.server`) and open the
`http://127.0.0.1:...` URL.

Do not hammer the Microsoft login. One run per change.

## Done

- **Window message-queue GC** (PR #1018). The window is a port (`queue_window`) with no `Owner::Port`,
  so `Dom::trace` never reached `sched_trace_port` for it. A queued `message` whose data was a rope lost
  its backing through a collection, which crashed authenticated Teams with SIGSEGV. Fix: trace the
  window port's inbox directly in `sched_roots` (`crates/bare/src/browser_scheduler.rs`).
- **IndexedDB transaction lifetime** (PR #1019). The `idb.js` shim finished a transaction on the first
  empty microtask. The `idb` wrapper and `async`/`await` enqueue the next request several microtask hops
  later, so the transaction died first and threw `TransactionInactiveError`. Teams logged 20 of these.
  Fix: commit the transaction at the next task boundary (one `setTimeout(_, 0)` grace), re-checking the
  queue (`crates/bare/src/shims/idb.js`, `kick`). Tests: `crates/bare/src/browser_idb_tx_tests.rs`.

Live effect of the two fixes, same journey, before vs after:

| console line                    | before | after         |
| ------------------------------- | ------ | ------------- |
| `TransactionInactiveError`      | 20     | 0             |
| `TypeError: ... not a function` | 5      | 0             |
| no SIGSEGV                      | crash  | clean 90s run |

## In flight

- **`caches` / CacheStorage shim**. Teams calls `caches.open(...)` during boot; `bare` has no
  CacheStorage, so the pre-load fails (2–3 `Failed to pre-load CacheStorage` warnings). A minimal
  in-memory shim (same pattern as `shims/idb.js`) is being added: `crates/bare/src/shims/caches.js`,
  registered in `crates/bare/src/browser_shims.rs`, tests in `crates/bare/src/browser_caches_tests.rs`.
  This is not the shell blocker (ExpLoader catches the failure), but Teams needs the API.

## Open blocker: telemetry worker infinite recursion

The authenticated shell (`[data-tid='app-layout-area--main']`) still does not render. After the two
merged fixes, the dominant hard error is, 3× per run:

```
listener of message: https://teams.microsoft.com:443/v2/worker/precompiled-telemetry-web-worker-204d4be5a2e88732.js: RangeError: maximum call stack size exceeded
```

The telemetry web worker's `message` listener overflows the JS stack. The 14× `Unhandled error/rejection {}`
lines are Teams' own catch-all logging the resulting `Error` (an `Error` serializes to `{}` under
`JSON.stringify`); they are downstream of this, not separate bugs.

**It is genuine infinite recursion, not a limit that is set too low.** Proof: raising the interpreter
value-stack cap `MAX_STACK` in `crates/vex/src/interp.rs` from `1 << 19` to `1 << 22` (8×) and re-running
the journey left the `maximum call stack` count unchanged at 3. A finite-but-deep recursion would have
cleared; an unbounded one overflows at any cap.

Reference numbers (measured 2026-11-24, `bare-drive` on an HTML probe):

- Native-callback recursion (`[0].forEach(cb)` calling back into JS each level) reaches depth ~1022
  before `RangeError`. Bound by `MAX_STACK` and the frame size, not by `MAX_NEST`.
- `MAX_STACK = 1 << 19` words (`crates/vex/src/interp.rs:83`).
- `MAX_NEST = 100` native↔JS re-entries (`crates/vex/src/reenter.rs:42`). Not the binding limit here.
- Chrome allows ~10920 frames for a trivial function. `bare`'s per-frame word cost is far higher than
  V8's, so the same memory buys fewer frames — but that is not the cause here, since the recursion is
  unbounded.

Worker messages are NOT delivered re-entrantly: each `postMessage` queues a `Task::Deliver`
(`crates/bare/src/browser_scheduler.rs`), so this is not synchronous re-dispatch recursion. The loop is
inside the worker's own JS, triggered by some value `bare` returns that Chrome does not.

### Hypotheses to test next

1. A self-referential object the telemetry serializer walks. Some global or error property in `bare`
   points back at itself (e.g. `err.cause === err`, a getter that returns its receiver, a `self`-chain),
   so a recursive descent never ends. Chrome's equivalent object is not self-referential.
2. `structuredClone` of the posted message hitting a cycle `bare` does not break, if the worker clones
   before serializing.
3. A feature-detect in the telemetry bundle that recurses on a wrong answer from a missing/odd API.

The worker is minified. The URL is public and needs no auth:
`https://teams.cloud.microsoft/v2/worker/precompiled-telemetry-web-worker-204d4be5a2e88732.js` (the hash
changes between deploys — read the current one from the journey's network log). Fetch it with a generic
client (no personal headers), beautify, and find the recursive function on the `onmessage` path. A
faster route: add a temporary host-side capture of the worker `RangeError`'s JS stack (the console entry
currently carries only `level`, `line`, `text`; no stack), or instrument the worker's `onmessage` to log
the deepest repeated frame, to name the exact recursion.

## Key files

- `crates/bare/src/shims/idb.js` — IndexedDB shim (the `kick`/transaction-lifetime fix lives here).
- `crates/bare/src/browser_scheduler.rs` — ports, worker/window message delivery (`Task::Deliver`),
  `sched_roots` GC roots (the window-inbox trace fix).
- `crates/bare/src/browser_shims.rs` — the `SHIMS` registration table for JS web-API shims.
- `crates/vex/src/interp.rs` (`MAX_STACK`), `crates/vex/src/reenter.rs` (`MAX_NEST`) — recursion caps.
- `crates/bare/src/browser_rejection.rs` — `unhandledrejection` / `Uncaught (in promise)` reporting.
- `docs/internal/teams/teams-login.steps` — the live login + shell-render journey.

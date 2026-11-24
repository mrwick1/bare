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

## Telemetry worker recursion: cause and fix

The telemetry web worker's `message` listener threw, 3 times per authenticated run:

```
listener of message: https://teams.microsoft.com:443/v2/worker/precompiled-telemetry-web-worker-204d4be5a2e88732.js: RangeError: maximum call stack size exceeded
```

It also fires on the logged-out page, once per run. That page needs no password, so use it to test.

Cause. `Function.prototype.toString` gave `function () { [native code] }` for every function. The worker
bundles the 1DS SDK. Its `isPlainObject` compares `Function.prototype.toString.call(proto.constructor)`
with the text for `Object`. Because every text was the same, every class instance passed as a plain
object. The SDK deep copy (`function e(t) { ... n[k] = e(v) ... }`) then walked class instances,
followed a cycle and did not stop. The guard that fired was `MAX_STACK` (`crates/vex/src/interp.rs`).
A frame dump at that guard named the cycle.

Fix. `Function.prototype.toString` now gives `function NAME() { [native code] }`, with the `name` of the
function (`crates/vex/src/builtins.rs`, `fn_to_string`). A bound function keeps `function () { ... }`,
as Chrome does. Test: `to_primitive` in `crates/vex/src/script_tests.rs`.

Live effect on the logged-out page, 3 runs each:

| console line                   | before | after |
| ------------------------------ | ------ | ----- |
| `maximum call stack` (worker)  | 1      | 0     |
| `Unhandled error/rejection {}` | 2      | 2     |

The 2 `Unhandled error/rejection {}` lines stay, so this recursion does not cause them. Their cause is
not known. The authenticated journey was not run after the fix: the vault item `qilin/microsoft` has
no password.

Remaining gap: a script function still gives no source text. Chrome gives the source.

### Other finding: native re-entry cap

`MAX_NEST = 100` (`crates/vex/src/reenter.rs`) stops some native-to-JS call chains at depth 100.
Measured with a probe test (2026-11-24): a Promise executor, `toJSON`, a `JSON.parse` reviver, a
`replace` callback, an `Array.from` map function, a Proxy trap, a custom `for-of` iterator and nested
`yield*`. Plain calls, `apply`, `call`, `bind`, `forEach`, `map`, getters and `new` reach 5000. Chrome
allows thousands on all of these paths. This cap was not the Teams cause. Each nested level costs about
4.8 KB in a release build: peak RSS 7.7 MB at cap 100 and 55 MB at cap 10000 (3 runs each, archMachine).

## Key files

- `crates/bare/src/shims/idb.js` — IndexedDB shim (the `kick`/transaction-lifetime fix lives here).
- `crates/bare/src/browser_scheduler.rs` — ports, worker/window message delivery (`Task::Deliver`),
  `sched_roots` GC roots (the window-inbox trace fix).
- `crates/bare/src/browser_shims.rs` — the `SHIMS` registration table for JS web-API shims.
- `crates/vex/src/interp.rs` (`MAX_STACK`), `crates/vex/src/reenter.rs` (`MAX_NEST`) — recursion caps.
- `crates/bare/src/browser_rejection.rs` — `unhandledrejection` / `Uncaught (in promise)` reporting.
- `docs/internal/teams/teams-login.steps` — the live login + shell-render journey.

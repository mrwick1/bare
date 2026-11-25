# Teams bring-up: status and the open blocker

Goal: run authenticated Microsoft Teams (`https://teams.microsoft.com/v2/`) in `bare`, as one step of
replacing a logged-in Chrome profile (Gmail, Teams, Outlook) with `bare`. Teams is first. This file is
the running record. Update it as the work moves.

Last updated: 2026-11-25.

## How to run the live journey

The login + shell-render journey is `docs/internal/teams/teams-login.steps`. It needs the Microsoft
password for `arjun@qilinlab.com` in a secrets file as `PW`.

Password source: the vault item `qilin/microsoft` (`rbw get qilin/microsoft`). It has the password
again since 2026-11-25. Never write the password to a file on disk.

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

Merged on 2026-11-25:

| PR    | Change                                                                                                            | Teams effect                                                                       |
| ----- | ----------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| #1033 | A CSSOM sheet owns its rules. `insertRule` no longer rewrites the `<style>` text.                                 | Fela rule inserts no longer reparse the whole sheet.                               |
| #1034 | The shapes dictionary index rebuilds before deleted slots fill it (`filled` count in `crates/vex/src/shapes.rs`). | A lookup of an absent key looped forever. Teams hung for 19 minutes. Fixed.        |
| #1035 | The `querySelectorAll` result has `item`.                                                                         | Outlook inbox renders.                                                             |
| #1037 | Native re-entry is bounded by Rust stack bytes (64 MB release), not by a count of 100.                            | Removes the `MAX_NEST` cap described below.                                        |
| #1038 | TypeError messages name the callee and the property, as V8 does.                                                  | Errors in the console name the failing call.                                       |
| #1039 | `childNodes` and `children` are the live, `[SameObject]` list of their node.                                      | The CKEditor `_toDom` move loop ended. Before, the step budget stopped it.         |
| #1040 | `Intl.PluralRules` in Vex.                                                                                        | Removes `Intl.PluralRules is not available in this environment`.                   |
| #1042 | `WeakRef` and `FinalizationRegistry` in Vex.                                                                      | Removes `ReferenceError: WeakRef is not defined`. Not yet run live.                |

## Current state (run t19, 2026-11-25, main before #1040)

Teams logs in and then ends on a blank page. `document.body.innerText` is empty.

The journey step 16 waits for "setting things up" to go, or for 30 s. The 30 s fallback passes on a
blank page. So `ok: true` does not show that the shell rendered. Read `BODY` in the console and the
screenshot.

Chain of failure. Each step except the last is verified by a probe:

1. A component throws `WeakRef is not defined` while it renders.
2. React catches it in an error boundary. React then builds a component stack: its prod build calls the
   component outside a render on purpose (`describeNativeComponentFrame`). That call throws React error
   #321. This #321 is a side effect, not a separate bug.
3. The compose box mounts again and again, with a new draft `clientMessageId` each time (23 times in
   t19). A source-rewrite probe that logs each `setState` of the RxJS store hook showed this.
4. React stops with error #185 (maximum update depth) and unmounts the root.

Hypothesis: #1042 (`WeakRef`) breaks this chain. Next step: run the journey on main with #1042.

### Other errors in run t19

These do not stop the shell yet. Each needs a cause before a fix.

- `The provided value is not of type 'CryptoKey'`. MSAL signs a proof-of-possession token with
  WebCrypto RSASSA-PKCS1-v1_5. Vex `SubtleCrypto` has no RSA, ECDSA or HMAC `sign`/`verify`, no JWK
  import and no `CryptoKey` global. Work in progress: branch `t3/webcrypto-msal-sign-algorithms`
  (5 commits, not pushed, stopped on 2026-11-25).
- MSAL `token_refresh_required`, `login_required` (AADSTS50058), `empty_response`, and
  `Token acquisition timed out`. Hypothesis: a result of the `CryptoKey` failure.
- `invalid base64 string input` in the Teams `parseJWT` helper: the token has dots, but its second part
  is empty. Vex `split` and array elision give the right result (probe test). So the token text is
  wrong, not the engine. Cause not known.
- `Invalid value for user ... role: [undefined]`, `User is not authenticated`, `user missing`.
- `Discover::regionGtm::InvalidResponse - <service> is missing in discover response`, for
  `searchService`, `groupsServiceV2`, `consumerLicenses` and `authService`.
- `Cannot read properties of undefined (reading '__typename')`.
- `Warming cycle timed out after 60000ms`, `["leakDetectorService"] are required, but not initialized`.
- 8 `JSON.parse` "Unexpected token" errors. No stack was captured. Hypothesis: caught parses of
  non-JSON storage values, which Chrome also throws.
- The Fela sheet grows to about 52,000 rules. Hypothesis: the remount loop inserts the same rules again.
  Fela finds the insert position with a scan of `cssRules`, so each insert costs O(n).

Run cost (t18, profiling build): 106 s, longest task 23 s, peak RSS 1.37 GB.

## Outlook

The Outlook inbox renders and a mail can be selected (PR #1035). The next errors were `WeakRef`
(fixed in #1042) and a `not a function` error in the `SplitButton` code of `owa.26033.m.*.js`.
Attachments are not tested.

## How to probe a live run

These probes live only in a local probe worktree. Never merge them.

- Throw probe: at `Op::Throw` in `crates/vex/src/interp.rs`, print each new error message once with
  the function names and source offsets of 6 frames. Map an offset to a bundle with
  `dd if=<bundle> bs=1 skip=<offset>`.
- Fuel probe: at `Op::JmpBack` when the step budget is 0, print the frames. It names a loop that the
  budget stops.
- Source-rewrite probe: `run_script_o` in `crates/bare/src/eng_new.rs` reads `TMP_REWRITE_FILE`.
  Each line is `needle<TAB>replacement`. The probe replaces the first match in each script. Use it to
  add `console.error` calls to minified app code without a rebuild.

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

Fixed by #1037 (2026-11-25): the limit is now Rust stack bytes. The record below is the old state.

`MAX_NEST = 100` (`crates/vex/src/reenter.rs`) stopped some native-to-JS call chains at depth 100.
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
- `crates/vex/src/interp.rs` (`MAX_STACK`), `crates/vex/src/reenter.rs` (stack-byte budget) — recursion limits.
- `crates/bare/src/browser_rejection.rs` — `unhandledrejection` / `Uncaught (in promise)` reporting.
- `docs/internal/teams/teams-login.steps` — the live login + shell-render journey.

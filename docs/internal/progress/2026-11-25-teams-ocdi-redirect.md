# Teams OCDI redirect: bare does commit it

Date: 2026-11-25. Base: main 73f3107c. No code change. Result: cause of one hypothesis found, hypothesis refuted.

## Question

After login, Teams fetches `https://teams.cloud.microsoft/?loginHint=...&ocdiRedirect=core-policy`. Does bare fail to commit that response, so that the emptied old document stays?

## Finding

No. bare commits the redirect as a new top document.

- A temporary print in `browser_session.rs` (`drive_loop`, `start`) gave this chain for `teams-login.steps` (1 live run, archMachine, probe build, not committed):
  1. `nav-request kind=Replace url=https://teams.cloud.microsoft/?loginHint=...&ocdiRedirect=core-policy#&migratedSettings=...` with `from=https://teams.microsoft.com/v2/`. So the page called `location.replace` (kind `Replace`).
  2. `start url=...` then `commit url=... how=replace`. The old realm ends and a new `Browser` loads the response.
- The run has 3 `ExperienceLoader` boot logs ("OCDI redirect: Not redirecting - not enabled"). The last one logs "Invalid theme value ignored from URL hash". That is the new document.
- `performance.timeOrigin` read after the redirect is 23.86 s after the run start. The fetch was at 22.94 s. `performance.now()` in bare is session based, so it did not restart.
- The earlier note "the same document stays, `__probe` is still there" was a misreading. The probe step (journey line 14) waits for the shell, and that is the shell of the new document. The marker was set in the new document.
- JS hooks on `Location`, `window.open`, form `submit`, iframe `src`, `history` and `document.cookie` saw no call after the redirect. The only iframe creation was at 158 s.

So hypothesis (a) is wrong. Hypotheses (b) and (c) are not the cause of the empty page: the cookie is not needed for the commit.

## What is left

The emptying runs inside the new document on `teams.cloud.microsoft`, about 1 s after its shell shows (526 elements). It is the Teams page itself, not a navigation. The same-tick removal of `div#app` and the scripts is not seen by prototype hooks installed late, so the page holds earlier references to `removeChild`. Next step to find the caller: a Vex frame print at the DOM `removeChild` native when the parent is `body` and the child is `#app` (`Error.prototype.stack` is not in Vex yet).

Other facts from the run: `Unhandled error/rejection {}` 20 times (the error objects print as `{}`); `cookiev2` returns 401 on both origins; the worker logs `getSerializedKeyForKeypath returned undefined key ... profiles ... email` many times.

## Not tested

No fix was built. Cost rows did not run. The cookie hypothesis (c) was not tested alone.

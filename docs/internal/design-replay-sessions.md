# Replay of a logged-in app (Teams) without a live login

Status: approved by the owner 2026-11-25 (option 1); built in this branch. Phase 0 of the Teams plan: every later step needs a run that is offline, repeatable and free of Microsoft logins.

## Measurement

- Recording: one live Teams login on main 56bab65a with `BARE_SEED=1 BARE_NET_RECORD`. 660 replies, 24 MB.
- Replay of the same steps (`BARE_NET_REPLAY`, `BARE_CLOCK=virtual`, `BARE_CLOCK_EPOCH` at the recording start): 17 requests had no recorded reply. The page failed at the first one ("the document did not load: network error") after 4 s.
- The misses are of 3 kinds:
  1. Cache-busting timestamps in the query: `diagnostics/network?cb=1795608651054`, `1x1-000000ff.png?cb=…`.
  2. Telemetry POST bodies (`OneCollector`): the body hash is part of the label.
  3. MSAL `authorize` URLs. `nonce`, `state` and `client-request-id` are UUIDv7 values, which start with the time in ms. `code_challenge` comes from random bytes.
- `BARE_SEED` fixes `crypto.getRandomValues` and `randomUUID`, but not the time. The live run and the replay never have the same timing, so a time-based value never repeats. `Math.random` is not seeded (`bi_number.rs`, seeded from the OS).

## Session test (2026-11-25)

- Chrome 155 (Playwright) logged in once and saved its `storageState`: 40 cookies and `localStorage` of `teams.cloud.microsoft` (106 keys) and `teams.microsoft.com` (49 keys). `open URL state="FILE"` in bare reads this format.
- bare opened Teams from that file with no login: the main area was visible after 28.4 s. Teams sent only `POST .../oauth2/v2.0/token` (the refresh token), no `authorize`. So the session file holds the token cache, and kind 3 above goes away.
- Exact replay of that recording: 359 misses. The largest groups: 43 token POSTs (the body differs), CORS preflights (`OPTIONS`) of the API calls, telemetry POSTs, and static scripts that the live run never asked for. The replay goes another way than the live run (the virtual clock fires timers at other times), so it loads chunks that the live run did not reach. `BARE_NET_FILL` records them from the CDN.

## Current design

- `replay.rs`: one file per request, named by the FNV hash of the label `METHOD url [body hash]`, plus a counter per label. `find` takes the exact label only.
- This works for the benchmark pages (static URLs). It cannot work for a single-page app that puts the time or random ids in URLs and bodies.

## Options

1. **Start the replay from a saved session, and match volatile requests loosely (proposed).**
   - Session: run A (live) logs in and runs `save-state` (cookies, `localStorage`, `sessionStorage`). Hypothesis: Teams' MSAL keeps its token cache there (`cacheLocation: localStorage`); to verify with the first saved file. Run B (live, recording) opens Teams with `state=FILE` and records. Each replay opens with the same file, on the virtual clock pinned to the start of run B. The tokens in the file then stay valid in every replay. No login and no `authorize` request in the replay: kind 3 goes away instead of being matched.
   - Loose match, behind `BARE_NET_REPLAY_LOOSE=1` so the benchmarks keep exact matching. On a miss of the exact label, a second key: the method, the origin, the path, and the query with each volatile value replaced by a placeholder. A value is volatile when it is all digits of 10 or more, or a UUID, or longer than 32 characters of base64url. The POST body is not part of the second key. The next unused entry of that key in recording order answers.
   - Seed `Math.random` from `BARE_SEED` as `getRandomValues` already is.
   - Cost: one more hash map on the tape (second key to the entries), built at load. Exact-match runs do not build it.
2. **Rewrite the replies (the Web Page Replay way for echoed values).** Keep the login in the replay, and put the request's own `state` and `nonce` into the recorded reply. The `id_token` is a signed JWT with the nonce inside, so its payload must change too. MSAL does not check the signature in the browser, but this ties the harness to one library's checks, and each new library needs new rules.
3. **Record the page's sources of time and randomness, and replay them.** Every `Date.now`, `performance.now` and random call returns the recorded value in order (as `rr` does for system calls). Exact, but any change in bare that changes the order of these calls (a fix, a new API) makes the rest of the run miss. The replay then breaks after each change that we want to measure.

Option 1 removes the login flow from the replay instead of imitating it, and stays valid when bare changes. Chrome's Web Page Replay (`wpr`) also matches an exact request first and then the closest one (to verify in its source before the build).

## Limits and risks

- A request that the app sends only when it has no token (a token refresh) still misses. The pinned clock should stop it. To verify: the count of misses on the first replay.
- Teams state in IndexedDB is not in the saved session. The first replay then starts with an empty IndexedDB, as run B did, so the requests match.
- The tape and the session file hold live credentials. They stay in `/data/bare-tmp/teams-rec` (mode 0700) and are never committed.

## Check

- Exact-match benchmarks: the existing replay tests pass, and loose matching is off by default.
- New tests: a loose key with a `cb` timestamp, a UUID in the query, a POST body that changes; the order of entries with the same key.
- Teams: replay misses drop from 17 to 0 (or each one left has a reason). Two replays in a row give the same requests and the same screenshot. Time to the shell on replay, 3 runs.

## How to use

All files below hold live credentials. Keep them in a directory of mode 0700 under `/data/bare-tmp` and never commit them.

1. Reference and session (one Microsoft login): `PW="$(rbw get qilin/microsoft)" node tools/ref/chrome-teams-ref.mjs OUT "<Chrome user agent>"`. It writes the timings, a HAR with bodies, the console, screenshots, HTML, the boxes of every `data-tid` element, and `OUT/state.json`.
2. Record (live, no login): `date +%s%3N > START; BARE_SEED=1 BARE_NET_RECORD=TAPE bare-drive --var STATE=OUT/state.json --out DIR docs/internal/teams/teams-session.steps`.
3. Close the tape: run the same with `BARE_NET_FILL=TAPE BARE_NET_REPLAY_LOOSE=1 BARE_CLOCK=virtual BARE_CLOCK_EPOCH=$(cat START)` until stderr has no `no recorded reply` line.
4. Replay (offline): the same with `BARE_NET_REPLAY=TAPE` in place of `BARE_NET_FILL`.

## Result (2026-11-25, this branch)

- Recording: 68 s live, no miss. Fill: 7 misses and 26 loose replies in the first run, 0 misses in the next 2.
- 2 offline replays: 20 s each, 0 misses, 27 loose replies, all 6 steps pass. The 2 screenshots are byte-identical, and the 2 runs make the same 557 requests (the order differs a little). Peak RSS 789 MiB (live: 797 MiB).
- The replay has 10 console errors more than the live run, all known:
  - 7 WebSocket failures (`trouter`, `augloop`): the tape has no WebSocket. Teams' push channel is closed in a replay.
  - 3 CORS errors of telemetry: a loose reply recorded for one origin (`teams.microsoft.com`) answers a request of the other (`teams.cloud.microsoft`). Telemetry only.
- Not done: WebSocket record and replay.

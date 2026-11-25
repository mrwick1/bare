# Offline replay of logged-in Teams

Design: `docs/internal/design-replay-sessions.md` (option 1, approved by the owner on 2026-11-25).

## Change

- `BARE_NET_REPLAY_LOOSE=1`: when the exact label has no reply, the request takes the next unused reply of its loose key (method, URL with volatile query values as `*`, no body), in recording order. Off by default: the benchmark replays keep exact matching.
- `Math.random` takes its seed from `BARE_SEED`, as `crypto.getRandomValues` does.
- `docs/internal/teams/teams-session.steps`: Teams from a saved session (`--var STATE=FILE`), no login.
- `tools/ref/chrome-teams-ref.mjs`: the Chrome reference capture of the Teams login journey, and the session file.

## Check

- New test file `replay_loose_tests.rs`, 3 tests: the loose key, the recording order and the repeat of the last reply, an exact label first and a different path still a miss. `t`: pass.
- Teams, archMachine, see the design doc: the live recording (68 s) closes after 1 fill run. 3 offline replays: 0 misses, all steps pass, 20 s each, byte-identical screenshots.
- Cost rows: none. The change is off unless the variables are set. Exact replay and `Math.random` without `BARE_SEED` take the same path as before, plus one `OnceLock` read at the first `Math.random` of a realm.
- Not tested: Windows; Outlook with this harness.

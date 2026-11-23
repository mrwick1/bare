## 2026-11-23: block tracker and analytics requests (#961)

### What changed
- New `crates/net/src/blocklist.rs`: a sorted list of 27 tracker hosts (Google Tag Manager, Google Analytics, DoubleClick, Segment, Hotjar, Datadog RUM, and others). A host matches when it, or a parent domain, is in the list. `BARE_TRACKERS=allow` turns the list off.
- `Client::start` (`crates/net/src/http.rs`) refuses a listed host before DNS and before any socket. Every request goes through it: scripts, fetch, XHR, redirects.
- `cause::kind` names the error `blocked`. A script load gives `LoadError::Blocked`, shown in the diagnostics as `blocked: tracker host`, not as `network error`.

### Measurements
Machine: archMachine, load average about 6 from other agents. Same binary for both sides (`BARE_TRACKERS=allow` vs default). Release build of `bare-drive` on origin/main plus this change. Replay of a corpus recorded from the live sites with the list off (`tools/bench/journeys.sh record`, then `run`). 6 runs per cell (2 rounds of 3). Instruction counts hold under load; wall time does not.

Only 4 of the 48 recorded journeys request a listed host. The other 44 do no different work.

| Journey | Listed hosts requested | Instructions allow | Instructions block | Change | Spread (allow / block) | Wall ms allow / block | Peak RSS KiB allow / block | Replay misses allow / block |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| devto-home | googleads.g.doubleclick.net, static.doubleclick.net, www.googletagmanager.com | 5832M | 5135M | -12.0% | 1.1% / 3.0% | 749 / 656 | 134610 / 122076 | 0 / 0 |
| react-docs | www.googletagmanager.com | 3885M | 3486M | -10.3% | 1.5% / 0.0% | 399 / 367 | 74832 / 67422 | 0 / 0 |
| substack-home | www.googletagmanager.com | 6165M | 6002M | -2.6% | 0.0% / 0.0% | 704 / 754 | 145522 / 144984 | 1 / 1 |
| go-dev-home | ad.doubleclick.net, www.googletagmanager.com | 1898M | 479M | -74.8% | 6.9% / 2.1% | 431 / 227 | 56442 / 37980 | 11 / 1 |

Notes:
- The gain is beyond the spread for devto-home and react-docs. For substack-home it is small (2.6%) with a 0.0% spread; its wall time went the other way (704 vs 754 ms).
- go-dev-home is not a fair row: the allow side had 11 replay misses (a replay with misses did not do the recorded work). Do not cite its gain.
- All four journeys passed on both sides.
- Peak RSS comes from `wait4` of the same copied binary (`cp --reflink=never`).

### Tests
New file `crates/bare/src/browser_loader_trackers_tests.rs`: a tape holds a reply for a script. From `www.googletagmanager.com` the script does not run, the page settles, and the diagnostics say `blocked: tracker host`, not `network error`. From another host the same reply runs (control, so the tape works). `t` passes.

I wrote the tests after the implementation. I did not run them against the old code.

### Not tested
- Windows (the check is host string matching only; no OS code).
- A live run on the real sites with the list on (the measurement is a replay).
- The 44 journeys that load no listed host.
- `fetch` and XHR from page JS to a listed host: the check is in `start`, so they are covered by design, but no test names them.
- Any `open` option: `BARE_TRACKERS=allow` is the only switch.
- The `network` list of a driver report: I did not check how it shows the `blocked` cause.

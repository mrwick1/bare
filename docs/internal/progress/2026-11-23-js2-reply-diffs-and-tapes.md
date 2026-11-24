# 2026-11-23 — Reply differences of OLD and js2 on 15 journeys, and tape numbers

This is a measurement. It changes no engine or browser code. It answers `docs/next-work.md` items 11 and 12: does a different reply count come from the live site or from the engine, and what are stable tape-based instruction counts?

## Inputs

- Source: branch `t3/measure-js2-reply-diffs` at `7d02af28` (main after #987). One release `bare-drive` for both engines (`cargo build --locked --release -p bare-drive --bin bare-drive`, no symbols).
- Host: archMachine, shared with two other agents. Trust the instruction counts, not wall time.
- Env for every run: `BARE_SEED=1 LC_ALL=C GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. OLD is `BARE_JS_EXEC=auto`, js2 is `BARE_JS_EXEC=new`.
- Journeys: `tools/drive/public/<name>.steps` with the URL in its header comment.
- Live record (one run per engine and journey, OLD first): `BARE_NET_RECORD=/data/bare-tmp/i11/tapes/<journey>-<old|new>`. All 30 runs passed their steps. No 429 and no captcha.
- Replay: `BARE_NET_REPLAY=<tape> BARE_CLOCK=virtual`, under `perf stat -x, -e instructions:u`. Each engine ran on its own tape and on the other tape (4 runs per journey). The five item 12 journeys ran 3 times on their own tape, rotating the engine.
- Tapes: `/data/bare-tmp/i11/tapes/<journey>-<old|new>/`. Run output (JSON, stderr, perf): `/data/bare-tmp/i11/{out,rep}/`.
- The report in the JSON has no initiator and no method. "Replies" below is the length of its `network` list, taken before the last step. A tape holds every request, also the ones after the last step. So the tape count is larger. I did not find initiators, so each cause below is a hypothesis unless the text says otherwise.

## Result in short

- In this live run, 10 of the 15 journeys have the same reply count and the same set of tape requests on both engines. The item 11 differences of those journeys did not repeat: site noise (hypothesis: one live run per engine).
- Five journeys differ in the live reply count (python-org-docs-link, react-docs, go-dev-home, wikipedia-article, wikipedia-toc-languages). On a replay, the reply counts of both engines match on every tape, except for react-docs (js2 panics) and craigslist-home (the replay fails for both).
- One engine difference with a visible request: on the OLD tape, js2 fetches two `substackcdn.com` face images that OLD does not fetch (141 against 143 replies).
- One engine bug: js2 panics on the replay of react-docs.
- Tape instruction counts are stable: spread at most 0.3% over 3 runs. The live spread of item 12 is 1.2× to 4× on the same rows.

## Table

Live replies: OLD / js2, from the run that made the tapes. Tape instructions are M instructions of the engine on its own tape: minimum [range]; one run unless the cell has a range. Cross: the reply count of an engine on the other tape.

| Journey | Live replies OLD / js2 | Cause | Requests that differ | Tape instr. OLD | Tape instr. js2 |
|---|---:|---|---|---:|---:|
| blog-rust-lang-home | 15 / 15 | site noise | none | 177.7 | 179.0 |
| hn-new-past | 9 / 9 | site noise | none | 150.7 | 148.4 |
| paulgraham-home | 7 / 7 | site noise | none | 95.1 | 94.6 |
| python-docs-search-results | 57 / 57 | site noise | none | 2,393.2 | 2,326.2 |
| python-org-docs-link | 25 / 27 | site noise (hypothesis: timing) | js2 only: `statuspage.io/api/v2/status.json`, a second `analytics.python.org/js/script.file-downloads.outbound-links.js`. The tapes hold the same 36 requests for both engines. Replay: 34 replies for all four runs. | 1,275.2 | 1,297.3 |
| react-docs | 10 / 24 | engine difference | js2 only in the report: 9 `_next/image` PNGs and a second fetch of 4 chunks. OLD only: the second `framework` chunk and `css/f647474d…css` (tape counts 33 / 34; a repeated fetch of the same URL). js2 panics on replay (below). | 5,030.4 | (panic) 3,563.2 |
| rust-book-chapters | 60 / 60 | site noise | one body 24 bytes longer on the OLD live run. A replay gives the bytes of the tape, for both engines. | 580.4 | 609.0 |
| rust-book-sidebar | 40 / 40 | site noise | none | 356.5 | 372.6 |
| sqlite-org-home | 9 / 9 | site noise | none | 61.7 [61.7–61.8] | 60.4 [60.4–60.4] |
| substack-home | 142 / 142 | engine difference (small) | tapes: the body of one `POST /api/v1/firehose/batch` differs (hash of a changing body: noise). js2 on the OLD tape fetches two face images from `substackcdn.com/image/fetch/…logged_out_home/face…` that OLD does not fetch: 141 / 143 replies. | 5,864.1 | 4,675.4 |
| wikipedia-article | 62 / 63 | site noise | js2 only: a second `BannerLoader` fetch (a donation banner campaign: A/B content). Two `POST …/ins-502b/v2/events` bodies differ (timestamps). | 1,993.4 | 1,991.2 |
| craigslist-home | 3 / 3 | unknown (tape defect) | none. The replay fails for both engines: `the document did not load: network error` on the first `302`, with no line on stderr. | 1.5 (failed) | 1.5 (failed) |
| devto-home | 1 / 1 | site noise | none | 3,902.1 [3,902.1–3,902.2] | 3,645.3 [3,645.3–3,646.8] |
| wikipedia-toc-languages | 47 / 57 | site noise (hypothesis: timing) | js2 only: 10 `load.php?modules=skins.vector.icons*&image=…` icon fetches. On replay js2 does not ask for them on the OLD tape (the only miss is a telemetry POST). | 2,496.0 [2,496.0–2,496.2] | 2,440.6 [2,440.6–2,440.9] |
| go-dev-home | 14 / 13 | site noise | OLD only: `go.dev/js/index.js`. 30 requests miss the tape on replay in both engines (the recording ended before the page asked: use `BARE_NET_FILL`). | 455.2 [455.2–455.3] | 462.1 [462.1–462.1] |

## Item 12: live samples at two levels

The live ranges are from [the final benchmark](2026-11-23-final-engine-benchmark.md) (OLD / js2, M instructions).

| Journey | Live OLD | Live js2 | Tape OLD | Tape js2 |
|---|---:|---:|---:|---:|
| craigslist-home | 2,353.2 [2,353.2–2,353.6] | 474.7 [474.7–1,967.3] | 1.5 (replay fails) | 1.5 (replay fails) |
| devto-home | 4,955.1 [4,955.1–5,897.9] | 5,775.7 [5,775.7–9,309.4] | 3,902.1 [3,902.1–3,902.2] | 3,645.3 [3,645.3–3,646.8] |
| sqlite-org-home | 109.2 [109.2–150.5] | 127.7 [127.7–149.8] | 61.7 [61.7–61.8] | 60.4 [60.4–60.4] |
| wikipedia-toc-languages | 1,237.0 [1,237.0–1,365.9] | 1,365.3 [1,365.3–1,378.9] | 2,496.0 [2,496.0–2,496.2] | 2,440.6 [2,440.6–2,440.9] |
| go-dev-home | 1,736.5 [1,736.5–3,815.8] | 1,314.2 [1,314.2–5,919.3] | 455.2 [455.2–455.3] | 462.1 [462.1–462.1] |

- The tape removes the two levels of devto-home, sqlite-org-home and go-dev-home. The live spread comes from the replies of the live site (what the page fetched before the step ended). It does not come from the engine. Hypothesis: the page runs more or less work with a late or missing reply.
- The tape of go-dev-home is short: 30 requests miss it, so 455 M is a lower bound. The tape of wikipedia-toc-languages is also not the live work: its tape count is 2,496 M against 1,237 M live. The live run is a journey with 47 replies. The replay gives 46 replies and 1 miss. I did not find why the replay costs twice the live run. To verify: the virtual clock.
- Craigslist gives no tape number. Its replay fails at the first redirect for both engines (1.5 M). The same problem as the `/learn` redirect in [the design note](../design-js2-real-pages.md). Hypothesis: the stored `302` of `www.craigslist.org/` loops on the replay (cookie or `Location`). I did not test it. The live bimodal sample (475 M and 1,967 M) stays unexplained.

## Engine differences

1. react-docs, js2 only: the replay panics at `crates/bare/src/browser_events.rs:104` (`event is registered`, an `expect` after `dispatch_inner`). It happens on both tapes, so it does not depend on the tape of the other engine. OLD finishes the replay. Exit code 1, and the report is not written. Live, js2 does not panic. Hypothesis: a page script removes the event while it dispatches, after a missing reply (13 requests miss the tape). To verify: a failing test in `crates/bare` that removes an event during `dispatch_inner`.
2. substack-home, js2 only: on the OLD tape, two extra `substackcdn.com` face images (4.7 KB more). Hypothesis: js2 runs a lazy-load script further than OLD does. The other agent profiles this journey.
3. react-docs: the live report shows 24 replies for js2 against 10 for OLD. Both are before the last step. Not explained: the tape counts are 34 and 33. Hypothesis: timing of the step. The other agent profiles this journey.

Not an engine difference: the five other differing journeys above. In each, an engine on the other tape gives the same reply count as the tape's own engine.

## Not tested

- One live run per engine. A repeat could show other differences. The 10 equal journeys are equal in this run only.
- Initiators of requests. The JSON report has none.
- The cause of the craigslist replay failure and of the 2× replay cost on wikipedia-toc-languages.
- Wall time. The host was shared.

Tapes stay in `/data/bare-tmp/i11/tapes` (28 MB).

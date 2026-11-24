# Design: where a journey ends (R2, 2026-11-14)

Status: measured, not built. The owner decides. No driver code is changed. Measurements and the full tables: `docs/progress.md`, section "Where a journey ends: bisect of the 1,560 M drop and the load state of the settled driver".

## Problem

The replay driver of the journey benchmark ends a journey when the last `expect` passes. Before each step and at the end it calls `Session::settle_pictures`. Since PR #529 that call finishes the pictures and runs nothing else of the page. It runs no timer, no script and no load of the document.

A step of a journey names what it needs (`expect title=...`). It does not wait for `load`. So the last document of a journey often stops before its scripts run. Our change in work then looks like a saving, but part of it is work that a user pays and that the journey no longer does.

Measured on main `d6837f6d`, corpus `virtual-settled`, 47 journeys, 96 documents:

| | New driver (main) | Old driver (`c4e32a85`) |
| --- | ---: | ---: |
| Documents left without `DOMContentLoaded` | 26 | 3 |
| Documents left without `load` | 37 | 6 |
| Journeys whose last document is not at `load` | 25 | 5 |
| Requests pending when the journey ends | 113, in 26 journeys | 31, in 10 journeys |
| Scripts of the last documents that never ran | 217 of 381 | 29 of 382 |

Two corrections to what we believed:

- The old loop did not run page timers for up to 5 s. It ran 346 calls, 74 of them ticked the page, 1,569 ms of page time in all, 127 ms at most in one journey. It ran each document to the end of its load. That is work a user pays.
- The new driver is not "less work for the same journey". It is less work because the last document of 25 journeys is not loaded.

The old driver was not a fair end either. It waited for the pictures, not for `load`. 5 last documents did not reach `load` with it (`archlinux-org-home`, `craigslist-home`, `crates-io-home`, `nodejs-org-home`, `substack-home`).

## How others define the end

I read these pages on 2026-11-14.

- Playwright `page.goto`, option `waitUntil`, default `load`: `'load'` is "when the `load` event is fired". `'domcontentloaded'` is "when the `DOMContentLoaded` event is fired". `'networkidle'` is marked DISCOURAGED: "no network connections for at least 500 ms". The page says: "Don't use this method for testing, rely on web assertions to assess readiness instead." `'commit'` is "network response is received and the document started loading" (<https://playwright.dev/docs/api/class-page>).
- Playwright navigations: a click waits for the element to be actionable. The page does not say that a click which navigates waits for `load`. It advises `page.waitForURL()` for that. Only `page.goto()` waits for the `load` event (<https://playwright.dev/docs/navigations>). So an action ends at its own condition, and a navigation by URL ends at `load`.
- WebPageTest: "Document Complete" is the `onload` event. "Fully Loaded" is the time of the last network activity before 2 s without any, after Document Complete; the 2 s are not counted. A script command `setactivitytimeout` changes the 2 s. An open idle websocket does not count as activity (search result summary of the WebPageTest forum and gist pages; I did not read the primary page: its host failed TLS).

Both tools use `load` as the end of a document, and a quiet network only as an extra, with a time bound. Neither makes the end depend on page timers.

## What a fair end means here

For a step: the step ends when its own condition holds (`expect ...`). This is what a user waits for. Work after that, in a document the user left, is not paid by a user: the browser aborts the load. I do not propose to change it.

For the end of a journey: the user stays on the last page. The browser finishes its load. So the end should be: the last document reached `load`, the pictures it started are done, and no more than a page-time bound passed. The bound is a stop for a hang. In the measurement the wait for `load` took 2 ms of page time at the median and 33.6 ms at most (`react-docs`); the 5 s bound was never reached.

## Options

I built each option as an environment switch in a temporary patch (`/data/bare-tmp/r2/journey-end/trace.patch`, not in the repository). The table gives instructions of the 47 journeys, sum of 3-run medians, new main against the frozen baseline `bench-base-vclock` with the same switch. Criterion 2 needs 80% or lower.

| Option | What ends the journey | Main, M | Baseline, M | Criterion 2 | Journeys with replay misses |
| --- | --- | ---: | ---: | ---: | ---: |
| 0. Keep | last `expect`, pictures settled | 42,319 | 59,372 | 71.3% | 0 |
| A. End at `load` | 0, then the last document runs to `load` | 70,467 | 89,059 | 79.1% | 6 |
| B. End at network idle | A, then no request or task waits (`Page::idle`), bound 5 s | 74,399 | 95,115 | 78.2% | 10 |
| C. Every navigation at `load` | each step starts after the document of the step before reached `load`, and A at the end | 93,932 | 114,251 | 82.2% | 13 |

For the reference, the old driver on its own tree (`c4e32a85`, corpus `virtual`, 0 misses) gives 95,453 M: option C does the same work (93,932 M, -1.6%).

A journey with a replay miss did not do the recorded work, so the options A to C are not valid measurements until the corpus has the replies that the longer run asks for (see "What it needs"). The misses are the same on the baseline and on main, so the ratios are an estimate. Sums over only the journeys with 0 misses: A 70.6% (41 journeys), B 66.1% (37), C 64.4% (34).

### Option 0: keep the driver

- Data: nothing changes.
- Cost: none. The corpus stays valid.
- Problem: 25 of 47 journeys end in an unloaded last document. A change that makes script work cheaper or removes it is seen only in the journeys that happen to load. A change that makes the load path slower is hidden. Criterion 2 reads 71.3% (met) on a measurement that skips the work of the last document in about half of the journeys.

### Option A: the end of the journey waits for `load` (recommended)

- Change: in `Session::finish`, before `settle_pictures`, tick the page until `Browser::loaded()` (as `Page::wait_load` does) or until the page-time bound (`CAPTURE_WAIT`, 5 s). Steps do not change.
- Complexity: one loop, O(page ticks), the same loop that `wait_load` has. No new data structure.
- Effect: +28.1 G (+66%) on main, +29.7 G on the baseline. 25 of 47 journeys change by more than 1 M. Nine journeys carry 27.7 G of the 28.1 G: `react-docs` +12,458 M (9 misses), `substack-home` +10,339 M (10 misses), `go-dev-home` +1,401 M, `devto-home` +1,306 M, `wiktionary-portal` +792 M, `wikipedia-portal` +475 M, `python-org-search` +414 M, `wikipedia-main-random` +394 M, `wikipedia-search` +115 M.
- Fit: it is Playwright's `goto` default (`load`) applied to the end, and WebPageTest's Document Complete. The old driver's last document reached `load` in 42 of 47 journeys; this option makes it 47 of 47.

### Option B: A and a quiet network

- Change: wait until `Page::idle()` is `Ok` (loaded, no fetch, no script, no task waiting; timers that are not due do not count), with the same bound.
- Effect: +3.9 G more than A (74.4 G). `python-docs-search` and `python-docs-search-results` start post-load fetches that have no reply on the tape (34 and 15 misses). `nextjs-docs` (does not pass on replay) is the only journey that reached the 5 s bound.
- Problem: the end depends on what the page starts after `load` (beacons, polling). Playwright marks the same idea as discouraged. WebPageTest needs a 2 s quiet window for it and gives a way to shorten it. A virtual clock does not wait 2 s for free: each idle look is a tick. I do not recommend it as the rule. It can be a second, separate "fully loaded" measure.

### Option C: every navigation waits for `load`

- Change: before each step, tick the page until the document reached `load`, as the old loop did (the old loop waited for `interactive` and the pictures).
- Effect: 93.9 G, the work of the old driver. 34 of 47 journeys change by more than 1 M, for example `mdn-reference` +6,281 M and `wikipedia-article` +3,471 M, both with misses.
- Problem: it counts the load of a document that a fast user left. The browser aborts that load. It is the patient-user case. I do not recommend it as the rule.

### Option D: the journey says it (variant of C)

A new step `expect load` in the `.steps` files, at the points where a user waits. The driver stays as it is. The totals are those of A or C, by where the steps stand. Cost: a new step kind in `crates/drive`, and 47 file edits. Gain: a journey that models a fast click can skip the wait. I would do it only if A is not enough.

## Effect on each criterion

Each row is measured with the same switch on the baseline and on main (3 runs, spread 0.4% or less).

| # | Measure | Option 0 | A | B | C |
| --- | --- | --- | --- | --- | --- |
| 1 | 47 journeys pass | 47 | 47 | 47 | 47 |
| 2 | Total instructions, main / baseline | 71.3% | 79.1% | 78.2% | 82.2% |
| 4 | `python-org-docs-link`, `rust-book-search` | 1.28 and 1.63 times | 1.28 and 1.63 | 1.28 and 1.63 | 1.28 and 1.63 |
| 6 | `python-docs-search-results` peak RSS | -17.5% (94.2 against 114.3 MiB) | -16.4% | not run | -17.2% |

- Criterion 4 and 6 do not change: their last documents reach `load` in every option.
- Criterion 2 moves from "met with room" to "met by 0.9 points" under A, and to "not met" under C. The number is an estimate while 6 to 13 journeys have misses. `react-docs` (12.6 G, ratio 1.00) and `substack-home` (10 G in A) are in it, and our work has not changed them. Our work has not lowered their cost, so they pull the ratio up.
- The baselines in `docs/perf-goal.md` ("Baseline on the recorded corpus, settled driver": 59,370 M) change under every option except 0. Each option needs a new frozen baseline measurement with the same driver.

## What it needs

- A new corpus for A to C (for example `virtual-loaded`): `journeys.sh fill --clock virtual` on the journeys with misses. A: `react-docs`, `substack-home`, `crates-io-home`, `go-dev-home`, `craigslist-home`, `nodejs-org-home`. The fill sends missing requests to the live sites, as every earlier fill did (it sent POST beacons).
- A new frozen baseline run with the same driver, and an update of the criterion table.
- Options A to C: `docs/perf-goal.md` needs one sentence that says what ends a journey.

## Recommendation

Option A, without the idle wait. It makes the final document of a journey do its load, as a user who stays on the page pays, and it uses the definition that Playwright and WebPageTest share (`load`). It leaves a step to end at its own condition. Expect about 70 G for 47 journeys on main, and criterion 2 near 79% after the corpus fill. Hypothesis: the ratio after the fill is within a few points of this, because the six journeys with misses have equal misses on both trees.

## Not measured

- A refill corpus, so no valid number for the 6 to 13 journeys with misses.
- Wall time, and a live run.
- Option D, and the 2 s quiet window of WebPageTest.
- Whether main has more replay misses with the old settle loop than `c4e32a85` has. With the old loop put back on main, 7 journeys show misses on `virtual` (`react-docs` 9, `wikipedia-main-random` 3, `github-docs-home` 3, `go-dev-home` 2, `mdn-reference` 2, `wikipedia-article` 2, `wikipedia-search` 1). On `c4e32a85` there are 0. Hypothesis: a change of the requests of the page code since then. I did not look for the commit.

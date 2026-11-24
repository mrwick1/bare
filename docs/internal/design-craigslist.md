# Design: the 47 G of `craigslist-home` (R2, 2026-11-14)

Status: measured, not built. The owner decides. No product code is changed. Measurements and run counts: `docs/progress.md`, section "Profile of `craigslist-home`: why the last page costs 47 G". Raw logs, patches and scripts: `/data/bare-tmp/r2/craigslist/` (see its `README.txt`). Nothing of it is in the repository.

## Problem

Since PR #542 the journey benchmark runs the last page to its `load` event. `craigslist-home` then costs 47,008 M instructions and 749 MiB peak RSS on main `49d6475d`. That is 39% of the total of the 47 journeys (119,049 M). Criterion 2 of `docs/perf-goal.md` is 86.1% (target 80%). The journey cost 48,250 M on the frozen baseline, so our work did not change it (-2.6%).

Owner decision (2026-11-14): profile the page and write a design before any code.

## What the page does

The steps are `open`, `expect title`, `expect js "a.length > 50"`. All three pass in the first 9 ms of page time. The 47 G are the wait at the end of the journey (`Session::finish` runs the page to `load`). One run: 5 ticks, 8 scripts, 8 timer tasks, 5.2 s of CPU.

The home page is a loader. Its first script (`c9c381….js`, 24 KB) defines `cl.oldBrowser`:

```js
cl.oldBrowser = function(){ try { return !(!cl.devKnobs||!cl.devKnobs.oldBrowser) || (!window.SharedWorker || (eval("cl?.bad"), eval("var SnifferClass = class {...static {...}}"), !1)) } catch(e){ return !0 } }()
```

bare has no `SharedWorker` (`browser_worker.rs`: "Not here: `SharedWorker`"). So `cl.oldBrowser` is `true` in bare (checked: `OB=true SW=undefined`). The loader then serves the ES5 path:

| Resource | Size (decoded) | Why it loads |
| --- | ---: | --- |
| `formatjs-polyfill-base….es5.js` | 762 KB | `oldBrowser` puts it first in `cl.resources` |
| `formatjs-polyfill-en….es5.js` | 129 KB | the same |
| `polyfill….es5.js` (core-js 3.50) | 289 KB | `oldBrowser` |
| `browsePostings….es5.js` | 1,349 KB | the ES5 build of the app (jQuery, jQuery Migrate, Leaflet, the page code) |

A modern browser loads `browsePostings….js` (360 KB gz; ES2015+) and nothing else.

Execution order on main (instructions of the thread, from a perf counter read around each script, task and event; run spans cost 1.3 M in all):

| Phase | Instructions | Share of 47,008 M |
| --- | ---: | ---: |
| The 8 scripts (parse and top-level run; the factories are only registered) | 470 M | 1.0% |
| 7 timers, DOMContentLoaded, 1 selection task | 7 M | 0.0% |
| The `load` event of the last injected `<script>`: its listener calls `cl.onStagingComplete.unlock`, which calls `runStagingFactories` and runs every staged AMD factory | 45,474 M | 96.7% |
| Rest (HTML, CSS, style, layout, network) | about 1,050 M | 2.2% |

So the cost is one synchronous call chain, not a loop, a poll or a timer. There is no timer that repeats. No budget fired (the console has 2 lines: jQuery Migrate and a Leaflet notice).

Inside the `load` chain (a second run with a counter around `runStagingFactory`; the counter itself costs 14%, so the shares are an upper bound for the call-heavy part):

| Factory | Traced share |
| --- | ---: |
| `formatjs-polyfill-base` | 83.7% |
| `browsePostings` app code (`public-es5-libs` 0.3% of it) | 8.2% |
| `formatjs-polyfill-en` | 0.6% |
| core-js polyfill | 0.4% |

Inside `formatjs-polyfill-base`: two modules, `Intl.NumberFormat` and `Intl.DateTimeFormat`. Each runs `shouldPolyfill("en")` when it loads. In bare that returns a locale (not `undefined`), so the module calls `H([a], availableLocales, "en")`: the "best fit" locale matcher of `@formatjs/intl-localematcher`. It computes, for the one requested locale and each of the 561 available locales, `F(a, n)`: two `new Intl.Locale(x).maximize()` and up to three calls of `R`, which scans the language-matching rule list (about 1,125 rules) and tests each rule with `z()`. That is 1,122 calls of `F` in all (2 × 561). The matcher costs 42.9 G in total, 38 M instructions for each `F` (derived below). The listener runs it all in one JS call chain.

### Which probes fail in bare

The formatjs `shouldPolyfill` returns a locale (it asks for the polyfill) when a probe of `NumberFormat` or `DateTimeFormat` gives a different answer than a browser. I ran each probe in bare:

| Probe | Chrome (spec) | bare |
| --- | --- | --- |
| `NumberFormat("en",{style:"unit",unit:"bit",unitDisplay:"long",notation:"scientific"}).format(1e4)` | `"1E4 bits"` | `"1E4 bit"` |
| `NumberFormat("en",{notation:"compact",min/maxSignificantDigits:3,min/maxFractionDigits:2,roundingPriority:"morePrecision"}).format(1e8)` | `"100.00M"` | `"100M"` (hypothesis: `roundingPriority` is ignored; not read in the code) |
| `NumberFormat.supportedLocalesOf(["es"]).length` | 1 | hypothesis: 0 (bare has `en` data only; not run) |
| `"formatRange" in DateTimeFormat.prototype` | true | false |

The other `DateTimeFormat` probes (`dayPeriod` part, `dateStyle` with `hour` throws, `resolvedOptions().dateStyle`) give the same answer as a browser.

### Experiments that separate the causes

All on corpus `virtual-load` (and a copy with the modern bundle where noted), `BARE_SEED=1 BARE_CLOCK=virtual`, `perf stat -e instructions:u`. Temporary patches only (`sw-only.patch`, `sw-intl3.patch` in the scratch directory).

| Case | Instructions | Peak RSS | Runs |
| --- | ---: | ---: | ---: |
| main (`SharedWorker` absent) | 47,007.9 M (spread 0.002%) | 749 MiB | 4 |
| `DateTimeFormat.prototype.formatRange` defined | 23,949.7 M | 436 MiB | 2 |
| and the 3 `NumberFormat` probes made true | 4,148.6 M | 115 MiB | 2 |
| `SharedWorker` defined (the page serves the modern bundle) | 2,681.9 M (spread 0.0%) | 84 MiB | 4 |

- The matcher is 91.2% of the journey (47,007.9 − 4,148.6 = 42,859 M; 1,122 calls; 38.2 M each).
- The ES5 path without the matcher still costs 1.47 G more than the modern path (4,149 against 2,682 M).
- With `SharedWorker`, the page asks for 7 files (no polyfill, one new file: the modern `browsePostings`), its final DOM has 952 elements (955 on the ES5 path), 6 script elements (9), the same 233 links, and the same console notices. Both paths end at `readyState === "complete"`.

## What Chrome does on the same page

Chrome 155 headless, 3 runs each, through CDP (`chrome/cdp.mjs`; live site, normal user agent, 3 s after `load`):

| | modern path (as shipped) | `SharedWorker` removed before the page runs |
| --- | ---: | ---: |
| `cl.oldBrowser` | false | true |
| Requests; encoded bytes | 10; 501,655 | 13; 889,285 |
| Script duration (`Performance.getMetrics`) | 100, 103, 120 ms | 210, 210, 213 ms |
| Task duration | 199, 202, 323 ms | 310, 314, 316 ms |
| JS heap used | 9.2 MB | 10.5 to 11.1 MB |

- A browser never takes the ES5 path on this page: `SharedWorker` exists in Chrome, Firefox and Safari. Chrome sends the modern bundle.
- Even when Chrome is forced to the ES5 path, it needs 2.0 times the script time, not 17.5 times the instructions (47,008 against 2,682 M in bare). The reason: its native `Intl` passes every probe, so `shouldPolyfill` is false and the matcher never runs. The formatjs page says the polyfill is meant to load only "when `shouldPolyfill` reports an unsupported locale" (formatjs.github.io/docs/polyfills/intl-numberformat, read 2026-11-14).
- Chrome's JS heap stays at about 10 MB. bare's reaches 445 MiB. V8 starts concurrent marking when the heap nears a computed limit, and does GC work in slices between stretches of JavaScript (v8.dev/blog/trash-talk, read 2026-11-14; the page does not say whether a single call can be interrupted: I did not verify it).
- Chrome's DOM has 443 elements and 152 links; bare's has 952 and 233. Hypothesis: the page lays out another list for the small default window of headless Chrome. I did not check it.
- bare with the modern path: 2.68 G instructions, 0.39 s CPU on a loaded machine. Chrome: 0.20 s of main-thread tasks. The ratio is about 2, not about 25.

## Where the 749 MiB comes from

Measured with a heap probe at every 16 MiB of growth (temporary patch):

- The JS heap grows in a straight line: 7.3 MiB for each 1 G of instructions, 3.15 M objects at 432 MiB. At that point RSS is 702 MiB (malloc in use 299 MiB; large tables mapped with `mmap` 513 MiB). That is about 234 B of RSS for each object (95 B malloc, 163 B in the object slab and its side tables; the split is a derivation from `mallinfo2`, not from a per-structure count).
- There is exactly one collection, at the end of the task. It frees 3.19 M objects (433 MiB). Live afterwards: 12 MiB, 65,393 objects. So 97% of the heap was garbage.
- Why no earlier collection: `Interp::gc_due()` is `depth == 0 && stmt_frames == 0 && used_bytes > …`, and `Interp::collect` returns an error "collect during a call". The `load` listener is one call chain of 45 G, so the interpreter is never at depth 0.
- The matcher allocates for each `F` call about 2,800 objects: two `Intl.Locale`, the rule arrays of `z()`, the `map`/`reduce` copies (`E(E([],a,!0),n,!0)` is a spread copy).

## Subsystem split

`perf record -e instructions:u -c 1000000` on main (46,997 samples) and `-c 200000` on the modern path (13,119 samples); symbols grouped by crate and module (`classify.py`; 4.6% of the main samples and 7.7% of the modern ones are unresolved addresses in libc and are listed as "other"). Instruction counts repeat to 0.002% (3 `perf stat` runs), so one `perf record` each is enough for shares.

| Subsystem | main (ES5 path) | `SharedWorker` (modern path) |
| --- | ---: | ---: |
| JS interpreter (`eval`) | 66.6% | 49.5% |
| JS heap and object model (`heap`, shapes, property maps) | 19.1% | 18.0% |
| other (libc, unresolved) | 4.6% | 7.7% |
| JS builtins (`call_ext`, array ops, string ops) | 4.0% | 1.7% |
| Allocator (malloc and free) | 3.4% | 3.3% |
| JS parse (lexer, parser) | 1.4% | 8.2% |
| JS regexp | 0.4% | 2.0% |
| JS GC | 0.1% | 1.6% |
| Layout | 0.2% | 2.4% |
| Style | 0.1% | 2.0% |
| DOM bindings | 0.1% | 1.5% |
| Network, TLS, inflate | 0.1% | 0.9% |
| HTML parse | 0.05% | 0.7% |
| CSS parse | 0.04% | 0.5% |

Top symbols on main: `eval_expr_inner` 13.2%, `link_inner` 11.4%, `Heap::lookup` 5.0%, `exec_simple` 3.3%, `machine` 3.0%, `link_base` 2.7%, `ordinary_define` 2.5%. (Sampled on the first `perf record`, 5 M instructions per sample.) Network, HTML, CSS, style, layout, paint and DOM bindings together are 0.6% on main: this journey is a JS cost.

## Current design, with complexity

- `cl.oldBrowser` is a feature test the page makes once. bare fails it for one missing global.
- Best-fit matcher: for `L` available locales and `K` rules, one `ResolveLocale` costs `O(L × 3 × K × cost(z))`. Here `L = 561`, `K ≈ 1,125`, so about 3.8 M calls of `z()` for each module. The page runs it in 2 modules. It is quadratic in the data and designed for browsers without `Intl`.
- GC: stop-the-world mark and sweep, only at call depth 0 (`gc_due`), checked between loader rounds (`collect_every`). Cost `O(live)` for mark and `O(slots)` for sweep. Garbage made inside one call is not freed until the call returns. Peak heap = live + all garbage of the longest call.
- Object memory: one slot for each object in a slab, plus property storage on the malloc heap (`heap.rs`, `heap_shape.rs`).

A limit that protects a slow algorithm: none fired here. The `max_steps` budget did not stop a 45 G task (the console has no "Script aborted" line). I did not read how steps are counted.

## Options

Gains are on this journey and on criterion 2. Arithmetic for criterion 2: total of main 119,049 M, baseline 138,272 M (both from the #542 table), `craigslist-home` on main 47,008 M. The baseline binary has no `SharedWorker`, so on a corpus that holds both bundles it still takes the ES5 path (48,250 M): the baseline total does not change.

### Option 1: define `SharedWorker` as a worker that fails to start (recommended)

Make the page see a browser that has `SharedWorker`, and let the page choose the modern bundle, as it does in every browser.

- Data structures: reuse `WorkerHost` (`IdMap<ObjId, bool>` of live workers, the lazy global accessor, the `made` prototype and constructor roots). `SharedWorker` is a second interface with the same host record. It has a `port` property: one `MessagePort` from the code of `MessageChannel` (`browser_scheduler.rs`: `message_channel_ctor`, `Run::Deliver`), never started with a message.
- Algorithm: the constructor reads its arguments in WebIDL order (URL, then `name` string or options with `type`, `name`, `credentials`), parses the URL as `Worker` does, and queues `Task::Worker(obj)`. The task fires `error` at the object. This is the HTML Standard step "fire an event named `error` at worker" when the script cannot load (html.spec.whatwg.org, processing model of "run a worker", read 2026-11-14; I did not read the constructor section: the page was cut off). `Worker` already does this.
- Complexity: `O(1)` for the constructor; no cost in a realm that never reads the global (the accessor is lazy). The page here reads it once: one prototype and one constructor, about 2 objects.
- Memory layout: no new table; 1 more `bool` in `objs` for each live shared worker; the `MAX_WORKERS = 64` cap applies to both.
- Expected gain:
  - `craigslist-home`: 47,007.9 → 2,681.9 M (-94.3%); peak RSS 749 → 84 MiB (-88.8%). Measured with a stub function, 4 runs, spread 0.0%.
  - Criterion 2: (119,049 − 47,008 + 2,682) / 138,272 = 74,723 / 138,272 = **54.0%** (target 80%: met). Without `craigslist-home`: 80.0%, unchanged.
  - Sum of peak RSS of the 47 journeys: 2,496 → about 1,831 MiB (-26.6%); against the baseline's 2,754 MiB: 66.5%.
  - Criterion 4 and 6: not affected.
- What it needs: `virtual-load` needs the one file of the modern bundle (360 KB gz; a `fill`). Both binaries then run without a miss. Change the test in `browser_worker_tests.rs:40` (it asserts `typeof SharedWorker` is `undefined`) and add one in `docs/decisions.md`.
- Risk: a page that really uses `SharedWorker` gets an `error` event instead of a missing global. That is the same as for `Worker` today. A page that tests `SharedWorker` to pick a code path now picks the path that browsers get. In the corpus, only `craigslist-home` (the test above) and `substack-home` (Sentry's list of event targets, which wraps `SharedWorker.prototype` when it exists) name it. I did not run `substack-home` with it.
- Honest limit: this gain is a change of the page that we load, not a faster engine. The engine is as slow on the ES5 path as before. If a page still takes a polyfill path (a 4.1 G floor here), the engine cost shows again.

### Option 2: make `Intl` pass the formatjs probes

Remove the matcher by making `shouldPolyfill` false for `NumberFormat` and `DateTimeFormat`, as in Chrome.

- Data structures: locale data tables for more than `en` (the `es` probe), plural rules for unit names in long form (`bits`), `roundingPriority` in the number rounding step, `DateTimeFormat.prototype.formatRange` and `formatRangeToParts`. Static data in the binary, not on the heap.
- Algorithm: table lookups, `O(1)` for each format call.
- Memory layout: read-only tables. ICU locale data for the common languages is megabytes; a subset for the probes is smaller, but a subset that says "supported" for `es` and gives English output is a wrong answer.
- Expected gain: tested with the probes faked (`sw-intl3.patch`): 4,148.6 M and 115 MiB (-91.2%). Criterion 2: (119,049 − 47,008 + 4,149) / 138,272 = 55.1%. That is within 1 point of Option 1, with the work of an `Intl` implementation.
- It helps every page that loads formatjs on any path (`nodejs-org-home` ships it in a Next chunk; not profiled). It does not fix the 1.47 G of ES5 code, and it does not give `SharedWorker`.
- Not recommended as the first step: the size of the work (data, plural rules, a range formatter) is far larger than Option 1 for the same number here. Keep it as a separate topic.

### Option 3: free garbage inside a call

The page makes 433 MiB of garbage in one call. A collection at a safe point inside a call keeps the heap near live size (12 MiB live here) plus the garbage between two collections.

- Data structures: a root map of every `Value` held by Rust code of the interpreter during a call (the tree walker keeps values in Rust locals and in `stk`); or a shadow stack of roots; or a conservative scan of the Rust stack.
- Algorithm: when `used_bytes` passes the trigger at an allocation, mark and sweep with the current frames as roots. Mark is `O(live)`; sweep `O(slots)`.
- Complexity of the work: high. `Interp::collect` refuses a call today for this reason. Every native that holds an `ObjId` across an allocation would need a root or a guarantee that no collection runs inside it.
- Expected gain: the instruction count does not fall (plus the marking cost). Peak RSS of this journey on the ES5 path falls from 749 MiB to a value that I did not measure (hypothesis: under 150 MiB at a trigger of 7/4 of live, from the 12 MiB live figure). With Option 1 the journey peaks at 84 MiB and Option 3 gives nothing here.
- It matters for another page that runs one long call. It belongs to the GC owner.

### Option 4: make the matcher cheap in the engine

38 M instructions for each `F` call. To make 1,122 calls cost 1 G, each call must cost 1 M: a 38 times gain on a loop of array `map`/`reduce`, property reads and string slices. The interpreter work in the table above (66.6% `eval`, 19.1% heap) does not give that without a different execution model. The bytecode lane was stopped at 2.49 times (`docs/progress.md`, R1). Rejected: it would also tune the engine to one polyfill that no browser runs.

## Comparison

| | `craigslist-home` M | Peak RSS MiB | Criterion 2 | Work |
| --- | ---: | ---: | ---: | --- |
| main | 47,008 | 749 | 86.1% | |
| 1. `SharedWorker` as a failing worker | 2,682 | 84 | 54.0% | one interface, one test, one corpus fill |
| 2. `Intl` passes the probes | 4,149 | 115 | 55.1% | locale data, plural rules, range formatter |
| 3. GC inside a call | 47,008 (unchanged) | hypothesis: under 150 | 86.1% | rooting in the whole interpreter |
| 4. A faster matcher | would need 38 times | | | not feasible |

## Recommendation

Build Option 1. It removes the work (the page does not load the polyfills), it is the same as what Chrome receives, and it is small. Then fill `virtual-load` with the modern bundle and measure the 47 journeys again against the frozen baseline.

Say clearly in `docs/progress.md` that the criterion 2 gain is a change of the loaded page. Report also the total without `craigslist-home` (80.0%), because the other large journeys (`react-docs` 12.6 G, `substack-home` 11.6 G) have not moved.

Keep Options 2 and 3 as topics, not as part of this change. Option 2 for any page that loads formatjs. Option 3 for a page that runs one long call (it would also lower this journey on the ES5 path).

## Where it touches the work of the other agents

- Class atoms and the selector arena (`crates/css`, style): none. Style is 0.1% of this journey on main and 2.0% on the modern path. The modern path has 952 elements.
- Freeing dead DOM trees and a GC trigger (DOM arena, GC): the journey has no dead DOM tree that matters (the DOM has 955 elements). A GC trigger that is checked at depth 0 (as `gc_due` is) cannot fire here: the 45 G run is one call chain. Only Option 3 changes that. With Option 1 the journey runs 2.7 G and the question does not arise.
- Option 1 changes `crates/bare/src/browser_worker.rs`, its test file, and the install hook of `Worker` in `browser.rs`. These files are not in the two areas above.

## Not measured

- Wall time on an idle machine (the CPU times come from a loaded machine).
- The 47 journeys with Option 1 (only `craigslist-home` was run with a stub; the other 46 are not measured with `SharedWorker` defined).
- A live run of `craigslist-home` in bare. The modern bundle came from the live site once (the fill).
- Windows and macOS.
- The reason for the 443 against 952 elements.
- `nodejs-org-home` (5.8 G) loads formatjs: not profiled.
- `substack-home` with `SharedWorker` defined.
- The exact split of the 749 MiB by structure (only malloc and mmap totals).

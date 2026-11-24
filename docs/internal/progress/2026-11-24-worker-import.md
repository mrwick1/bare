# importScripts in a worker, and the console of a worker (W1, step 2)

Date: 2026-11-24. Base: main ad943606. Design: `../design-workers.md`, W1 step 2 and decision 7.2. Follows `2026-11-24-worker-run.md`.

## Change

- The worker global gets `importScripts(urls…)`. All the URLs are parsed against the worker URL first; a bad one is a `SyntaxError` before any download. Then each one downloads in order with a blocking call and runs in the worker global.
- The download uses `resources::fetch` with the client of the worker page. These are the rules of a classic script element: redirects, 2xx, a JavaScript type, the size cap. A `data:` URL is decoded. A failed download is a `NetworkError`, and the later URLs do not load. A script that throws stops the call with its exception.
- Vex: `run_script` inside a host call is now a nested call, as `eval_global` is. Before, it was `Unsupported("run_script during a call")`. A top-level `let` or `const` of an imported script is in the global scope (a test checks it).
- The console and the uncaught errors of a worker already reach the page with the worker URL as their source (`take_child_output`). A new test checks it.

## Teams (signed out, `teams.steps`, 1 run)

- Before: the page fetched the two outer worker scripts and no inner chunk.
- After: the worker fetches all 6 inner chunks (`precompiled-web-worker-inner~0` to `~4` and `-defer`), 56 requests in total. The console has no worker error. The page still stays on the spinner. The page console still has 3 lines of `Unhandled error/rejection {}`. The next blockers are not known yet (hypothesis: `navigator.serviceWorker`, `caches`).

## Cost gate

- `interp_load` (loopfn, calls, fib, closures, objects): all within ±0.002%.
- `cost_matrix.sh -g 'js2_'`: 4 rows, mean +0.001%, 0 over the rule.

## Tests

- New: `browser_worker_import_tests.rs` (5 tests): order and relative URLs, with a top-level `const`; `NetworkError` for a 404 and for an HTML type, and the later URLs do not load; `SyntaxError` before any download; a `data:` URL and a throw that reaches the caller; the console and an uncaught error of a worker reach the page.
- `t`: 1659 passed, 1 failed: `downloads::run::tests::a_changed_file_starts_again_from_zero`. It passes alone 3 times out of 3 (flaky under load; this change does not touch downloads).

## Not tested

- The old engine (`run_script` there is not changed).
- An `importScripts` call from a module worker (the spec throws `TypeError`; here it runs).
- Wall time of the blocking downloads.

# Dedicated workers run (W1, step 1)

Date: 2026-11-24. Base: main 1de4f51e. Design: `../design-workers.md`, W1 step 1.

## Change

- A dedicated `Worker` in a page with a driver (a session page) is now a child page of the kind worker (`browser_child.rs`, `Of::Worker`). It has its own `Browser` and heap on the page thread. It gets turns like a frame.
- The document of a worker page is one script element with the worker URL as `src`. So the rules of a script load apply (MIME type, size cap). The document URL is the script URL.
- A `data:` worker gets an empty document. Its decoded text runs when the page is made, because a script element does not take a `data:` URL in bare.
- `worker.postMessage(v, transfer)` clones into the outbox for the worker. A message that is posted before the worker page runs waits for it. A frame still drops such a message.
- The worker global gets `postMessage(v, transfer)` and `close()`. A message from the worker is a `message` event at the `Worker` object, with origin `""` and source `null`.
- `terminate()` and `close()` end the worker page. A running worker is a GC root of the page.
- A worker script that does not load fires `error` at the `Worker` object (the old task).
- A page with no driver (`Browser::new` and `run_script` only) keeps the old stub. So the tests in `browser_worker_tests.rs` do not change.

## Teams (signed out, `teams.steps`, 1 run)

- Before: `ExperienceLoader: Error in worker`. The worker scripts did not run.
- After: the console has no worker error. The page fetches `precompiled-web-worker-*.js` and `precompiled-telemetry-web-worker-*.js`. The page still stays on the spinner. It fetches no inner worker chunks. Hypothesis: the worker stops at `importScripts` (step 2). The console of a worker page does not reach the drive result, so this is not verified.

## Cost gate

`cost_matrix.sh -a -g 'scheduler_load|navigation_load|web_apis_load|browser_load'`: 39 rows, mean −0.004%. Over the rule under both allocators: 0. `navigation_load url 1` is +0.551% under glibc and +0.425% under jemalloc (noise candidate). `web_apis_load segmenter` fails to run on main too (same error).

## Tests

- New: `browser_worker_run_tests.rs` (7 tests): a same-origin worker answers with no origin and no source; a `data:` worker posts first; message order both ways; a script that does not load fires `error` and the page settles; `terminate` stops the worker and its timers; `close` delivers what was posted before it; a function in a message throws `DataCloneError`.
- `t`: all pass (lib run: 1655 passed, 101 ignored).

## Not tested

- The old engine.
- A worker that navigates (`location` set in the worker). It loads a document as a frame does.
- Uncaught errors in a worker: they go to the reports of the page, not to an `error` event at the `Worker` object.
- A nested worker (a worker that makes a worker).
- Peak memory and instructions of the Teams run.

# 2026-11-26: MessagePort has its toString tag

Branch `t3/fix-messageport-worker-cloning`, merged with `origin/main` b486ebe9.

## Why

The WhatsApp Web worker logs "MessagePort object could not be cloned.". The cause in bare is a hypothesis: the bundle was not read.

## Search

Every standard transfer of a port works in bare, in a worker of the page and in a worker on its own thread (cross-origin isolated):

- page to worker and worker to page, both ways over the pair;
- a port inside an object or an array, with an `ArrayBuffer`, and with the `{ transfer }` form;
- a port that goes back, both ends of one pair, a port relayed over another port;
- a `MessageChannel` made in the worker, `structuredClone` with `transfer`, a worker made inside a worker.

One gap: `Object.prototype.toString.call(port)` gave `[object EventTarget]`. Chrome gives `[object MessagePort]`. `MessageChannel` gave `[object Object]`. Code that finds the ports of a message by their tag leaves the port out of the transfer list. Then `postMessage` throws "MessagePort object could not be cloned.", the text of the log.

## What changed

- `crates/bare/src/browser_scheduler.rs`: `install_scheduler` puts `Symbol.toStringTag` on `MessagePort.prototype` and `MessageChannel.prototype`.

## Tests

- New `crates/bare/src/browser_worker_port_transfer_tests.rs`, 6 tests. `a_transfer_list_built_from_the_to_string_tag_moves_the_port` failed before the change with "MessagePort object could not be cloned." and passes after it. The other 5 tests pass before and after. They keep the transfers above in place (2 of them on a worker thread).
- `browser_clone_tests.rs`: the row `transfer port` expects `[object MessagePort]`. The test is ignored for other rows.
- `t` passed: `browser_scheduler.rs` (scheduler, message queue GC, idle and related tests) and the new file.

## Cost rows

Machine archMachine, load average about 4 to 5. Base: release build of `origin/main` b486ebe9. New: this branch on b486ebe9. `tools/js/cost_matrix.sh -J -g 'scheduler_load (messages|mixed)'`, minimum of 3 runs.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | --- | --- | --- | --- | --- | --- |
| `scheduler_load messages 1` | 17390686 | 17393314 | +0.015% | 24475588 | 24479019 | +0.014% |
| `scheduler_load messages 10` | 131666841 | 131680224 | +0.010% | 141157192 | 141173887 | +0.012% |
| `scheduler_load mixed 1` | 28990118 | 28993763 | +0.013% | 35924887 | 35927414 | +0.007% |
| `scheduler_load mixed 10` | 245752903 | 245653614 | -0.040% | 253074390 | 253095222 | +0.008% |

No row is over +0.5%. Vex did not change, so test262 did not run.

## Gaps found, not fixed

- `new MessageEvent(type)` throws `TypeError` in bare. `MessageEvent.prototype` has no tag.

## Not tested

The WhatsApp worker, Chrome itself, Windows, the full suite, the full cost matrix.

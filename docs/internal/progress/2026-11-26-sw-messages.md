# Service worker messages (design-service-workers.md, step 4, design S2)

## Change

- A message from a service worker to a page (`client.postMessage`) is a `MessageEvent` task on the
  `ServiceWorkerContainer` of the page. The host queues it (`queue_delivered`), because script cannot construct a
  `MessageEvent`. The event has `origin` and `source` (the `ServiceWorker` object of the sender), and it carries
  transferred `MessagePort` objects.
- `browser_sw.rs` `sync_item`: the `Msg` item asks the shim for the container and the worker object, then queues the event.
- `shims/sw.js`: the page `sync("msg")` branch returns `[container, worker]`.

## Cost when no registration exists

No change. The new code runs only for a page that is a service worker client and has a message in its inbox.

## Tests

- `crates/bare/src/browser_sw_message_tests.rs` (new, 5 tests): a reply with `source` and `type`, a `MessagePort` that
  crosses to the worker and back, `clients.matchAll()` and `clients.get()`, a message that starts a stopped worker, and
  `postMessage()` with no argument throws a `TypeError`.
- Before the change, 3 of 5 fail: the reply path threw in the shim. After: 5 passed. `t` on the changed files passes.

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load|browser_load'`, base `9990185e` (origin/main) against this branch,
archMachine, glibc. 24 rows, none over +0.5% under one allocator. Mean change +0.041%.

## Not tested

- Two tabs of one session that use one registration.
- Windows.

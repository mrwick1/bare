# Service worker registration and lifecycle (design-service-workers.md, steps 2 and 3, design S2)

## Change

- `sw_hub.rs` (new): the service workers of a session. It holds the registrations (scope, `updateViaCache`, the installing,
  waiting and active versions with their script text), the jobs, the clients and the events. It holds no heap object. It lives
  in the session `WebStorage`, so the tabs of a session share it.
- The profile keeps each registration that has an active version in `<profile>/sw.bin` (magic `baresw01`, same file rules as
  `permissions.bin`). `StorageState::service_workers` carries it. A new session imports it as an activated version, so the
  first load of a page in its scope is controlled with no network request.
- `shims/sw.js` (new): `ServiceWorkerContainer`, `ServiceWorkerRegistration` and `ServiceWorker` of a page, and the worker
  side: `ExtendableEvent`, `ExtendableMessageEvent`, `Client`, `WindowClient`, `Clients`, `self.registration`,
  `self.clients`, `self.serviceWorker`, `skipWaiting()` and the `on*` handlers of the worker global. It replaces the stub
  container of `browser_web.rs` (`register` rejected with `NotSupportedError`).
- `browser_sw.rs` (new): the native ops of the shim, the register job (the URL checks and the messages of Chrome, a script
  download with `Service-Worker: script`, no redirect and no HTTP cache), the driver turn of a client page (`sw_turn`) and the
  event dispatch into a worker page.
- A running service worker is a worker page `Of::Service` (`browser_child.rs`) in the page that first had work for it. It
  starts on an event and stops after 30 s with no event in flight (design S2). The global is
  `ServiceWorkerGlobalScope` (`browser_worker::install_scope_named`).
- The lifecycle: the script runs first (a throw rejects the job with a `TypeError`). Then the job resolves, `updatefound`
  fires, and `install` runs. A good install makes the version waiting. It becomes active at once when nothing holds it
  (no active version, an active version that controls no client, or `skipWaiting()`), then `activate` runs.
  `clients.claim()` sets the controller of each client in scope and fires `controllerchange`. `ready` resolves with the
  registration of the page when it has an active worker.

## Cost when no registration exists

A page becomes a client only when it reads `navigator.serviceWorker` or a service worker global, or when a registration
controls it at load (`browser_sw::navigated`: one length check when the session has no registration). A page that is not a
client has no extra work in a driver turn: `Kids::any` reads one more `Cell<bool>`. No per-request work exists yet (fetch
interception is step 7).

## Tests

- `crates/bare/src/browser_sw_register_tests.rs` (new, 5 tests): resolve order and the state order, the
  `Service-Worker: script` request header, `getRegistrations`, `getRegistration`, `unregister`, six rejections with the
  messages of Chrome (404, MIME type, max scope, cross origin, script throw, bad `updateViaCache`),
  `Service-Worker-Allowed`, a page with no service worker use is no client.
- `crates/bare/src/browser_sw_lifecycle_tests.rs` (new, 6 tests): `claim` fires `controllerchange`, a page loaded under an
  active worker is controlled and `ready`, a new version waits while the old one controls a page, `skipWaiting`, a failed
  install makes the worker redundant, the idle stop after 30 s with the virtual clock, the profile round trip.
- Before the change, all 11 fail: `register` rejected with `NotSupportedError`. After: 11 passed.
- `browser_sw_tests.rs`: the `register` check moved to the new files. `t` on the changed files: `browser_tests`,
  `browser_child_tests`, `browser_sw_tests`, `profile_tests`, `profile_permissions_tests`, `browser_worker_scope_tests`,
  `browser_worker_run_tests` pass.

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load|browser_load'`, base `e6fe18bf` (origin/main) against this branch
(merged with it), archMachine, 3 runs per row (minimum of `instructions:u`), glibc. 24 rows, none over +0.5%. Largest:
`browser_load canvasidle 10` +0.336%. Mean change -0.062%. No row registers a service worker. The design names `worker_*` rows;
`cost_rows.txt` has none.

## Not done in this PR

- Step 1 (the cost of WhatsApp's `/sw.js` in a worker page) is not measured yet. It comes after the notification steps, with this
  runner.

## Known deviations

- A worker runs on the page thread, also in a cross-origin isolated page.
- A running worker belongs to the page that started it. Another tab of the session that sends an event while it runs queues
  the event for that worker. A tab that closes stops the workers that it hosts.
- `startMessages()` does nothing: a message is delivered at once.
- `importScripts` in a service worker loads through the HTTP cache at each start of the worker.
- `navigationPreload` is a manager that never preloads.
- A module service worker runs as an inline module script in its worker page.
- `navigator.serviceWorker` exists also in a page that is not a secure context (Chrome hides it there).
- `bare-drive` state files (`crates/drive/src/state.rs`) do not carry `service_workers` yet.

## Not tested

- WhatsApp live. The notification steps (4 and 5) come first.
- Two tabs of one session that use one registration.
- Windows.

# Native `navigator.serviceWorker`

Date: 2026-11-24. Base: main 3b9aee14.

## Change

- `navigator.serviceWorker` is a getter on `Navigator.prototype` (`crates/bare/src/browser_web.rs`). It gives the one `ServiceWorkerContainer` of the realm.
- The container is an `EventTarget`. It has `getRegistration` (resolves `undefined`), `getRegistrations` (resolves `[]`), `register` (rejects with `NotSupportedError`), `startMessages`, `controller` (`null`) and `ready` (a promise that never settles).
- The globals `ServiceWorkerContainer`, `ServiceWorkerRegistration` and `ServiceWorker` are illegal constructors. Their prototypes extend `EventTarget.prototype`.
- Design source: Ladybird (`Libraries/LibWeb/ServiceWorker/ServiceWorkerContainer.cpp`, commit acdaccb). Ladybird exposes the container and rejects `register`, because it does not run service worker scripts.
- Why: Teams calls `navigator.serviceWorker.getRegistration()` without a check in `startWebClientUpdateCheck`. bare threw `TypeError` 3 times per run.

## Cost

- Nothing is made at install. The container and its interfaces are made at the first read of `navigator.serviceWorker` or of one of the 3 global names (`browser_handlers::lazy_key`).
- `navigator` has one more getter function. It is made once, with the other `Navigator` getters.

## Tests

- New: `browser_sw_tests.rs` (2 tests).
- `t`: all pass.

## Not tested

- The cost gate rows were not run.
- test262 counts: not run. The change touches no Vex code.

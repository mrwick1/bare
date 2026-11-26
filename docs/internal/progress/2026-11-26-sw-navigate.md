# Navigation fetch events (design-service-workers.md, step 8)

## Change

- Before a document load, the session looks for a registration whose scope matches the URL, with an active worker that has a
  `fetch` listener (`Hub::nav_ver`). The check is one length check when the session has no registration (`sw_document`).
- A match: the session makes a blank page of the target URL as the host of the worker (`sw_navigate`, `#[cold]`). The new
  document does not exist yet, so it cannot host the worker. The hub queues a `fetch` event for client 0 (no client). The host
  page runs until it settles. Then the session takes the reply (`Hub::take_nav`) and ends the host page.
- The request: method GET (POST with the body of a form), mode `navigate`, redirect `manual`, credentials `include`, the
  `Accept` and `Upgrade-Insecure-Requests` headers of a navigation. `clientId` is empty, as in Chrome.
- `respondWith` gives the document: the body decodes by its `Content-Type`, as a network document does. A response with no URL
  takes the request URL. No reply goes to the network. A rejected reply fails the load with "network error".
- The live path (`start`) runs the worker on the session thread and gives the document to the loader at once.

## Cost when the session has no registration

One `has_regs` check per document load. No other change.

## Tests

- `crates/bare/src/browser_sw_navigate_tests.rs` (new, 4 tests): the worker answers a navigation (the request fields, and the new
  page is controlled), no reply goes to the network, a rejected reply fails the load, and the first load of a later run goes to
  the worker.
- With the navigation hook turned off, 3 of the 4 tests fail (the network test passes in both cases). With the hook: 4 passed.
  All `browser_sw_` tests and `t` pass.

## Cost rows

`tools/js/cost_matrix.sh -g 'browser_load|session_load|navigation_load|loader_load'`, base `d7c9ae65` (origin/main) against this
branch (merged with it), archMachine, glibc. 42 rows, none over +0.5%. Mean change +0.010%.

## Known deviations

- Frame loads (`browser_child.rs`) do not go to the worker.
- The host page is a client of the hub while it runs: `clients.matchAll({includeUncontrolled: true})` in the worker can see it.
- The worker starts again in the new document for its next event (one more start per controlled navigation).
- `resultingClientId` and `replacesClientId` are empty. No navigation preload.
- A redirect response from the worker (`Response.redirect`) is not followed. The body of the redirect response is the document.

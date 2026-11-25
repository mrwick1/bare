# Network: send `Referer` on subresources

## Problem

Teams avatars (`profilepicturev2`) returned 401. The `authtoken` cookie was sent and intact. bare sent no `Referer`. A probe that added `Referer` alone turned 64 × 401 into 64 × 200. A probe that added the `Sec-Fetch-*` headers alone did not help.

## Change

- New `resources::referrer(doc, target)` gives the `Referer` value of Chrome's default policy `strict-origin-when-cross-origin`:
  - same origin: the full document URL (the origin only when the URL is longer than 4096 bytes);
  - https to http: no `Referer`;
  - another origin: the origin plus `/`.
- Scripts and styles (`resources::fetch`), pictures (`images::load`), `fetch` and XHR (`browser_fetch.rs`, new field `Cors::page`) send it. Each redirect hop computes it again for its own target.
- A script still cannot set `Referer`. The value comes from the page.

## Tests

- New: `crates/bare/src/browser_referer_tests.rs` (3 tests: the policy, `fetch` and XHR to the same origin and to another origin, a script and a picture). All 3 fail when `referrer` returns `None`.
- `fetch_only_queues_and_the_network_starts_in_the_driver` and `managed_and_sensitive_headers_cannot_be_set_by_script` now expect the page URL as `Referer`.
- `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run, saved session)

| | Before | After |
|---|---|---|
| `profilepicturev2` | 64 × 401 | 57 × 200, 0 × 401 |

## Cost

No cost rows ran. The change adds one URL compare and one string per request.

## Not done

- A navigation sends no `Referer`. `document.referrer` stays "".
- bare does not parse a referrer policy (header, `<meta>` or attribute). The default policy applies always.
- A picture from CSS sends the document URL, not the URL of the stylesheet.
- Picture requests wait 15 to 90 s in the queue on Teams. Not changed here.

# 2026-11-25: Fetch Metadata on subresource requests

Branch `subresource-fetch-metadata`.

## Change

- `resources::fetch` takes the URL of the document that asks. To a potentially trustworthy URL, the request sends
  `Sec-Fetch-Site` (the least trusted relation over the redirects, `browser_fetch::metadata_site`), `Sec-Fetch-Mode`
  (`cors` for a cross-origin module of an isolated page, else `no-cors`) and `Sec-Fetch-Dest` (`script`, `style` or `empty`).
- The loader (`Net`) keeps the origin of the page. `importScripts` gives the URL of the worker document.
- Cause: WhatsApp link attempt 4. The CDN answers 400 to a worker script request without `Sec-Fetch-Site`, and keeps that
  answer for the URL. See `docs/internal/design-whatsapp-bringup.md`.
- Limit: a worker script loads as a `<script>` of a worker document, so its `Sec-Fetch-Dest` is `script`, not `worker`.
  The mode of a classic worker script is `no-cors`, not `same-origin`.

## Tests

- New `crates/bare/src/resources_fetch_metadata_tests.rs`: a script and a stylesheet of a page send the three headers. It
  failed before the fix (all three empty).
- `t`: all pass.
- `potentially_trustworthy` and `metadata_site` moved from `browser_fetch.rs` to `resources.rs` (`browser_fetch` re-exports them):
  the `resources_load` example builds `resources.rs` alone.

## Cost

archMachine. Base = main a2d09d49, new = this branch. Minimum of 3 runs (`instr.sh`).

- `cost_matrix.sh -g 'browser_load|session_load|web_apis_load|navigation_load frames|resources_load|fetch'`: 40 rows, none over
  +0.5%. Mean glibc +0.001%. `web_apis_load segmenter 1` and `10` fail to run, on main too.

## Live

- WhatsApp link attempt 5 with this branch: the worker `init_script` requests got no 400, and the backend worker loaded. The
  link still failed later (`PN-less setup requires a LID device jid`), see `design-whatsapp-bringup.md`.

## Not tested

- A cross-site or same-site subresource (only same-origin is in the test). A redirect chain. Windows.

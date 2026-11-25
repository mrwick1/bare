# Parsed data: scripts and nonce (WhatsApp bring-up step 1)

Record: `docs/internal/design-whatsapp-bringup.md`.

## What changed

- A classic `<script>` that the parser finds with a `data:` `src` now runs. Before, bare skipped it with no message
  (`Skip::BadUrl`). It is `Source::Data` (`resources.rs`). The loader decodes the URL when the script's turn comes. It uses
  the timing of an external script (blocking, `defer` or `async`) and fires `load`, or `error` for a URL that does not decode
  (`browser_loader.rs`, `run_parsed`). A `data:` module and a `data:` style sheet stay skipped.
- `nonce` on `HTMLElement` and `SVGElement` reflects the `nonce` attribute (`browser_element.rs`). Before, it was `undefined`.

## Live effect

`docs/internal/whatsapp/whatsapp-qr.steps`, archMachine, 1 run each. Before: the page was idle after 1.1 s on the splash
screen, 11 requests, no console error. After: 22 requests, the page is busy at 90 s, and the next blocker shows
(`WAWeb-curve25519-donna`, see the record).

## Tests

- `crates/bare/src/browser_loader_data_tests.rs` (2 tests: order with blocking, `defer` and inline; `load` and `error`).
- `crates/bare/src/browser_element_nonce_tests.rs` (2 tests).
- `t`: `browser_element_nonce_tests`, `browser_loader_data_tests`, `browser_child_tests`, `browser_element_tests`,
  `browser_loader::` all pass. `resources::` (30 tests) passes; the discovery test now expects `Source::Data`.

## Cost rows

`cost_matrix.sh -g 'resources_load|loader_load|scheduler_load|browser_load'`, base = merge base 7e49eeac, archMachine,
3 runs per row (minimum), glibc. 31 rows, none over +0.5%. Largest: `resources_load dups 10` +0.386%,
`resources_load dups 1` +0.347%, `scheduler_load timers 10` +0.302%. Mean +0.043%.

## Not tested

- A `data:` script with a `charset` other than UTF-8 (the text is decoded as UTF-8).
- A `data:` script that a script inserts (this change covers parsed scripts only).
- Windows.

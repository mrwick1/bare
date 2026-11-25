# ResizeObserver: device-pixel-content-box

## Change
- `observe(el, { box: 'device-pixel-content-box' })` works. Before, it was gated as `Unsupported`. WhatsApp Web calls it in the photo attach preview.
- Every entry has `devicePixelContentBoxSize`. Before, the getter was gated.
- bare's `devicePixelRatio` is a fixed 1 (`browser_basics.rs`). No window scale factor reaches the page. So a device pixel is a CSS pixel.
- The size is the content box with its edges rounded to whole pixels: `round(right) - round(left)`. The spec says "in integral device pixels". Chrome snaps the edges in the same way.
- An observation of this box is active when the snapped size changes. A change below one pixel gives no report.

## Tests
- New file `crates/bare/src/browser_resize_device_box_tests.rs`, 2 tests. Both failed before the fix (no entry was delivered) and pass after it.
- `browser_resize_tests.rs`: one assertion changed. It expected the gate.
- `t`: all covering tests pass (resize, resize device box, message queue GC, raf, abort).

## Cost rows
- No row of `tools/js/cost_rows.txt` runs a ResizeObserver. `observer_load` has only `mo` and `raf`. No row was run.
- A page with no observation runs no new code. An observed target adds 4 float operations per measure and one size object and one array per entry.

## Not tested
- WhatsApp Web itself.
- A fractional device pixel ratio. bare has none.

# The style object sets `grid-template`

## Problem

- Teams (offline replay, `docs/internal/design-replay-sessions.md`) showed an almost empty page. The app grid had no areas: `getComputedStyle(layout).gridTemplateAreas` was `none`, and the title bar was at (287, 473) and not at (0, 0).
- React sets the grid with `el.style.gridTemplate = "…"`. In bare this did nothing:
  - `grid-template` was not in `SHORTHANDS` (`crates/css/src/values.rs`), so the style object did not know the name. The JS assignment made a plain property on the object. `setProperty('grid-template', …)` returned without a change.
  - When the name was known, `set` refused the value: no longhand text of a track list or of areas was found (`Unsupported("a style shorthand value with no faithful longhand text")`).
- `cssText` with the same value worked, because it parses the text directly.

## Fix

- `grid-template` is in `SHORTHANDS`. The parser already reads it (`grid.rs`).
- `bare_css::grid::text` writes a track list (`auto`, `1fr`, `minmax(…)`, `fit-content(…)`, `repeat(…)`, `[names]`) or areas (`"a a" "b c"`, `.` for an empty cell) as text. `written` in `browser_style_block.rs` uses it. `written` still parses the text again and keeps it only when it gives the same value, so a wrong text gives the old error, not a wrong value.

## Check

- New test file `crates/bare/src/browser_style_grid_template_tests.rs`: `style.gridTemplate`, `setProperty('grid-template')` and `cssText`. The first 2 failed before the fix (`none`); all 3 pass now.
- `t`: the test files that cover the changed files pass (4 runs: 3, 12 + 1 ignored, 9 and 37 tests).
- Cost rows `cost_matrix.sh -J -g style_load` (archMachine, base 96e3edd7, one count per row as the script does): 8 rows, all under the +0.5% limit. Mean +0.057% (glibc) and +0.032% (jemalloc). The largest: `churn 1` +0.139% (glibc), `churn 10` -0.139% (jemalloc).
- Teams offline replay (archMachine, 1 run, tape3): the layout grid now has its 6 rows of areas, and the title bar is at (0, 0). The page is still not right: Teams puts `header`, `main` and `contextual-notifications` in a wrapper with `display: contents`, which bare does not support (it computes `block`). That is a separate layout design.
- Not tested: Windows; the live Teams journey; `display: contents`.

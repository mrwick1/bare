# `HTMLStyleElement` reflects `media` and `type`

Date: 2026-11-25. Base: main 979eb9b7 (after #1040).

## Problem

- Logged-in Teams showed its main layout (`[data-tid='app-layout-area--main']`) about 30 s after sign-in. Then React threw error #185 (maximum update depth) and the shell went blank.
- The page logged `error {}` 613 times. A `console.error` hook showed the text: `TypeError: Cannot read properties of undefined (reading 'length')`.
- Evidence: a local print in the Vex unwinder gave the throw site. It is source offset 1132694 of `421867-5e7c69dbf8a5721d.js`, in Fela's DOM renderer: `e.map(function(e){return e.media}).filter(function(e){return 0!==e.length})`. `e` holds the `<style data-fela-type>` elements of the head.
- In bare, `style.media` was `undefined`. Chrome reflects the attribute and gives `""` when it is absent.
- `Error.prototype.stack` is also missing in Vex. A probe that added it did not change the 613 errors, so it is not the cause.

## Change

- `style_proto` (`browser_cssom.rs`) reflects `media` and `type` as strings, as the specification gives.
- A `<style>` wrapper now gets `style_proto` at once. Before, it used the `HTMLElement` prototype until the first `sheet` read. `link_proto` uses the same rule: the prototype is built at the first wrapper of its kind.

## Live effect (Teams, `docs/internal/teams/teams-login.steps`, 1 run each, archMachine)

| | main 979eb9b7 | this change |
| --- | --- | --- |
| `error {}` (the `length` TypeError) | 613 | 0 |
| React error #185 | 1 | 0 |
| CPU in the last step (`shot`, 90 s) | 61.8 s | 4.5 s |
| peak RSS | 1 352 540 KiB | 1 117 352 KiB |

The screenshot is still blank grey in both runs. That is the next blocker (not known yet).

## Cost

- `cost_matrix.sh -a -g 'cssom_load|style_load|preload_load style'`, base main 979eb9b7, glibc, minimum of 3 runs of `instructions:u`.
- 18 rows. Largest: `cssom_load sheets 1` +0.199%, `sheets 10` +0.157%. All within the rule. The jemalloc runs were skipped (glibc under the limit).

## Tests

- New: `crates/bare/src/browser_style_media_tests.rs`, `style_reflects_media_and_type`. It fails on main (`","` for `media,type`) and passes here.
- `t`: all pass.

## Not tested

- test262 counts: not run. The change has no JS engine code.
- Windows.
- More than one live run.

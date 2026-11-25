# Paint: Teams left rail (`clip: rect()` and inline `svg`)

## Problem

The Teams left rail had two faults. It showed the text "Has context menu" at the top. It painted only 3 of the 9 app icons (OneDrive, Copilot, Bookings).

## Cause

1. `Has context menu` is `#app-bar-context-menu-description`. Teams hides it with the Griffel class `screenReaderContainer`: `clip: rect(0, 0, 0, 0); position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; padding: 0`. A page rule also gives the box `min-width: 100%` and `min-height: 100%`, so its box is 68 x 752 px in bare. Chrome hides it with `clip`. bare had no `clip` property.
2. The 6 missing icons are `svg` elements with the Griffel class `f1w7gpdv` (`display: inline`). The UA sheet gives `svg` `display: inline-block`. An author `display: inline` made layout treat the `svg` as a plain inline, which ignores `width` and `height`. The `svg` box was 0 x 0. The 3 icons that showed are `<img>` with a filter.

The brief said that the labels are missing. They are not: Chrome shows the rail with icons only (`chrome1/settled.png`).

## Change

- `crates/css/src/values.rs`: new property `Clip`. `rect()` with no area (`right <= left` or `bottom <= top`, `px` or plain numbers) reads as `none`. `auto` and any other rect read as `auto`.
- `crates/paint/src/display.rs`: an absolute or fixed box with `clip: none` paints nothing, with all its content.
- `crates/layout/src/boxes.rs`: an `svg` with `display: inline` is an atomic inline (`Kind::Atomic`), as with `inline-block`.

## Tests

- New: `crates/css/src/values_clip_tests.rs` (3 tests).
- New: `crates/paint/tests/clip_rect.rs` (3 tests). `an_empty_clip_rect_hides_the_whole_box` failed before the change.
- New: `crates/paint/tests/svg_inline_display.rs` (2 tests). Both failed before the change.
- `cascade.rs`: the style size test goes from 880 to 888 bytes.
- `t`: pass.

## Live result (archMachine, 2026-11-25, saved session)

| | Before (`ws22xhtml/teams.png`) | After (`rail-8/teams.png`) |
|---|---|---|
| "Has context menu" | 3 lines of text | gone |
| Rail icons | 3 of 9 | 9 of 9 |

The chat list pane also shows more icons and a sidebar title row. I did not compare it with Chrome.

## Cost

No cost rows ran. `ComputedStyle` grows by 8 bytes per element (880 to 888) for the `Clip` slot. This is a cost debt: a row on style memory may go over the rule. A fix could pack `Clip` into a flag byte of an existing slot.

## Not done

- A `clip: rect()` with an area clips nothing (it reads as `auto`).
- `clip` does not affect hit tests (Teams sets `pointer-events: none` on this box).
- `img`, `canvas` and `video` with an author `display: inline` still lay out as plain inlines. Only `svg` is fixed.
- The page rule that gives the box `min-width: 100%` was not traced.
- In the shot, the title row and the right pane still differ from Chrome (header icons, the banner, the avatar stack, the PDF card).

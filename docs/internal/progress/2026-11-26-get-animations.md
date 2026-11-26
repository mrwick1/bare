# getAnimations

## Problem

T3 Code crashed on a thread page: `TypeError: (intermediate value).getAnimations is not a function`. It calls `getAnimations()` on
the sidebar panels to find running `width` transitions. bare had no `getAnimations`.

## Change

- `crates/bare/src/shims/animation.js`: `Element.prototype.getAnimations` and `Document.prototype.getAnimations` give a new empty
  array. In bare an `animate()` animation ends at once, and CSS transitions are not `Animation` objects, so no animation is running.
- `getAnimations` is a lazy name: its first read loads the Animation shim (`browser_shims.rs`, `browser_frames.rs`).
- New `crates/bare/src/browser_animation_list_tests.rs`.

## Gaps

- An `animate()` animation in the `running` state before its timer ends is not in the list.
- `options.subtree` is ignored. `CSSTransition` and `CSSAnimation` do not exist.

## Checks

- The new test failed before the change (throw) and passes after. `t` on the changed files: 66 passed.
- Cost rows: not run. A page that never reads the three names loads nothing.

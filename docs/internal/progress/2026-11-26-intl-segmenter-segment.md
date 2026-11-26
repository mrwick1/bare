# Intl.Segmenter.prototype.segment

## Problem

T3 Code crashed after pairing: `TypeError: (intermediate value).segment is not a function`. Its `ProjectMonogram` component calls
`Array.from(new Intl.Segmenter(void 0, {granularity: 'grapheme'}).segment(name))`. Vex had `Intl.Segmenter` with only
`resolvedOptions`.

## Change

- `crates/vex/src/bi_intl.rs`: `segment(string)` on the Segmenter prototype.
  - It uses the existing `icu::segment_breaks` (ICU4X grapheme, word and sentence breaks).
  - It returns an array of segment data objects: `segment`, `index`, `input`, and `isWordLike` for word granularity.
  - The array has a non-enumerable `containing(index)` method. One native for it is made once per realm and kept in the intl `State`.
- New `crates/vex/src/bi_intl_segment_tests.rs`: graphemes with `Array.from`, words with `for...of` and `containing`, a wrong
  receiver.

## Gaps

- The result is an array, not a `%SegmentsPrototype%` object. It iterates the same. `Array.isArray` on it gives `true`.
- `containing` does a linear scan.
- A lone surrogate inside a segment becomes U+FFFD in `segment`.

## Checks

- `t crates/vex/src/bi_intl_segment_tests.rs`: 3 passed. `t` on the changed files: 16 passed.
- `cargo check -p bare-vex --all-targets`: no errors.
- test262 counts: not run.
- Cost rows: not run. The change adds one entry to a lazy table. `interp_load` does not build that table unless a script reads
  `Intl.Segmenter.prototype`.

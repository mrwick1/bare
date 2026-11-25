# Error.stack: root the cached keys

## Problem

- After #1051, Teams panicked at the first page load: `atoms.rs:259` "flat", in `Op::Delete` (`key_of`).
- `ErrStack.keys` keeps the atoms `stackTraceLimit`, `\0stk` and `stack`. Nothing rooted them.
- A collection freed `\0stk` when no error held it. The cached `Str` then pointed to a freed or reused cell.

## Fix

- `stack_keys` now calls `keep` for the three atoms when it makes them. This is the same way the realm roots its other keys (`consts`).
- Test: `error_stack_gc_tests.rs`. It fails before the fix (`\0stk` is a new atom after a collection) and passes after.

## Check

- Bisect on archMachine, open-only journey on `https://teams.microsoft.com/v2/` (no login):
  - 1528d4f5 (before #1051): no panic.
  - 04980b96 (#1051): panic.
  - This fix: no panic. The login field shows after 11.0 s.
- `t`: pass.
- Cost: `keep` runs once per realm, at the first capture. No hot path changes, so no cost rows were run.

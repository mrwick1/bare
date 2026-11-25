# Source text in Function.prototype.toString (WhatsApp bring-up step 2)

Record: `docs/internal/design-whatsapp-bringup.md`.

## What changed

- `Function.prototype.toString` of a page function gives its source text (ES2026 20.2.3.5). Before, Vex gave
  `function NAME() { [native code] }` for every function. Each `Code` keeps the span of its text (`bytecode.rs`, `Code.span`).
  `fn_to_string` slices the text of the script unit (`LazyCompile::page_source_of`).
- Plain functions and arrows get the span. Methods, getters and setters keep the native form: their pre-parse span starts at
  the parameter list, not at the name.
- Prelude units (the JS parts of the engine) are not page text. Their functions stay native.
- A function from the `Function` constructor shows the name `anonymous`.

Cause: WhatsApp's `WAWeb-curve25519-donna` (Emscripten output) reads the source of small functions with
`t.toString().match(d)`. A `null` match threw, and the boot stopped.

## Tests

- `crates/vex/src/fn_source_tests.rs` (4 tests). `script_tests.rs`: two old expectations now give the source text.
- `t`: all covering tests pass.

## test262

Full pinned subset, archMachine, base d2725483 (main) and this branch merged with it. Base: 30679 pass, 429 fail, 8265 skip.
This branch: 30678 pass, 430 fail, 8265 skip. The one difference is `Script_-_Kannada.js` in
`RegExp/property-escapes/generated`, a time-out under load. Run alone, it passes. `built-ins/Function`: 433 pass, 42 fail,
34 skip on both.

## Cost rows

`interp_load` rows, base 16c1e3c3, archMachine, glibc and jemalloc. All rows within ±0.001% (example: `fib 27`
243250745 vs 243250775 instructions).

## Not tested

- Class methods, getters and setters with source text (they stay native).
- Memory: `Code.span` adds 8 bytes per function.
- Windows.

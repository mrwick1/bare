# 2026-11-25: WebAssembly PR 1 (Module, Instance, Global)

Branch `wasm-runtime`. Design: `docs/internal/design-webassembly.md` (approved by the owner on 2026-11-25), steps 1 to 4.

## Change

- New `crates/bare/src/browser_wasm.rs` on wasmi 2.0.3: `WebAssembly.validate`, `compile`, `instantiate`, `Module` (with
  `exports`, `imports`, `customSections`), `Instance`, `Global`, `CompileError`, `LinkError` and `RuntimeError`.
- Values: i32, i64 (BigInt), f32, f64, funcref, externref. A JS import can call into wasm again.
- A memory or a table in the imports or the exports is a `LinkError` that says bare does not support it yet.

## Tests

- New `crates/bare/src/browser_wasm_tests.rs`: 12 tests, all pass. They include the three feature checks of WhatsApp's
  `WAWebKaleidoscopeWasmFeatureSupport`.
- `t`: all pass.

## Cost

archMachine, base origin/main dffc5674, new = this branch. `cost_matrix.sh -g 'browser_load|web_apis_load|session_load'`,
3 runs per row (minimum), 26 rows.

- No row is over +0.5% under both allocators. Mean change: glibc +0.075%.
- `session_load chain 1`: +0.983% under glibc, +0.348% under jemalloc (noise candidate, under one allocator only).
- `web_apis_load segmenter 1` and `10` fail to run. They fail on main too.

Binary size, release `bare-mcp`: 19,778,504 bytes (base) and 21,391,304 bytes (new): +1,612,800 bytes (+8.2%). One build of
each.

## Not tested

- Memory, Table, streaming compile: not built.
- A collection that frees a `Module` or an `Instance` while its exports are in use (no GC test).
- A wasm loop that never ends (no step limit; it stops the page).
- The live WhatsApp journey. Windows.
- test262 does not cover this code; no counts.

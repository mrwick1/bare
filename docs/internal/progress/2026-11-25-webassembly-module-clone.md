# 2026-11-25: structured clone of a WebAssembly.Module

Branch `wasm-module-clone`. Design: `docs/internal/design-webassembly.md`, step 6.

## Change

- `browser_web.rs`: `WebClone::WasmModule`. `clone_out` and `clone_in` carry a `WebAssembly.Module` (its `wasmi::Module`).
- `browser_wasm.rs`: the engine is a field of the realm (`Wasm.engine`), not of the lazy state. A frame
  (`browser_child.rs`) and a threaded worker (`browser_child_thread.rs`) take the engine of the parent realm.
- A module from another engine is a `DataCloneError`.
- Cause: WhatsApp link attempt 3. The page posts its host check module to the backend worker, and the worker got a plain
  object: "The argument must be a WebAssembly.Module".

## Tests

- New `crates/bare/src/browser_wasm_clone_tests.rs`: 2 tests. `structuredClone` of a module instantiates. A module posted to
  an isolated (threaded) worker instantiates there. Both failed before the fix.
- `t`: all pass. `cargo test -p bare --lib browser_wasm`: 18 pass.
- test262: not run. The change has no Vex code.

## Cost

archMachine. Base = main 7a37bf38 (tree of #1084), new = this branch. Minimum of 3 runs (`instr.sh`).

- `cost_matrix.sh -g 'browser_load|web_apis_load|session_load|scheduler_load messages|navigation_load frames'`: 28 rows, none
  over +0.5%. Mean glibc +0.047%. `web_apis_load segmenter 1` and `10` fail to run, on main too.
- Release `bare-mcp`: 21,560,760 bytes (base 21,560,472, +288).

## Not tested

- A WhatsApp link with this fix (it needs a scan).
- A module posted to a frame, or to a worker that is not isolated. A module from another tab (`DataCloneError`). Windows.

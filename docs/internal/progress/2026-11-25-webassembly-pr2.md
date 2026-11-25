# 2026-11-25: WebAssembly PR 2 (Memory, Table)

Branch `wasm-memory`. Design: `docs/internal/design-webassembly.md`, step 5.

## Change

- Vex: a third `ArrayBuffer` kind, external. Slot 0 holds the address of host bytes as a number. No side table and no sweep.
  The hot paths of views read slot 0 as before; an external buffer takes the cold path. Script cannot transfer it.
- `bare-js-api`: `Engine::new_external_buffer(&'static mut [u8])` and `Engine::detach_buffer`.
- bare: `WebAssembly.Memory` (`buffer`, `grow`), `WebAssembly.Table` (`length`, `get`, `set`, `grow`), and memories and tables
  in imports and exports. A grow detaches the old buffer before JS runs again.
- Cause: WhatsApp link attempt 2 failed in `suspicious-host-checks-init`, because a module exports its memory.

## Tests

- New `crates/vex/src/bi_buffer_external_tests.rs`: 3 tests. New `crates/bare/src/browser_wasm_memory_tests.rs`: 5 tests.
- The PR 1 test `a_memory_export_is_a_link_error_for_now` is removed: the behavior changed on purpose.
- `t`: all pass. Vex `bi_buffer`, `bi_typed`, `api_clone`: 34 pass. Miri `bi_buffer::`: 11 pass.
- test262, `test262_js2.sh -g 'ArrayBuffer|TypedArray|DataView|Atomics'`: 3,471 pass, 0 fail on base and on new. The same tests pass.
- Live: on a fresh, unlinked WhatsApp page, `require('ErrorPubSub').history` is empty after 20 s.

## Cost

archMachine. Base origin/main 96e3edd7, new = this branch. Minimum of 3 runs (`instr.sh`).

- `cost_matrix.sh -g 'browser_load|web_apis_load|session_load'`: 24 rows, none over +0.5%. Mean glibc +0.022%.
  `web_apis_load segmenter 1` and `10` fail to run, on main too.
- Vex `interp_load` (loopfn, calls, fib, closures, objects): within ±0.001% under glibc and jemalloc.
- A typed-array loop (`script_run`, 60 x 65,536 `Uint8Array` reads and writes): +0.000% under both allocators. A first version
  that read the address on the hot path cost +2.1%; the address now goes through the cold path only.
- Release `bare-mcp`: 21,545,976 bytes (PR 1 on its base: 21,391,304). One build; the base of this PR was not built.

## Not tested

- The host check of WhatsApp after a link (it needs a scan).
- A shared memory (`shared: true` is a `TypeError`), `Tag`, `Exception`, streaming compile.
- A GC test of a `Memory` whose buffer is in use. Windows.

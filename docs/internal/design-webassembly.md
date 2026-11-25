# Design: WebAssembly in bare

Status: approved by the owner (2026-11-25). Option A (wasmi) is chosen. Build in steps (section 6).

## 1. Why now

- `docs/internal/decisions.md` keeps `WebAssembly` absent until a real runtime exists. A fake object that reports false success is not allowed. This document proposes the real runtime.
- WhatsApp Web loads WebAssembly in these places (source search of the live bundles, 2026-11-25):
  - `liboqs_wasm_wrapper.wasm`: post-quantum key exchange.
  - `WAMediaWasmWorkerResource`: a media worker (17 `WebAssembly` references).
  - URL spoof checks and VoIP.
- To verify: which of these the journey needs (link, chat list, read, send). The message download failure of 2026-11-25 was not WebAssembly. Its cause was invariant 56318: the worker global was not a `DedicatedWorkerGlobalScope` (fixed in branch `whatsapp-worker-scope`).
- Other journeys reach a missing `WebAssembly` too: Redis (Rive, Task 37) and the Angular docs page (script#7).

## 2. What a page needs

The JS API (WebAssembly JS API standard):

- Namespace: `validate`, `compile`, `instantiate`, `compileStreaming`, `instantiateStreaming`.
- Classes: `Module` (with `imports`, `exports`, `customSections`), `Instance`, `Memory`, `Table`, `Global`, `Tag` and `Exception` (later).
- Errors: `CompileError`, `LinkError`, `RuntimeError`.
- Calls both ways: an import is a JS function that wasm calls; an export is a JS function that calls wasm.
- `Memory.buffer` is an `ArrayBuffer` whose bytes are the linear memory. `grow` detaches the old buffer.

Core features used by Emscripten builds (to verify per WhatsApp module): MVP, bulk memory, sign extension, non-trapping float to int, multi-value, mutable globals, reference types. SIMD is possible in the media codecs (to verify).

## 3. Options

|                 | A. wasmi 2.x (interpreter crate)                                                                                          | B. Wasmtime (Cranelift JIT, or Pulley interpreter)                                                                        | C. Own interpreter in Vex style                    |
| --------------- | ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| Data structures | Register-based IR made from wasm at compile time. Linear memory is a `Vec<u8>`.                                           | Native code per function (Cranelift), or Pulley bytecode. Linear memory is a virtual-memory reservation with guard pages. | Our own IR, our own memory.                        |
| Time to ready   | Validation plus translation, linear in module size. Lazy translation per function exists (to verify the 2.x option name). | Cranelift compiles every function before the first call: the slowest start. Pulley is closer to A.                        | Like A, after months of work.                      |
| Run speed       | Interpreter. Hypothesis: several times slower than a JIT; enough for key exchange, maybe not for media codecs.            | Near native with Cranelift.                                                                                               | Like A at best.                                    |
| Memory          | Small runtime. The IR is about the size of the code.                                                                      | Large: compiled code, plus a 4+ GiB reservation per memory with guard pages (virtual, not RSS).                           | Like A.                                            |
| Binary size     | Hypothesis: about 1 MiB (wasmparser plus wasmi). To measure.                                                              | Hypothesis: 5 to 15 MiB with Cranelift. To measure.                                                                       | Smallest.                                          |
| Platforms       | Pure Rust. Linux and Windows, no executable memory.                                                                       | JIT needs W^X memory per OS; Pulley does not.                                                                             | Pure Rust.                                         |
| Work            | JS API glue only.                                                                                                         | JS API glue, plus a heavier dependency to keep current.                                                                   | Interpreter, validator, spec tests, then the glue. |
| Risk            | A slow codec path.                                                                                                        | Start time, memory and binary size, all against our main focus.                                                           | Months of work before the first page works.        |

Option D: keep `WebAssembly` absent and fix journeys around it. Not possible where a page has no fallback (Redis Rive aborts; WhatsApp to verify).

## 4. Proposal

Option A (wasmi), with the JS API in a new host module `browser_wasm.rs`. It removes no work by design, but it adds the least start time, memory and binary size of the real runtimes. A later step can add a faster tier for hot functions if a journey needs it. The JS glue stays the same.

Lazy rules, as for other interfaces (`browser_abort.rs`):

- `WebAssembly` is an accessor of the global until the first read. A page that never reads it pays nothing.
- A module translates a function at its first call (lazy translation), so a large module that runs a few functions stays cheap.

## 5. Integration with Vex

- **Memory.buffer.** This is the hard part. A Vex `ArrayBuffer` owns a heap byte cell; a `SharedArrayBuffer` wraps a shared block (`shared_memory.rs`). Views already resolve their backing store after each script call. Add a third backing kind: an external block owned by the wasm store, with a length and a "detached" flag. `grow` makes a new buffer object and detaches the old one, as the standard says. No byte copy on any access.
- **Calls.** An export is a host function (`HostFn::Method`) that holds the instance and the function index. An import is a wasm host function that calls back into Vex through the existing call API. Re-entry (JS calls wasm calls JS) must keep the Vex scope rules; the existing host-call path handles this for other host functions (to verify for deep recursion).
- **GC.** An instance roots its imported JS functions and its `Memory` object. A JS wrapper roots its instance. The wasm store lives as long as the last wrapper.
- **Async API.** `compile` and `instantiate` return promises and finish in a task, not in the call. `instantiateStreaming` reads the whole `Response` body first (no streaming compile in step 1).
- **Errors.** A wasm trap becomes `RuntimeError` with the trap text. Fuel or a step limit maps to the existing `EvalLimits` so a wasm loop cannot hang a page.
- **Workers.** A worker page is a full page, so the same module works there. A shared `Memory` across workers waits for a real use.

## 6. Steps

1. Count the need: list every module a WhatsApp journey loads, with size, features (SIMD?) and whether a failure stops the journey. Same for Redis and Angular.
2. Add wasmi behind the feature. Measure binary size, idle RSS and `cost_rows` with the feature on and off. The rows of a page that does not read `WebAssembly` must not move.
3. `validate`, `Module`, `compile`, `CompileError`.
4. `Instance`, `instantiate`, imports and exports of functions and globals, `LinkError`, `RuntimeError`.
5. `Memory` with the external buffer kind, `grow` and detach. `Table`.
6. `compileStreaming` and `instantiateStreaming`.
7. Run the WebAssembly spec JS API tests (wpt `wasm/jsapi`) and the WhatsApp journey. Record the numbers in a progress file.

## 7. Owner answers (2026-11-25)

The owner approved the plan ("Yes web assembly looks good"): the new dependency is accepted, and wasmi is on by default.
Measurement decides the speed question for media codecs when a journey needs them.

## 8. Build record

### Step 1: what WhatsApp needs (2026-11-25)

- The login page loads 22 bundles. None names a `.wasm` file: the modules load later, through the bootloader.
- `WAWebKaleidoscopeWasmFeatureSupport` tests three features before it uses its media path: bulk memory (`memory.copy`),
  reference types (`ref.null func`) and the i64 to BigInt integration. All three must pass. wasmi supports all three.
- To verify on a linked session: the modules that the chat list and a conversation load (liboqs, media).

### Steps 2 to 4: PR 1 (2026-11-25)

Changes from the plan:

- No cargo feature. The global is an accessor until the first read, so a page that never reads `WebAssembly` pays for one
  function. A feature would only save binary size; the progress file gives the size.
- Ladybird (`Libraries/LibWeb/Bindings/Rust/src/web_assembly`) is the reference for the JS glue. Its `Memory.buffer` is an
  external data block that resolves through a handle into the engine's memory, and "refresh the memory buffer" detaches the
  old buffer on grow. Step 5 takes the same design. wasmi has no grow callback: `ResourceLimiter::memory_growing` can mark
  the grow, and the next return to JS refreshes the buffer.
- One wasmi store for each page, freed with the page. wasmi cannot free an instance before its store. Imported JS
  functions and `externref` values are `Persistent` for the life of the page. Hypothesis: this is enough for the journeys,
  which make a few instances. A page that makes many instances keeps them all.
- A JS import can call into wasm again: the import keeps the wasmi `Caller` in a thread-local cell, and a nested call uses
  its store.
- No step limit in wasm code yet. A wasm loop that never ends stops the page.

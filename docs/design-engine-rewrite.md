# Design: a new JavaScript engine for bare, with a JIT

Status: design, for review by the owner. No engine code changed. Date: 2026-11-18. Base: main `25126702` (measurements) and `d89f0299` (this document).

Owner decision (2026-11-18): rewrite the whole engine (`crates/js`) and add a JIT. The goals are CPU, memory, a design that the owner can explain and defend, and simpler code.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "op" is one executed bytecode instruction of the current lane (`bc_vm.rs`).
- "journey" is the staging E2E of `docs/progress.md` (2026-11-18): log in, create a client and an agent, delete them. It is a production React and Next.js app.
- "G" is a billion user-mode instructions (`instructions:u`).

## 1. Summary

The current engine spends its time and memory on its own bookkeeping, not on the work of the script. Three facts from the journey show it:

- Hypothesis: one op costs about 210 instructions on average. This is an estimate, not a measured count: 22.23 G instructions times the JS share of CPU samples (72.3% in the `jscat.py` run, 71.3% in the `classify.py` run) gives 15.9 to 16.1 G, over 76.3 M ops. It includes natives, parse, GC and allocation that ops call. Decode and dispatch was about 23 instructions per op in the research of 2026-11-17 (a different workload; that number also includes spills and code with no source line). So a JIT that removes dispatch alone would remove roughly 10% of the JS work (hypothesis). The old bound (5.5% to 12.9%) used another page and denominator; it points the same way but does not prove this number.
- The heaviest ops are moves and variable reads: `Mov` 12.8%, `GetField` 12.6%, `GetEnv` 12.4%, `SetEnv` 3.3%. Register functions already keep locals in registers (`bc_vm.rs:323`). A function with inner functions still makes an `Env` per call, and all its named variables that inner functions can see live there (stage 1 of `bc_gen.rs`). `GetEnv` reads them through the environment chain.
- An object costs 104 bytes plus 8 bytes of slot metadata, before any property is stored out of line. A value is 16 bytes. V8 stores an object with two fields in about 20 bytes (hypothesis: from the published layout, not measured here).

Recommendation (section 7): build a new engine crate, `bare-js2`, next to the old one, and switch it on per session with `BARE_JS_EXEC=new`. Its core is:

1. Values in 64 bits (NaN-boxing) with direct pointers into the GC heap. No slot table, no generation check.
2. A compiler that goes from the parse straight to register bytecode, with scope analysis. The pre-parser records which variables each inner function captures, so a lazy inner function no longer forces all variables of its parent into an environment. Locals and arguments live in registers. Only captured variables go to a small context object. Moves are removed with direct register operands and copy coalescing.
3. Objects as a shape pointer and inline slots: a plain object has an 8-byte header; element and overflow pointers exist only in layouts that need them. Object literals with fixed keys get their final shape at once. Inline caches are allocated on the second use, start with one shape and grow to a small polymorphic cache only where the shape keeps changing.
4. A non-moving heap of size-class blocks with lazy sweep (section 6.10). Immix and a sticky-mark young generation are kept only if they beat it on the journey's measured object lifetimes (section 6.11).
5. One interpreter (no tree walker). A baseline JIT that keeps the interpreter's frame layout (the Sparkplug model, on `dynasm-rs` for x64 and arm64) comes only if a profile of the daily journeys shows hot code that pays for it. A speculating JIT is checked last, after the old engine is removed (S8, section 6.12).
6. The syntax tree is dropped after compile. Bytecode of a function that has not run for a while is dropped and compiled again on the next call (bytecode flushing).
7. A written GC safety contract at the host boundary (section 6.8): safepoints, scoped and persistent roots, wrapper identity and barriers. It is a condition of S0 and S1.
8. Scope: the engine supports and is tuned for what the daily journeys run: the staging journey, the typing page and the public journeys. A feature census decides which language features and built-ins are ported (section 6.12).

Expected result on the journey, all hypotheses until stage gates measure them: JS work down 50% to 65% before the JIT, and another 10% to 20% of what is left with the baseline JIT, if it is built. JS heap at the peak down 40% to 55% (about 51 MiB to 23 to 30 MiB).

## 2. How this was measured

- Machine: archMachine (AMD Ryzen 9 5900HX, 16 threads, Linux 7.2.15). Other agents ran at the same time (load average 0.8 to 9). Only counts and shares are used. Wall time is not used.
- Journey: `journey.steps` of the 2026-11-18 entry, headless, live staging. Every run passed.
- CPU split: `perf record -F 4000 -e cpu-clock --call-graph fp` on a release build with line tables and frame pointers. 3 runs in `tree`, 1 run in `auto`.
- Op counts: the `bc-hist` feature of `bare-js`, `BARE_JS_EXEC=auto`, 1 run (op counts were deterministic within 0.002% in the research of 2026-11-17).
- Memory: one run under `valgrind --tool=massif`, peak heap tree grouped by the first engine frame.
- Mode A/B: 3 runs each of `tree`, `auto` and `byte`, `perf stat -e instructions:u,task-clock`.
- Scripts and raw output: `/data/bare-tmp/voicebot-e2e-2026-11-18/subsystem-profile/` (not in the repository).

## 3. Measured profile

### 3.1 Where the CPU goes

|                                                      | `tree` (default) |  `auto` |
| ---------------------------------------------------- | ---------------: | ------: |
| Instructions, whole journey                          |          29.85 G | 22.23 G |
| JS engine share of CPU                               |              78% |     71% |
| Style                                                |               9% |     12% |
| Everything else (DOM, network, layout, HTML, driver) |              13% |     17% |

JS work in `auto`, by kind (share of all CPU; a sample goes to the first matching frame from the leaf, so the split is approximate):

| Kind of work                                             | Share |
| -------------------------------------------------------- | ----: |
| Bytecode loop (`bc_loop`, decode, dispatch, inline arms) | 18.2% |
| Property get and set (shapes, caches, lookups)           | 17.2% |
| Allocation (objects, arrays, `malloc`, `free`)           | 11.1% |
| Parse and compile                                        |  6.0% |
| Calls (frames, arguments, `bind_call`)                   |  5.1% |
| Collector                                                |  4.7% |
| Strings and atoms                                        |  4.3% |
| Tree walker (code that the lane refuses)                 |  1.8% |
| Variable lookup                                          |  1.5% |
| Other                                                    |  2.8% |

The collector row counts only samples whose leaf is in the collector. Samples with `Interp::collect` or `Heap::collect` anywhere on the stack: 1,229 of 13,026, so 9.4% of all CPU (1 run, round 2 review). The collector work in `free`, `Vec` and shape lookups goes to other rows in the table.

### 3.2 What the bytecode runs

76,322,599 ops in 4 realms. 21 ops give 90.1%.

| Op         | Share |     | Op       | Share |
| ---------- | ----: | --- | -------- | ----: |
| `Mov`      | 12.8% |     | `Jmp`    |  3.6% |
| `GetField` | 12.6% |     | `CallR`  |  3.5% |
| `GetEnv`   | 12.4% |     | `SetEnv` |  3.3% |
| `LoadNull` |  6.1% |     | `LoadK`  |  3.2% |
| `JmpIfNot` |  5.1% |     | `JnSNeK` |  2.7% |
| `JnSNe`    |  5.0% |     | `JnSEq`  |  2.2% |
| `LoadI`    |  4.2% |     | `Ret`    |  1.9% |
| `SetField` |  3.8% |     | `Wide`   |  1.6% |

- The top pair is `GetEnv` then `GetField` (2.7 M): read a variable from the environment, then a field of it.
- `Mov` and `GetEnv` together are 25% of all ops. A compiler that keeps every non-captured variable in a register removes most `GetEnv` (hypothesis). Which `Mov` go away depends on the register allocator of the new compiler (hypothesis).
- Inline caches: 11.8% of `GetField` executions (1,137,155 of 9,632,782) leave the inline own-property path. That count includes prototype cache hits and primitive receivers, so the true miss rate is lower and not measured. `SetField`: 1.3%. The cache holds one shape.
- Lane coverage: 98.5% of calls (2,363,990 on the lane, 34,864 on the tree walker).

### 3.3 Where the memory goes

Peak heap 61.6 MiB plus 9.3 MiB of malloc overhead. Peak RSS of normal runs: 83 to 97 MiB. The JS engine holds 50.7 MiB:

| Part                                                                     |  MiB |
| ------------------------------------------------------------------------ | ---: |
| Objects, shapes and property slots                                       | 23.6 |
| Source and syntax trees                                                  | 12.9 |
| Collector work lists at the peak                                         |  4.9 |
| Scopes, closures and strings                                             |  4.9 |
| Per-script name and scope tables (`Caches::new`, indexed by syntax node) |  3.3 |

The split is approximate. It groups massif allocation stacks by the first engine frame, in the default mode (`tree`). Some rows mix kinds (for example `ObjTab::retain` is counted with objects). Source, syntax tree, bytecode, feedback and transient collector memory in `auto` are not measured separately (section 9).

`auto` adds 6.7 MiB of peak RSS over `tree` (the bytecode, kept next to the syntax tree; hypothesis, not split by part).

## 4. The current design, and what it costs

| Part          | Data structure now                                                                                                                                                                          | Cost it causes                                                                                                                                                                                        |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Value         | `enum Value`, 16 bytes; a string, symbol or object is a handle (slot index and generation)                                                                                                  | Every heap access goes handle → slot table → dense `Vec` → payload, with a generation check. Twice the memory of an 8-byte value in every slot, register and array element.                           |
| Object        | `struct Object`, 104 bytes: prototype, shape id, flags, `elems: Vec<Option<Value>>`, 3 inline values, then a `Vec` for more, and an optional dictionary                                     | A fourth property moves all values to a `Vec` (one more allocation, one more pointer hop). Arrays pay a `Vec` header and a 16-byte `Option<Value>` per element.                                       |
| Variables     | Register functions keep locals in registers. A function with inner functions makes `Env { parent, bindings: Vec<Binding>, index: Option<FxMap>, ... }` per call, and per block where needed | One heap environment and one `Vec` per such call. Variables that inner functions see are read with `GetEnv` (hops and index). Closures keep whole environments alive.                                 |
| Inline caches | One shape per site, in a side table                                                                                                                                                         | 11.8% of `GetField` executions leave the inline own-property path. That count includes prototype cache hits and primitive receivers, so it is not a miss rate. Miss reasons by site are not measured. |
| Execution     | Tree walker for first calls and refused code, bytecode from the second call                                                                                                                 | Two engines for every feature. Every semantic rule is written twice. The bytecode stays next to the tree, so memory grows.                                                                            |
| Front end     | Lexer → arena syntax tree (kept) → bytecode on demand                                                                                                                                       | The tree stays for the life of the script (12.9 MiB with the source). Lazy bodies are parsed again at the first call.                                                                                 |
| Collector     | Mark and sweep over slabs, at safe points, non-generational; every collect walks the whole live heap                                                                                        | The cost of one collect grows with the live heap, not with the garbage. Freed memory goes back to malloc, so allocation is a `malloc` call.                                                           |

## 5. How other engines solve it

Sources read on 2026-11-18: <https://v8.dev/blog/sparkplug>, <https://v8.dev/blog/maglev>, <https://v8.dev/blog/pointer-compression>. Other rows are from the engines' published design and are marked "not re-read".

|             | V8                                                                                                          | SpiderMonkey (not re-read)                                            | JavaScriptCore (not re-read)                                  | QuickJS (not re-read)                    |
| ----------- | ----------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------------------------- | ---------------------------------------- |
| Value       | Tagged 32-bit slot: 31-bit small integer or a 32-bit compressed pointer in a 4 GiB cage. Doubles are boxed. | NaN-boxing, 64 bits                                                   | NaN-boxing variant, 64 bits                                   | 16-byte tagged struct on 64-bit          |
| Locals      | Bytecode registers                                                                                          | Frame slots                                                           | Frame slots (virtual registers)                               | Frame slots                              |
| Tiers       | Ignition (interpreter) → Sparkplug (baseline JIT) → Maglev (mid JIT, SSA) → TurboFan                        | Interpreter → baseline interpreter → baseline JIT → Warp (optimizing) | LLInt → Baseline JIT → DFG → FTL                              | Interpreter only                         |
| Collector   | Generational: copying young generation, mark-compact old generation, concurrent and parallel                | Generational, incremental, compacting                                 | Generational (sticky marks), concurrent, non-moving (Riptide) | Reference counting plus cycle collection |
| Syntax tree | Dropped after bytecode. Bytecode flushed when old.                                                          | Dropped; lazy parse                                                   | Dropped; lazy parse                                           | No tree: parse emits bytecode directly   |

Numbers that bear on our choices:

- Sparkplug (baseline JIT, no IR, keeps the interpreter's frame layout; the published version has a prepass that finds loops and one code-generation pass): about 5% to 15% of V8 main-thread time on browsing benchmarks, and 5% to 10% on Speedometer, over Ignition with TurboFan. The Maglev blog gives 41% on Speedometer over Ignition alone (Chrome 117). These are reference results of another engine, not a forecast for bare.
- Maglev compiles about 10 times slower than Sparkplug and 10 times faster than TurboFan.
- Pointer compression: tagged values were about 70% of the V8 heap. Compression made the V8 heap up to 43% smaller and the renderer up to 20% smaller.

How V8 handles each module, with its published numbers: `research-v8-reference.md`.

How QuickJS handles each module, read from its source, and a measured baseline of QuickJS, Bare and Node on the `eval_load` cases: `research-quickjs-reference.md`.

What we take from them:

- A good interpreter and a good object model come first. V8, SpiderMonkey and JavaScriptCore keep locals in registers or frame slots and use a 4-byte or 8-byte value. QuickJS uses a 16-byte value, as bare does today.
- A baseline JIT in the Sparkplug model is cheap to build and keeps the interpreter's frames. That makes deoptimization unneeded, because nothing is speculated.
- The spike of 2026-11-17 measured one field copy in JIT code at 83 instructions with today's checked handles and 49 with a raw object pointer. Direct pointers are part of the JIT's gain.
- JavaScriptCore shows that a non-moving collector with sticky mark bits can be generational. That fits Rust, where moving objects needs precise rooting of every pointer.

## 6. Options

Each part has at least two options. The recommended one is first.

### 6.1 Values

| Option                                          | Layout                                                                                                                         | Gain                                                                                                                               | Cost                                                                                                                                      |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| **A. NaN-boxing, 64 bits** (recommended)        | A double is stored as is. Other values sit in the NaN space: a 48-bit pointer, a 32-bit integer, `undefined`, `null`, booleans | Half the size of today's value. A type test is one compare. No allocation for doubles. Fits x64 and arm64 (48-bit user addresses). | Pointers must fit 48 bits (true on all three target platforms; arm64 pointer authentication is not used for data pointers).               |
| B. Compressed 32-bit slots in a 4 GiB cage (V8) | 31-bit integer or 32-bit offset from a cage base; doubles boxed                                                                | A quarter of today's size in slots. V8 measured up to 43% smaller heap.                                                            | Every double that does not fit 31 bits is a heap object. Every load adds the base. A 4 GiB heap limit. Harder to write correctly in Rust. |
| C. Keep a 16-byte tagged enum (QuickJS)         | as today                                                                                                                       | No change                                                                                                                          | Keeps the memory cost of section 4.                                                                                                       |

Option B can come later for object slots only, as a separate step with its own measurement.

Conditions of option A:

- Address invariant: every GC pointer must fit the payload. On Linux x64, `mmap` returns addresses above 47 bits only when the caller asks for them with a high hint (5-level paging). The heap reserves its own address range at start and checks every block address against the payload width. If the check fails, the value stores a 32-bit offset from the reservation base instead (option B for pointers only).
- Rust pointer provenance: values hold pointers as integers. Use the exposed-provenance API (`expose_provenance`, `with_exposed_provenance`) in one module, and run that module under Miri.
- NaN canonicalization: every double that enters from outside (arithmetic results, `Float64Array` and `DataView` reads, host numbers) is checked, and a NaN with a payload is replaced by the canonical NaN. Otherwise a crafted NaN could look like a pointer.

### 6.2 Heap and collector

| Option                                                                                                | Data structures                                                                                       | Allocation                                                      | Collect cost                                                                                                                                                     | Fit for Rust                                                                                                                                                     |
| ----------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A. Immix mark-region, non-moving, sticky marks (candidate; must beat size-class blocks, section 6.10) | 32 KiB blocks of 128-byte lines; a mark bit per object and per line; large objects in their own pages | Bump pointer inside free lines: a few instructions, no `malloc` | Young collections mark only new objects (sticky mark bits and a write barrier on old → young stores). Full collections mark the live heap; sweeping is per line. | Objects never move, so a Rust pointer held across a collect stays valid. Roots: the JS register stack, the globals and an explicit handle stack for native code. |
| B. Copying young generation, mark-compact old (V8)                                                    | Two semispaces for the young generation                                                               | Bump pointer                                                    | Young collection costs the survivors only                                                                                                                        | Every pointer in Rust locals must be rooted and updated. A missed root is memory corruption.                                                                     |
| C. Reference counting with a cycle collector (QuickJS)                                                | Count in each object                                                                                  | `malloc`                                                        | Pays on every store; cycles need a second pass                                                                                                                   | Simple, but slow on the store-heavy code that React runs.                                                                                                        |

Notes on option A:

- Original Immix also moves some objects (opportunistic evacuation) to fight fragmentation. This design leaves evacuation out, so fragmentation must be measured: live bytes against committed bytes, and pages given back to the OS after churn.
- The write barrier (old object gets a pointer to a young object) costs instructions on every pointer store, in the interpreter and in JIT code. S1 measures it.
- S1 builds a plain non-generational mark-sweep with the same roots first, and compares both on the same rows. The sticky-mark young generation is kept only if it wins on instructions and peak RSS.

### 6.3 Variables and calls

| Option                                              | How                                                                                                                                                                                                                                                    | Effect                                                                                                      |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| **A. Scope analysis at compile time** (recommended) | The pre-parser records the captures of each inner function. The compiler knows each name: a register (local, not captured), a context slot (captured by a closure), or a global (cached by shape). A call pushes one frame of registers on a JS stack. | `GetEnv` becomes `Mov` or nothing. No heap environment per call. Closures keep only the captured variables. |
| B. Keep environments, cache lookups                 | Environments as today, with (hops, index) cached per site                                                                                                                                                                                              | Removes the search, keeps the allocation and the hop chain.                                                 |

What option A must also design (in S2, before the compiler is built):

- Dynamic scopes: direct `eval`, `with` and sloppy-mode mapped `arguments` can read or create variables at run time. A function that contains them keeps its variables in a context object with a name table, as today.
- Resumable frames: generators and async functions keep their registers in a heap object while suspended. The collector traces it.

### 6.4 Objects and property access

| Option                                                                                                                    | How                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Effect                                                                                                                                                                                                                       |
| ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Shape-only header + inline slots; overflow and elements only when needed; caches that grow on demand** (recommended) | Plain object: one 8-byte shape pointer (the prototype lives in the shape), then inline slots. The elements pointer and the out-of-line slots pointer exist only in layouts that need them, and inline slots stay when overflow is added. Inline slot count per allocation site from the observed shape (slack tracking). Literals with fixed keys allocate their final shape. A cache site gets its first entry on the second use and grows to at most 4 (shape, offset) pairs where the shape keeps changing; past that it uses a global (shape, key) table. | An object `{a, b}` is about 24 bytes before allocator metadata (hypothesis: 8 header + 2 × 8 slots). A cache hit is a compare and a load. Fewer leaves from the inline path (hypothesis; S1 measures miss reasons per site). |
| B. Keep today's 104-byte object, add polymorphic caches                                                                   | as today, plus 4-entry caches                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Fixes the misses, not the memory.                                                                                                                                                                                            |

Cache rules: a site starts with one entry and grows to at most 4 when it sees new shapes; past 4 it becomes megamorphic and uses a global (shape, key) table. A hit on a prototype property is guarded by a validity cell that the prototype clears when its keys change. Dictionary-mode and exotic objects (proxies, host objects with named properties, typed arrays) always take the slow path. S1 measures miss reasons per site before the size 4 is fixed.

Arrays: a dense elements store of 8-byte values with kinds (integers, doubles, any), and holes marked by a reserved NaN value.

### 6.5 Front end

| Option                                                                              | How                                                                                                                                                                                                                                                                                                                                                                                                          | Effect                                                                                                                                             |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Parse to a temporary tree per function, compile, drop the tree** (recommended) | Lazy pre-parse (syntax check and capture facts, no tree) for inner functions, with today's nested skip ranges. Source and compiled code are shared by all realms of a session (key: verified source identity and parse mode); atoms, constants and cache data stay per realm. At the first call: parse that function, compile to bytecode, drop its tree. Flush bytecode that has not run for N collections. | The 12.9 MiB of trees goes away (hypothesis: the source text stays, about 4 to 6 MiB on this app, not measured). Simple compiler passes on a tree. |
| B. Single pass, parse straight to bytecode (QuickJS)                                | No tree at all                                                                                                                                                                                                                                                                                                                                                                                               | Least memory, but scope analysis and some optimizations need a second pass over the bytecode. Harder to read.                                      |

The lexer, the parse cache and parse-ahead on a worker thread stay as ideas. The code is rewritten to emit the new format.

### 6.6 Execution tiers

| Option                                                                  | Tiers                                                                                                                                                                                                                                                                                                           | Gain                                                                                                                                                                                                                         | Cost                                                                                                                                                                                         |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Interpreter, then a baseline JIT (Sparkplug model)** (recommended) | One register interpreter (no tree walker). A function that runs often compiles to machine code in a prepass and one code-generation pass over its bytecode. Same frame layout, so no deoptimization. Hot ops (field access, compares, jumps, integer math) are inline; the rest call the interpreter's helpers. | Reference: Sparkplug gave 5% to 15% of V8 main-thread time (section 5). Removes dispatch and decode (about 10% of today's JS work, hypothesis; more once the other costs are gone, because dispatch becomes a larger share). | Two assembler back ends (x64, arm64). Code memory (the design caps it and drops cold code).                                                                                                  |
| B. A JIT with Cranelift                                                 | Bytecode → Cranelift IR → machine code                                                                                                                                                                                                                                                                          | Better register use, later a path to an optimizing tier                                                                                                                                                                      | The spike of 2026-11-17 (branch `baseline-jit-design`) measured 1.65 M instructions of compile for each inline property op, and +3.0 to 4.9 MB of binary. Better as a later optimizing tier. |
| C. An optimizing tier with speculation (Maglev-like)                    | SSA with type feedback and deoptimization                                                                                                                                                                                                                                                                       | The largest gain on hot loops                                                                                                                                                                                                | Deoptimization, feedback vectors and an SSA compiler. Checked last (S8), only if a daily-journey profile shows hot loops that the baseline tiers leave slow (section 6.12).                  |

JIT tools: `dynasm-rs` (an assembler crate with x64 and arm64), or a small hand-written emitter. Platform rules: Linux `mmap` then `mprotect` (W^X), Windows `VirtualAlloc` and `VirtualProtect`, macOS arm64 `MAP_JIT` with `pthread_jit_write_protect_np`.

### 6.7 How to replace the engine

| Option                                                       | How                                                                                                                                                                                                                                                                                                                      | Risk                                                                                                                       |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| **A. New crate next to the old one** (recommended)           | `crates/js2` with its own value, heap, compiler and interpreter. The browser talks to the engine through one trait (the host interface: DOM wrappers, callbacks, microtasks, modules). `BARE_JS_EXEC=new` picks it per session. Journeys run on both until the new one passes every gate, then the old crate is removed. | Two engines for a while. The host interface must be written once for both.                                                 |
| B. Replace parts of `crates/js` in place                     | Change the value, then the heap, then the scopes, inside the current crate                                                                                                                                                                                                                                               | Every step touches 65,000 lines and both tiers. The value change alone touches nearly every file.                          |
| C. Embed an existing engine (QuickJS through a crate, or V8) | Bind it to the host interface                                                                                                                                                                                                                                                                                            | Fast to mature, but not the owner's design, and V8 conflicts with the memory goal. Kept as a reference to measure against. |

### 6.8 GC safety at the host boundary

Today the collector refuses to run during a call (`eval.rs:983`), and host code relies on it: `HostIter` lives in Rust locals with no rooting (`eval_host.rs:22`). DOM wrappers keep listeners and other wrappers alive through edges that the host owns (`crates/bare/src/browser.rs`). With raw pointers, a mistake here is memory corruption, not a stale-handle error. The new engine needs a written contract before S1:

- Safepoints: the collector runs only at listed points: the interpreter's allocation slow path, loop back edges and calls, and the host's explicit `safepoint()` calls. Host code between safepoints may hold raw pointers.
- Scoped roots: a host function that keeps a JS value across a safepoint puts it in a handle scope (a stack of roots, popped when the scope ends).
- Persistent roots: host objects that live longer than a call (listeners, timers, wrappers in DOM nodes) hold realm-bound persistent handles. A realm frees all of them when it ends.
- Re-entrancy: JS called from the host may collect. Host code must not hold a Rust borrow of engine memory across such a call. The API makes this a compile error where it can (the borrow ends before the call).
- Wrapper identity: one wrapper per DOM node, kept in a weak table. A wrapper with no other reference is pruned at collection. Cycles of wrapper → listener → closure → wrapper through a detached node are collected.
- Barriers: host stores of JS values into old objects, and JIT stores, run the same write barrier as the interpreter.
- Gates: a stress mode that collects at every allowed safepoint runs the journeys in S0 (old engine, to prove the contract) and in S1 and later (new engine). A test shows that a detached subtree with listeners in a cycle is reclaimed.

### 6.9 Changes from the data-structure review

A second review (GPT-6.2 Sol, 2026-11-18) looked at each module for data structures and algorithms. I checked its main points against the code. The design takes these changes, ranked by expected gain for the effort (every gain is a hypothesis):

1. Calls across scripts stay in the interpreter loop. Today a compiled function of another script already runs in the caller's loop (`bc_target`, `bc_vm.rs:466`). The earlier measurement: -7.31% instructions at 50 keys, -3.7% at load (`design-lane-calls.md`). The new frame carries the code and realm, and reuses the caller's argument window.
2. Object literals with fixed keys allocate their final shape at once and fill the slots in order. Today the lane emits `NewObject` and then one `DefField` per key (`bc_gen_expr.rs:254`); the tree walker already uses a per-site shape. This removes the key lookup, the transition and the descriptor check per field.
3. One graph walk per collection. Each cell has a trace kind in its layout and visits its own edges. No side-table probe for plain cells. Target: the 4.9 MiB of collector work memory and the collect time. To verify: how much of today's walk is done twice (`eval.rs:983` collects roots, then `Heap::collect` marks).
4. The pre-parser records which variables each inner function captures. Today, when a function has any lazy inner function, all its variables stay in the environment (`bc_gen_rf.rs:51`, `lazy_inner`). With capture facts, the parent keeps the other variables in registers. The code generator also removes moves with direct register operands and copy coalescing in a block. Target: the top pair `GetEnv` then `GetField` (2.7 M) and `Mov` (12.8%). S2 first counts reads by kind (captured, dynamic, local).
5. A plain object has a header of one shape pointer (8 bytes). The elements pointer and the out-of-line slots pointer exist only in layouts that need them. Inline slots stay when an overflow store is added. Target: the 23.6 MiB of objects.
6. Source and compiled code are shared by all realms of a session. The key is the verified source identity and the parse mode. Atoms, constant objects and cache data stay per realm. HTTP and JS share one source buffer. This removes repeated compiles of the same Next.js chunks.
7. Feedback is allocated on the second use, as the tree walker does today. A cache site starts empty, gets one entry, and grows to a small polymorphic cache only where the shape keeps changing.
8. Arrays start with one tagged elements layout, dense or sparse. Numeric or pointer-free stores are added only if an element census shows the work they remove.
9. The lexer keeps borrowed spans and its lexical goals. The grammar gets a check-only mode that records scope facts without tree nodes. The nested skip ranges stay (`parse.rs:3329`), so a first-call compile jumps over checked children.
10. Before the young generation, S1 compares size-class pages with Immix, with the same collector and layouts. This separates allocation and fragmentation from barrier cost.

Kept from today, because it is already better than a generic design: the streaming lexer with borrowed spans, 32-bit bytecode words, the script-neutral call path, lazy prototypes of built-ins, data literals and `JSON.parse` that build heap values straight from the text, and native-byte accounting in the collector trigger.

### 6.10 Changes from the four-model review, round 1

Four reviewers (Claude Sonnet 5.5, Claude Opus 5.5, GPT-6.2 Sol, GPT-6.2 Astra; 2026-11-18) read the design with one brief: data structures and algorithms for resource use. I checked their claims against the code. Facts they found:

- The collector copies every live binding of every live environment into one roots `Vec` before it marks (`eval.rs:939-941`, then `eval.rs:1108`). This answers the "to verify" of 6.9 item 3. It explains part of the 4.9 MiB of collector work memory (hypothesis: the share is not measured).
- The 23.6 MiB of objects at the peak includes a slab `Vec` that doubles when it grows: one 13,631,488-byte block from `grow_amortized` (131,072 × 104 bytes) in `massif.out`. Up to half of a doubled block is spare capacity, so the live object bytes are lower than 23.6 MiB (hypothesis: by up to 6.8 MiB).
- Constant operands exist only for number literals (`num_konst`, `bc_gen_rf.rs:231`). `null`, `undefined`, booleans and strings load into a register first.
- `switch` compiles to a linear chain of compares (`bc_gen_stmt.rs:288`).
- `Wide` words are allowed only for `GetEnv`, `SetEnv`, `InitEnv` and `DefField` (`bc_vm.rs:107`). Environment slots past 255 happen, so captured-variable layout matters.
- A call fills every non-argument register with `undefined` (`bc_vm.rs:1446`).
- A dictionary-mode read clears the cache site and then installs a dictionary entry (`IC_DICT`) for an own key (`get_dict`, `heap_shape.rs:1372`). Correction from round 2: the site does not lose its entry each time; it loses the previous shape entry when receivers alternate.

The design takes these changes. Every gain is a hypothesis until its gate measures it.

Interpreter and bytecode:

1. Constant operands for `null`, `undefined`, booleans and atoms in compare-jumps and in `SetField`, not only numbers. This removes a load and a register per compare.
2. `switch` with constant cases compiles to a jump table (dense integers) or a hashed table (atoms). A linear chain stays for small or mixed cases.
3. A lean dispatch loop is designed and measured first, before the JIT: hot state in locals, one decode per op, no per-op checks that the verifier already proved. S2 records instructions per op by kind.
4. A frame does not clear dead registers. The compiler proves that a register is written before it is read. Only registers that the collector can see before the first write get a hole value. This removes the fill at each call. Round 2 condition: safe only with collection between tasks or with root maps per safepoint (section 6.11).
5. Destination-driven code generation: an expression writes into the register its parent asks for. This removes most `Mov` at the source (`Mov` is 12.8% of ops).
6. Fused `GetCtxField` (context slot, then field) for the top pair `GetEnv` then `GetField`. `GetEnv` carries a hop count. A variable in its temporal dead zone holds a hole value, so a read checks one value, not a flag.
7. Constants split by kind: numbers and atoms inline in the word where they fit, others in a per-function table.
8. One frame record (code, context, `this`, argument count, return address) on the JS stack. No separate frame `Vec` and register `Vec`.

Functions and calls:

9. A closure is a function cell: code pointer and context pointer. A call reads the code from the cell. This removes the `ObjTab` probe for functions.
10. Eager compile for functions that run at once: an IIFE and a module or bundle wrapper. This removes the lazy pre-parse and second parse of code that always runs.

Objects, caches and strings:

11. The overflow layout is fixed at allocation: inline capacity from slack tracking, plus one overflow record when a store passes it. An object never moves to grow.
12. A cache record splits into a hot part (shape, offset; 8 to 16 bytes, in the per-realm feedback vector of section 6.11, not in the shared code) and a cold part (miss counts, prototype cell). A dictionary-mode hit keeps its own entry kind. It does not reset the site.
13. Strings: one-byte (Latin-1) and two-byte forms. Short strings keep their characters inline in the cell. Ropes stay for long concatenations, as today (`heap.rs:1312`); round 2 corrected "no ropes". A concatenation census in S1 checks them.
14. Atoms from source text are interned once per session and shared by all realms (an atom has no realm state). Atoms made at run time stay weak and collectable (round 2 scope rule). The main gain is fewer roots and shareable code; interning itself is about 0.25% of instructions (`progress.md`).

Heap and collector:

15. The baseline allocator is size-class blocks with lazy sweep (sweep a block when it is next allocated from). Immix and the young generation must beat it.
16. The marker sets the mark bit before it pushes a cell, and it walks edges as a stream. A fixed-size mark stack, with an overflow scan of the heap, replaces a growing `Vec`. Roots are walked in place, not copied (see the first fact above).
17. A store into a young object that was just allocated does not run the write barrier, only when no collection can happen between the allocation and the store. Round 2 correction: a literal value can call JS and collect, so literals use the one-op fill of section 6.11.

Startup and code cache:

18. Realms start from static recipes: built-in shapes and prototypes are constant data. Each built-in function object is made on first access. Today's lazy prototypes stay.
19. The source identity is computed once in parse-ahead and carried to the compile step, so the shared-code lookup of 6.9 item 6 does not hash the text twice.
20. An optional on-disk code cache keyed by source identity and engine version. Cold runs compare with cold Chrome, and warm runs with warm Chrome (section 10, question 6).
21. Bytecode flushing waits for a measured need. When it comes, a function that ran only once (a "ran once" bit) is the first to flush.

JIT:

22. A function tiers up on calls and on loop back edges, weighted by the ops run, not the call count only. On-stack replacement enters the machine code at a loop header. The frame layout is the same, so this is a jump, not a frame conversion.

Host boundary:

23. Wrapper ownership: a wrapper keeps its expando properties as long as its DOM node is reachable. A detached cycle of node, wrapper, listener and closure is reclaimed. The `IdTab` of node ids and the edges that native code traces stay.

Gates added: S1 records the census in `auto` mode (the massif run of section 3 was in tree mode). S6 repeats the absolute gate of S5 (lower than today's engine and Chrome), not only the JIT-on against JIT-off delta.

### 6.11 Changes from the four-model review, round 2

The same four reviewers read the design again, with section 6.10 in place. I checked their claims against the code. Facts they found:

- The collector is 9.4% of CPU when counted by any collector frame on the stack, not 4.7% (section 3.1). It ranks above parse and calls.
- Today the collector runs only between tasks: `gc_due` requires `depth == 0` and `stmt_frames == 0` (`eval.rs:1126`).
- A compare uses a constant operand only for the right side (`num_konst(right)`, `bc_gen_rf.rs:297`). Minified code puts the constant first (`null!==e`, `0===t`). Measured pairs: `LoadNull` then `GetEnv` 1.37%, `LoadI` then `GetEnv` 1.08% (realm 2).
- The effect-only compile (`expr_void`, `bc_gen_rf.rs:345`) has no case for `&&`, `||` or `?:`, so `null!==e&&f()` makes a value and then tests it. `StrictNe` then `JmpIfNot` is 1.77% of ops.
- `expr_into` (`bc_gen_rf.rs:332`) already writes some results straight into the target register. Section 6.10 item 5 extends it; it is not new.
- The roots `Vec` also copies the name, site and property strings of every script at each collection (`eval.rs:932-934`).
- In the first realm, 84,949 of 824,634 `GetField` runs leave the inline path (10.3%). The 11.8% counts all realms.
- Parse-ahead copies the source text and allocates a job before it checks for a queued duplicate (`parse_ahead.rs:151`).
- Making the wrapper of one DOM node also makes the wrappers of all its ancestors (`browser.rs:1024`).
- The host tracer calls every subsystem for every owner (`Dom::trace`, `browser.rs:676`).
- A shape-backed `for-in` prototype measured -4.80% instructions at 50 keys and -1.67% at load on the typing page (`design-for-in-keys.md`; prototype not kept).

The design takes these changes. Every gain is a hypothesis until its gate measures it.

Collector and roots:

1. Collect between tasks by default, with an empty JS stack, as today. The roots are then the globals, the host's persistent roots and the job queues. This removes a whole kind of work: no scan of JS frames, no root maps, and no handle scopes for most host code. It also makes 6.10 item 4 (no register fill) and item 17 (no barrier on a new object) safe. Decided (section 10, question 11): a task that allocates past the collector trigger also collects inside the task, at the allocation slow path. The compiler emits a root map of live, initialized slots for each safepoint, shared by suspended frames and all tiers. The maps cost compile-time bytes only; no run-time work happens until a collection. Reason: `throws` 10 peaks at 32.4 MiB on Bare against 3.3 MiB on QuickJS (`research-quickjs-reference.md`).
2. Measure in today's engine before S1: survival per collection (objects made since the last collection that survive it), the largest allocation within one task, a `GetEnv` hop-count histogram, and the reads kept in the environment only because of `lazy_inner` (`bc_gen_rf.rs:51`). The young generation and the S1 allocator choice depend on journey lifetimes, which S1 micro rows cannot show. The allocator choice is checked again on the S2 browser slice.
3. The mark work list grows in chunks that are allocated on demand and freed after collection. It does not rescan the heap on overflow. Ephemeron (WeakMap) wake-ups keyed by the key object stay, as today (`eval_weak.rs:36`).
4. Host tracers are registered per owner when it gets host state, so a collection does not probe every subsystem for each owner.

Compiler and bytecode:

5. Constant operands on either side of a compare: swap the operands for `==`, `!=`, `===` and `!==`, and mirror `<` and `>`.
6. Three result contexts in the compiler: value, effect and test (V8's result scopes). In effect context, `&&`, `||` and `?:` compile to jumps, and pure parts are dropped. A comma expression evaluates its non-last parts for effect only.
7. An object literal computes its values first and then allocates and fills the object in one op (`NewObjectFromRegs shape, base, n`). No collection can happen between the allocation and the stores. This removes one `DefField` dispatch per field.
8. Superinstructions from the histogram taken after S2, before the JIT. Measured today: fusing 8 pairs removes 17.7% of dispatches, 32 pairs remove 26.2% (first realm). The pair mix changes after the new compiler, so it is counted again.
9. If the hop histogram shows that most `GetEnv` reads go up to the module function (webpack factories), a closure or frame keeps a direct pointer to the module context: one load instead of a parent walk.
10. A `for-in` over a plain object walks the shape's keys with a cursor and no key `Vec`. In the loop, `src[k]` reads the slot directly while the receiver, the key and the shape still match.

Code, caches and realms:

11. Bytecode and baseline JIT code hold no heap pointers. Each closure and realm has a feedback vector (cache records, per-realm constants), reached from the frame record (V8's feedback cell). The code is then shared by all realms and can go to a disk cache later. This settles the conflict between 6.9 item 6 and 6.10 items 7 and 12.
12. The object header is a 32-bit shape id and 32 bits of kind, flags and collector state (JavaScriptCore's cell layout), compared in S1 with a raw 8-byte shape pointer. With ids, a cache compares ids and the marker does not read the shape.
13. S1 compares two cache designs with the shapes-per-site census: one entry per site plus a global (shape, key) table, against a site that grows to 4.
14. The overflow pointer is reserved only in layouts of objects that grow. S1 compares this with a pointer in every object.
15. Shapes that differ only in prototype or inline capacity share one descriptor table, as today's shared key tables do (`heap_shape.rs:78`).

Front end and host:

16. Parse-ahead claims the (source identity, parse mode) entry before it copies the text. One worker publishes the bytecode and the lazy data; a duplicate is never parsed.
17. Only the wrapper that is asked for is made. The collector follows native reachability through nodes that have no wrapper (WebKit's opaque roots). Expandos and listeners of live nodes stay.
18. DOM wrappers are plain objects with getters on shared prototypes. A cache entry kind calls a native getter. This is an S4 option, measured by `GetField` on wrapper receivers.

JIT:

19. The first JIT enters at calls only. On-stack replacement at loop headers comes later, if a profile after S2 shows long loops that pay for it. The compiler records loop headers, so the JIT needs no loop-discovery pass.

Lower value than round 1 assumed: realm recipes (6.10 item 18; a realm costs about 70 K to 166 K instructions, `progress.md`) stay as today's lazy built-ins, with no stage item. Eager compile (6.10 item 10) does not catch webpack module functions, which are not wrapped in parentheses; it stays low priority.

Stage changes: S2 gains a browser slice gate (the typing page and a frozen copy of the staging scripts, through the real host interface). The measurements of item 2 come before S1. The typing page gets a subsystem profile before S5.

### 6.12 Owner decision: build for the daily workload (2026-11-18)

The daily workload is the staging journey, the typing page and the public journeys. The engine is built and tuned for it. Two measured facts support this:

- On short, cold code, a JIT does not pay back its start cost. Node with V8's JIT starts with about 94 M instructions and 46 MiB, more than the whole run of most `eval_load` cases on QuickJS or Bare (`research-quickjs-reference.md`). The earlier bound of a JIT on today's engine was 5.5% to 12.9% (`progress.md`, 2026-11-17).
- QuickJS, an interpreter with no JIT, is 1.6 to 3.8 times below today's `auto` on instructions. The interpreter and the object model hold most of the gain.

What changes:

1. The speculating JIT (option C of 6.6) is checked last, in S8. It costs feedback memory, code memory, compile time and deoptimization, and it gains only on long hot loops. S8 profiles the daily journeys on the finished engine and builds it only if hot loops are a large share.
2. The baseline JIT (S6) is optional. It is built only if a profile of the daily journeys on the new engine (end of S5) shows hot code that it would speed up by more than its gate. The interpreter is a full engine on its own.
3. A feature census comes first (S0): run the daily journeys on today's engine and count which syntax kinds, built-in objects and built-in methods run. The new engine ports the features in the census. A feature outside it throws a `TypeError` that names the feature and is logged, so that a new journey shows what to port next. The census runs again when a journey is added.
4. test262 gates cover the features in the census, at a pinned revision, test by test against the old engine.
5. Groups that the census does not show wait until a journey needs them (hypothesis: for example `Intl`, `Proxy`, typed arrays and `with`; the census decides).

## 7. Recommended architecture

```
source text
  │  lexer (new tokens, same rules)
  ▼
pre-parse (syntax check, capture facts)  ── inner functions stay lazy
  │  first call
  ▼
function tree (temporary) ─► compiler with scope analysis ─► register bytecode   (tree dropped)
                                                               │
                                     interpreter (one loop) ◄──┤  feedback on second use: call counts, cache shapes
                                                               │  hot
                                                               ▼
                                                  baseline JIT, optional (prepass + one emit pass, same frames)

values: 64-bit NaN-boxed      objects: shape* | inline slots   (elements*, overflow* only when needed)
code: source + bytecode shared by all realms of a session
heap: size-class pages or Immix (S1 decides), one typed graph walk per collection;
      sticky-mark young generation and write barrier only if they win
```

Steps, each behind `BARE_JS_EXEC=new`, each with a gate before the next:

| Stage | Builds                                                                                                                                                                                                                                                                                                        | Gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S0    | Host interface trait with the GC contract of section 6.8; the old engine behind it with no change; a feature census of the daily journeys on today's engine (section 6.12)                                                                                                                                    | All journeys pass; instruction delta under 0.5%; journeys pass with collection forced at every allowed safepoint; the census is recorded in `docs/progress.md`                                                                                                                                                                                                                                                                                                                          |
| S1    | Values, heap, collector (size-class blocks with lazy sweep; Immix and a young generation only if the journey survival data favor them), strings, shapes and objects; unit tests and a stress test. First: record today's `auto` peak bytes by layout (objects, shapes, elements, strings, bytecode, feedback) | The layout census exists before the allocator and the cache width are chosen; allocation and collection micro rows; size-class pages against Immix; the generational heap kept only if it wins; barrier cost, fragmentation (live vs committed bytes) and page release measured; no leaks under the stress test; Miri on the value and heap modules                                                                                                                                     |
| S2    | Lexer, pre-parser, compiler with scope analysis (incl. dynamic scopes and resumable frames by design), interpreter for the core language, and the built-ins that core tests need (Object, Function, Array, Error)                                                                                             | test262 for the core features in the census, at a pinned revision; `eval_load` rows at or below QuickJS on instructions (section 10, question 14); a browser slice (the typing page and a frozen copy of the staging scripts, through the real host interface) measured against today's `auto`: load, 50 keys and first usable input (section 6.11)                                                                                                                                     |
| S3    | The language features in the census (from: classes, generators, async, modules, `eval`, `with`, destructuring, spread, private names), with the Promise and iterator built-ins that they need. A DOM vertical slice: one public journey runs on the new engine                                                | Per-test test262 outcome for the census features compared with the old engine (no test that passes today fails); resource-limit tests kept; the vertical-slice journey passes                                                                                                                                                                                                                                                                                                           |
| S4    | The built-ins in the census, ported group by group. Groups outside the census wait until a journey needs them (section 6.12)                                                                                                                                                                                  | test262 per census group, per test against the old engine; built-in rows of the cost matrix                                                                                                                                                                                                                                                                                                                                                                                             |
| S5    | Browser on the new engine; a profile of the daily journeys decides if S6 runs; if it does, a JIT platform spike (signed macOS arm64 with `MAP_JIT` and instruction-cache flush; Windows x64 calling convention, unwind registration and `FlushInstructionCache`)                                              | Every public journey and the staging journey pass; on the staging journey and the typing page (`type_load` load and 50-key rows), instructions, CPU, peak RSS and time to ready are lower than both today's engine (`auto`) and Chrome, over repeated runs (section 10, answer 1), cold against cold and warm against warm, on a replayed network cache; `eval_load` rows at or below QuickJS on instructions and peak RSS; the spike runs one compiled function on all three platforms |
| S6    | Only if the S5 profile shows hot code that pays for it: baseline JIT, x64 first, then arm64; interpreter-only when executable memory is not available                                                                                                                                                         | Differential fuzz against the interpreter is clean; JIT-on against JIT-off: journey instructions at least 5% lower and RSS within +2%, compile time, code and metadata included; the S5 absolute gate passes again with the JIT on                                                                                                                                                                                                                                                      |
| S7    | Remove the old engine                                                                                                                                                                                                                                                                                         | Every daily journey passes; all gates passed on Linux x64, Windows x64 and macOS arm64; owner decision                                                                                                                                                                                                                                                                                                                                                                                  |
| S8    | Speculating JIT (option C of 6.6), checked last: profile the daily journeys on the finished engine                                                                                                                                                                                                            | Built only if hot loops are a large share of the profile; then the same absolute gate as S5 with it on, and differential fuzz against the interpreter                                                                                                                                                                                                                                                                                                                                   |

Each stage records its numbers in `docs/progress.md` and gets a learning page.

## 8. Risks

- Unported features: a new site can use a feature outside the census. Control: a `TypeError` that names the feature, a log entry, the old engine as a fallback until S7, and a new census when a journey is added.
- Size: the current engine is about 65,600 lines and 19,900 lines of tests. Most of that is built-ins and language features, not the core. S3 and S4 are the long stages (hypothesis: most of the effort).
- `unsafe` code: a GC heap with raw pointers and a JIT need `unsafe`. Keep it in two modules (heap, JIT) with their own tests and Miri runs where Miri can run.
- Correctness drift: two engines can disagree. A differential runner (same script on both, compare output) runs on every PR from S2.
- Memory of the JIT: machine code is larger than bytecode (hypothesis: 3x to 6x). Compile only hot functions, cap the code space, drop cold code.
- The baseline JIT may gain less than Sparkplug did, because V8's interpreter was already tuned. Gate S6 decides.
- GC rooting: host code that keeps a raw pointer across a safepoint is memory corruption. Control: the contract of section 6.8, the forced-collection stress mode, and Miri on the heap module.
- Bytecode flushing: dropping code that is still running or suspended is a crash; flushing too often costs recompiles. Control: pin code of active and suspended frames; count recompiles and measure cold readiness.

## 9. Not measured

- The memory of the source text alone, and the memory of the bytecode in `auto`, by part.
- Instructions per op by op kind on the journey (only the average). A callgrind run of the journey would give it.
- How many shapes each `GetField` site sees (the case for 4-entry caches).
- Windowed runs, paint, other journeys, wall time on an idle machine.
- Any number for the new design. Every gain in sections 1, 6 and 7 is a hypothesis until its stage gate measures it.
- Retired-instruction attribution on a matched replay of the journey (one classifier for all categories). The category split of section 3.1 comes from CPU-time samples and broad name rules.
- Miss reasons per `GetField` site.
- Call allocation: how many calls make an `Env` today.
- The share of collector work memory that the roots copy takes (section 6.10).
- The live object bytes at the peak, without the spare capacity of the doubled slab (section 6.10).
- The peak memory census in `auto` mode (the massif run was in tree mode).

## 10. Questions for the owner

1. Answered (2026-11-18): the new engine must use less CPU and fewer resources than the others: today's engine and Chrome. Proposed default for time to ready, until the owner names another event: the time from start until the first step of the journey can act (the login form accepts input).
2. Is an interpreter-only engine acceptable where executable memory is not available (for example a locked-down macOS or Windows setup)? Answered (2026-11-18): yes; the interpreter is a full engine and the JIT is optional (section 6.12).
3. NaN-boxing (recommended) or compressed 32-bit pointers for the first heap? Decided (2026-11-18): NaN-boxing. For: no boxed doubles (no allocation per non-integer number), one mask to decode a pointer, no 4 GiB cage to reserve on three platforms. Against: 8-byte slots, so objects are larger than with 4-byte slots (V8 measured up to 43% smaller heap). The S1 layout census measures the share of value slots in the peak; compressed slots are tried only if that share is large.
4. Approve the new crate next to the old one (section 6.7, option A)? Decided (2026-11-18): yes. For: the daily journeys keep working, every stage is an A/B on the same build, and the new design is not bent to fit the old one. Against: two engines for a while and one host interface for both.

5. Answered (2026-11-18): yes, S1 records the `auto` peak bytes by layout first. Answered: yes, S5 keeps the typing-page rows next to the staging journey.

From the round 1 review:

6. Is a warm on-disk code cache fair in the comparison with Chrome? Chrome has its own code cache. Decided (2026-11-18): yes, with matched runs. Cold Bare is compared with cold Chrome and warm with warm, and both are reported. For: it removes repeated parse and compile on daily journeys, which load the same bundles each day. Against: disk space and invalidation logic. It is built after S5.
7. May the collector run only where no native frame holds a raw pointer (the safepoints of 6.8), even if a long host call then delays a collection? Decided (2026-11-18): yes. Host code that calls into JS keeps its JS values in handle scopes, so JS safepoints below a host frame stay safe. A long host call without JS in it allocates no JS objects, so the delay costs no memory.
8. May the JIT stay optional per build or per platform, with the interpreter as the full engine? Answered (2026-11-18): yes (section 6.12).
9. Are long sessions (hours, many navigations) in scope? This decides how much flushing and compaction matter. Decided (2026-11-18): memory must stay flat, but flushing and compaction wait for evidence. S5 adds a long-session row: the daily journeys repeated 20 times in one session, with peak RSS per round. Flushing or compaction is built only if that row grows.
10. Which event marks "ready", and do we count one process or the browser with its helpers? Is ready measured on a replayed network cache, so engine time is apart from network time? Decided (2026-11-18): ready is the time until the first step of the journey can act (answer 1). Count the browser with all its helpers, because that is the real cost. Engine comparisons use a replayed network cache; live runs are reported apart.

From the round 2 review:

11. May the new engine collect only between tasks (section 6.11, item 1), and add collection inside a task only when a measured task needs it? Decided (2026-11-18): both. Collect between tasks by default (no stack scan), and inside a task at the allocation slow path when the task passes the trigger, with compiler root maps. For: the low peak of in-task collection (`throws` 10: 32.4 MiB on Bare against 3.3 MiB on QuickJS) and the low cost of between-task collection. Against: root maps add compiler work and must be tested with forced collections.
12. May small measurement and compiler changes land in today's engine before S1 (section 6.11, item 2)? The work is partly thrown away, but it tests the op-count claims early. Decided (2026-11-18): yes, limited to the measurements (feature census, survival per collection, largest allocation in one task, `GetEnv` hop histogram) and two small compiler changes (constants on either side of a compare, the effect context). For: real numbers in days, and the daily journeys get faster now. Against: some throwaway work. Larger compiler changes wait for the new engine.
13. Is the S2 browser slice (the typing page and a frozen staging bundle) the right first performance gate? Decided (2026-11-18): yes. For: it is the daily workload, and it shows host, wrapper and startup costs that `eval_load` cannot show. Against: the host interface must work early (S0 builds it).

From the QuickJS baseline:

14. Should the S2 gate be "at or below QuickJS on instructions and peak RSS on every `eval_load` row"? QuickJS is 1.6 to 3.8 times below today's `auto` (`research-quickjs-reference.md`). Decided (2026-11-18): instructions at or below QuickJS in S2; peak RSS at or below QuickJS by S5, after in-task collection exists. For: a concrete, measured bar from an engine with no JIT. Against: the 2.4 MiB RSS floor of Bare's process may need work outside the engine.

## 11. Review

A reviewer (GPT-6.2 Sol, 2026-11-18) read this design. Its findings changed: sections 1, 3.2 and 3.3 (estimates labeled, the cache count and the memory labels corrected), 6.1 (address, provenance and NaN rules), 6.2 (evacuation, barrier, a mark-sweep baseline), 6.3 (dynamic scopes, resumable frames), 6.4 (cache rules), 6.8 (new: GC safety at the host boundary), 7 (gates) and 8.

A second review (GPT-6.2 Sol, 2026-11-18) on data structures and algorithms added section 6.9 and changed the S1 and S5 gates.

Round 1 of a four-model review (Claude Sonnet 5.5, Claude Opus 5.5, GPT-6.2 Sol, GPT-6.2 Astra; 2026-11-18) added section 6.10, the S6 gate, three rows of section 9 and questions 6 to 10.

Round 2 of the same review (same four models, 2026-11-18) added section 6.11, the collector note of section 3.1, the S2 browser slice and questions 11 to 13. It corrected section 1 item 4, the section 6.2 recommendation, the S1 heap order, items 4, 12, 13, 14 and 17 of section 6.10, and one fact of section 6.10 (the dictionary cache entry).

Owner decision (2026-11-18), after the V8 and QuickJS references: build for the daily workload. It added section 6.12, summary item 8, and changed summary item 5, option C of 6.6, stages S0 and S2 to S7, section 8 and questions 2 and 8.

Decisions on questions 3, 4, 6, 7 and 9 to 14 (2026-11-18): the owner asked for the best choice for speed and resource use, weighed for and against. They changed summary item 5, option C of 6.6, 6.10 item 20, 6.11 item 1, 6.12 item 1, the S2 and S5 gates and the new S8 stage.

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

- One op costs about 210 instructions on average (16.1 G of JS work for 76.3 M ops in `auto` mode). Decode and dispatch is about 23 of them (research of 2026-11-17). So a JIT that removes dispatch alone removes about 10% of the JS work. This matches the old bound (5.5% to 12.9%).
- The heaviest ops are moves and variable reads: `Mov` 12.8%, `GetField` 12.6%, `GetEnv` 12.4%, `SetEnv` 3.3%. Local variables live in heap environments. Each call makes an `Env` with a `Vec` of 24-byte bindings.
- An object costs 104 bytes plus 8 bytes of slot metadata, before any property is stored out of line. A value is 16 bytes. V8 stores an object with two fields in about 20 bytes (hypothesis: from the published layout, not measured here).

Recommendation (section 7): build a new engine crate, `bare-js2`, next to the old one, and switch it on per session with `BARE_JS_EXEC=new`. Its core is:

1. Values in 64 bits (NaN-boxing) with direct pointers into the GC heap. No slot table, no generation check.
2. A compiler that goes from the parse straight to register bytecode, with scope analysis. Locals and arguments live in registers. Only captured variables go to a small context object.
3. Objects as a shape pointer and inline slots, with polymorphic inline caches (up to 4 shapes).
4. A mark-region heap (Immix): bump allocation in 32 KiB blocks of 128-byte lines, non-moving, with sticky mark bits for a young generation.
5. One interpreter (no tree walker). Then a baseline JIT that keeps the interpreter's frame layout (the Sparkplug model), on `dynasm-rs` for x64 and arm64.
6. The syntax tree is dropped after compile. Bytecode of a function that has not run for a while is dropped and compiled again on the next call (bytecode flushing).

Expected result on the journey, all hypotheses until stage gates measure them: JS work down 50% to 65% before the JIT, and another 10% to 20% of what is left with the baseline JIT. JS heap at the peak down 40% to 55% (about 51 MiB to 23 to 30 MiB).

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

### 3.2 What the bytecode runs

76,322,599 ops in 4 realms. 21 ops give 91%.

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
- `Mov` and `GetEnv` together are 25% of all ops. A compiler with locals in registers removes most of both (hypothesis; V8's Ignition reads locals as registers, so the same React code would run neither op).
- Inline caches: `GetField` misses 11.8% (1,137,155 of 9,632,782). `SetField` misses 1.3%. The cache holds one shape. A miss takes the slow path.
- Lane coverage: 98.5% of calls (2,363,990 on the lane, 34,864 on the tree walker).

### 3.3 Where the memory goes

Peak heap 61.6 MiB plus 9.3 MiB of malloc overhead. Peak RSS of normal runs: 83 to 97 MiB. The JS engine holds 50.7 MiB:

| Part                               |  MiB |
| ---------------------------------- | ---: |
| Objects, shapes and property slots | 23.6 |
| Source and syntax trees            | 12.9 |
| Collector work lists at the peak   |  4.9 |
| Scopes, closures and strings       |  4.9 |
| Inline caches (`Caches::new`)      |  3.3 |

`auto` adds 6.7 MiB of peak RSS over `tree` (the bytecode, kept next to the syntax tree; hypothesis, not split by part).

## 4. The current design, and what it costs

| Part          | Data structure now                                                                                                                                      | Cost it causes                                                                                                                                                              |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Value         | `enum Value`, 16 bytes; a string, symbol or object is a handle (slot index and generation)                                                              | Every heap access goes handle → slot table → dense `Vec` → payload, with a generation check. Twice the memory of an 8-byte value in every slot, register and array element. |
| Object        | `struct Object`, 104 bytes: prototype, shape id, flags, `elems: Vec<Option<Value>>`, 3 inline values, then a `Vec` for more, and an optional dictionary | A fourth property moves all values to a `Vec` (one more allocation, one more pointer hop). Arrays pay a `Vec` header and a 16-byte `Option<Value>` per element.             |
| Variables     | `Env { parent, bindings: Vec<Binding>, index: Option<FxMap>, ... }` per call and per block                                                              | One heap environment and one `Vec` per call. A local read is `GetEnv` (hops and index) through the environment chain. Closures keep whole environments alive.               |
| Inline caches | One shape per site, in a side table                                                                                                                     | 11.8% misses on `GetField`. A miss runs the generic lookup.                                                                                                                 |
| Execution     | Tree walker for first calls and refused code, bytecode from the second call                                                                             | Two engines for every feature. Every semantic rule is written twice. The bytecode stays next to the tree, so memory grows.                                                  |
| Front end     | Lexer → arena syntax tree (kept) → bytecode on demand                                                                                                   | The tree stays for the life of the script (12.9 MiB with the source). Lazy bodies are parsed again at the first call.                                                       |
| Collector     | Mark and sweep over slabs, at safe points, non-generational; every collect walks the whole live heap                                                    | The cost of one collect grows with the live heap, not with the garbage. Freed memory goes back to malloc, so allocation is a `malloc` call.                                 |

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

- Sparkplug (baseline JIT, no IR, one linear pass over the bytecode, keeps the interpreter's frame layout): 5% to 15% on real-world benchmarks over Ignition with TurboFan. 41% on Speedometer over Ignition alone (Maglev blog, Chrome 117).
- Maglev compiles about 10 times slower than Sparkplug and 10 times faster than TurboFan.
- Pointer compression: tagged values were about 70% of the V8 heap. Compression made the V8 heap up to 43% smaller and the renderer up to 20% smaller.

What we take from them:

- A good interpreter and a good object model come first. Every engine above has registers or frame slots for locals and an 8-byte (or 4-byte) value.
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

### 6.2 Heap and collector

| Option                                                           | Data structures                                                                                       | Allocation                                                      | Collect cost                                                                                                                                                     | Fit for Rust                                                                                                                                                     |
| ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Immix mark-region, non-moving, sticky marks** (recommended) | 32 KiB blocks of 128-byte lines; a mark bit per object and per line; large objects in their own pages | Bump pointer inside free lines: a few instructions, no `malloc` | Young collections mark only new objects (sticky mark bits and a write barrier on old → young stores). Full collections mark the live heap; sweeping is per line. | Objects never move, so a Rust pointer held across a collect stays valid. Roots: the JS register stack, the globals and an explicit handle stack for native code. |
| B. Copying young generation, mark-compact old (V8)               | Two semispaces for the young generation                                                               | Bump pointer                                                    | Young collection costs the survivors only                                                                                                                        | Every pointer in Rust locals must be rooted and updated. A missed root is memory corruption.                                                                     |
| C. Reference counting with a cycle collector (QuickJS)           | Count in each object                                                                                  | `malloc`                                                        | Pays on every store; cycles need a second pass                                                                                                                   | Simple, but slow on the store-heavy code that React runs.                                                                                                        |

### 6.3 Variables and calls

| Option                                              | How                                                                                                                                                                                        | Effect                                                                                                      |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| **A. Scope analysis at compile time** (recommended) | The compiler knows each name: a register (local, not captured), a context slot (captured by a closure), or a global (cached by shape). A call pushes one frame of registers on a JS stack. | `GetEnv` becomes `Mov` or nothing. No heap environment per call. Closures keep only the captured variables. |
| B. Keep environments, cache lookups                 | Environments as today, with (hops, index) cached per site                                                                                                                                  | Removes the search, keeps the allocation and the hop chain.                                                 |

### 6.4 Objects and property access

| Option                                                                                    | How                                                                                                                                                                                                                           | Effect                                                                                                                                                                                                       |
| ----------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. Shape pointer + inline slots + out-of-line slots; polymorphic caches** (recommended) | Header: shape pointer, prototype in the shape, elements pointer. A fixed number of inline slots chosen per allocation site from the shape's observed size (slack tracking). Caches of up to 4 (shape, offset) pairs per site. | An object `{a, b}` is about 32 bytes (hypothesis: 8 header + 8 elements + 2 × 8 slots). A cache hit is a compare and a load. The 11.8% misses fall, because most React sites see 2 to 4 shapes (hypothesis). |
| B. Keep today's 104-byte object, add polymorphic caches                                   | as today, plus 4-entry caches                                                                                                                                                                                                 | Fixes the misses, not the memory.                                                                                                                                                                            |

Arrays: a dense elements store of 8-byte values with kinds (integers, doubles, any), and holes marked by a reserved NaN value.

### 6.5 Front end

| Option                                                                              | How                                                                                                                                                                                        | Effect                                                                                                                                             |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Parse to a temporary tree per function, compile, drop the tree** (recommended) | Lazy pre-parse (syntax check, no tree) for inner functions. At the first call: parse that function, compile to bytecode, drop its tree. Flush bytecode that has not run for N collections. | The 12.9 MiB of trees goes away (hypothesis: the source text stays, about 4 to 6 MiB on this app, not measured). Simple compiler passes on a tree. |
| B. Single pass, parse straight to bytecode (QuickJS)                                | No tree at all                                                                                                                                                                             | Least memory, but scope analysis and some optimizations need a second pass over the bytecode. Harder to read.                                      |

The lexer, the parse cache and parse-ahead on a worker thread stay as ideas. The code is rewritten to emit the new format.

### 6.6 Execution tiers

| Option                                                                  | Tiers                                                                                                                                                                                                                                                                             | Gain                                                                                                                                                                                                        | Cost                                                                                                                                      |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Interpreter, then a baseline JIT (Sparkplug model)** (recommended) | One register interpreter (no tree walker). A function that runs often compiles to machine code in one pass over its bytecode. Same frame layout, so no deoptimization. Hot ops (field access, compares, jumps, integer math) are inline; the rest call the interpreter's helpers. | Sparkplug: 5% to 15% over a tuned interpreter on real-world pages. Removes dispatch and decode (about 10% of today's JS work; more once the other costs are gone, because dispatch becomes a larger share). | Two assembler back ends (x64, arm64). Code memory (the design caps it and drops cold code).                                               |
| B. A JIT with Cranelift                                                 | Bytecode → Cranelift IR → machine code                                                                                                                                                                                                                                            | Better register use, later a path to an optimizing tier                                                                                                                                                     | The spike of 2026-11-17 (branch `baseline-jit-design`) measured 1.65 M instructions of compile for each inline property op, and +3.0 to 4.9 MB of binary. Better as a later optimizing tier. |
| C. An optimizing tier with speculation (Maglev-like)                    | SSA with type feedback and deoptimization                                                                                                                                                                                                                                         | The largest gain on hot loops                                                                                                                                                                               | Deoptimization, feedback vectors and an SSA compiler. Not before A is measured.                                                           |

JIT tools: `dynasm-rs` (an assembler crate with x64 and arm64), or a small hand-written emitter. Platform rules: Linux `mmap` then `mprotect` (W^X), Windows `VirtualAlloc` and `VirtualProtect`, macOS arm64 `MAP_JIT` with `pthread_jit_write_protect_np`.

### 6.7 How to replace the engine

| Option                                                       | How                                                                                                                                                                                                                                                                                                                      | Risk                                                                                                                       |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| **A. New crate next to the old one** (recommended)           | `crates/js2` with its own value, heap, compiler and interpreter. The browser talks to the engine through one trait (the host interface: DOM wrappers, callbacks, microtasks, modules). `BARE_JS_EXEC=new` picks it per session. Journeys run on both until the new one passes every gate, then the old crate is removed. | Two engines for a while. The host interface must be written once for both.                                                 |
| B. Replace parts of `crates/js` in place                     | Change the value, then the heap, then the scopes, inside the current crate                                                                                                                                                                                                                                               | Every step touches 65,000 lines and both tiers. The value change alone touches nearly every file.                          |
| C. Embed an existing engine (QuickJS through a crate, or V8) | Bind it to the host interface                                                                                                                                                                                                                                                                                            | Fast to mature, but not the owner's design, and V8 conflicts with the memory goal. Kept as a reference to measure against. |

## 7. Recommended architecture

```
source text
  │  lexer (new tokens, same rules)
  ▼
pre-parse (syntax check, scope facts)  ── inner functions stay lazy
  │  first call
  ▼
function tree (temporary) ─► compiler with scope analysis ─► register bytecode   (tree dropped)
                                                               │
                                     interpreter (one loop) ◄──┤  feedback: call counts, cache shapes
                                                               │  hot
                                                               ▼
                                                  baseline JIT (one pass, same frames)

values: 64-bit NaN-boxed      objects: shape* | elements* | inline slots
heap: Immix blocks/lines, bump allocation, sticky-mark young generation, write barrier
```

Steps, each behind `BARE_JS_EXEC=new`, each with a gate before the next:

| Stage | Builds                                                                                                                                                         | Gate                                                                                               |
| ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| S0    | Host interface trait; the old engine behind it with no change                                                                                                  | All journeys pass; instruction delta under 0.5%                                                    |
| S1    | Values, Immix heap, collector, strings, shapes and objects; unit tests and a stress test                                                                       | Allocation and collection micro rows; no leaks under the stress test                               |
| S2    | Lexer, pre-parser, compiler with scope analysis, interpreter for the core language (no generators, async or classes yet)                                       | test262 core subset; `eval_load` rows at least 2x faster than today's `byte` (hypothesis target)   |
| S3    | All the language: classes, generators, async, modules, `eval`, `with`, destructuring, spread, private names                                                    | test262 at today's pass rate or better                                                             |
| S4    | Built-ins, ported group by group (Object, Function, Array, String, Number, Math, JSON, RegExp, Promise, Map and Set, typed arrays, Date, Intl, Proxy, Reflect) | test262 per group; built-in rows of the cost matrix                                                |
| S5    | Browser on the new engine                                                                                                                                      | Every public journey and the staging journey pass; instructions and peak RSS below `auto` of today |
| S6    | Baseline JIT, x64 first, then arm64                                                                                                                            | Differential fuzz against the interpreter is clean; journey gain at least 5%; RSS within +2%       |
| S7    | Remove the old engine                                                                                                                                          | Owner decision                                                                                     |

Each stage records its numbers in `docs/progress.md` and gets a learning page.

## 8. Risks

- Size: the current engine is about 65,600 lines and 19,900 lines of tests. Most of that is built-ins and language features, not the core. S3 and S4 are the long stages (hypothesis: most of the effort).
- `unsafe` code: a GC heap with raw pointers and a JIT need `unsafe`. Keep it in two modules (heap, JIT) with their own tests and Miri runs where Miri can run.
- Correctness drift: two engines can disagree. A differential runner (same script on both, compare output) runs on every PR from S2.
- Memory of the JIT: machine code is larger than bytecode (hypothesis: 3x to 6x). Compile only hot functions, cap the code space, drop cold code.
- The baseline JIT may gain less than Sparkplug did, because V8's interpreter was already tuned. Gate S6 decides.

## 9. Not measured

- The memory of the source text alone, and the memory of the bytecode in `auto`, by part.
- Instructions per op by op kind on the journey (only the average). A callgrind run of the journey would give it.
- How many shapes each `GetField` site sees (the case for 4-entry caches).
- Windowed runs, paint, other journeys, wall time on an idle machine.
- Any number for the new design. Every gain in sections 1, 6 and 7 is a hypothesis until its stage gate measures it.

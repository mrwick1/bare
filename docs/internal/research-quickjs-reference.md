# How QuickJS handles each part of the engine, and a baseline against Bare (reference)

This page is a reference for `design-engine-rewrite.md`, next to `research-v8-reference.md`. It lists how QuickJS solves each module, read from its source, and measures QuickJS, Bare and Node on the `eval_load` cases. It does not change the plan.

Source: QuickJS release 2026-06-04 (`quickjs-2026-06-04.tar.xz` from <https://bellard.org/quickjs/>, MIT license), read on 2026-11-18. File lines are in `quickjs.c` unless stated.

## Design, module by module

- Values: on 64-bit, a 16-byte struct of a union and a tag. NaN-boxing is used only on 32-bit targets (`quickjs.h:57-64`: `JS_NAN_BOXING` when not `JS_PTR64`).
- Objects (`struct JSObject`, line 990): a collector header, flag bits, a class id, a weak-reference count, a shape pointer, a pointer to a separate property array, and a per-class union. All property values live out of line in `prop`.
- Shapes (`struct JSShape`, line 974): the prototype is in the shape. Each shape has its own hash table of keys. Shapes are shared through one hash table for the whole runtime (`shape_hash`).
- Property reads: no inline caches. `get_field` probes the shape's hash table with `find_own_property` on each read, then walks the prototype chain (`GET_FIELD_INLINE`, line 19133).
- Bytecode: a stack machine with 244 opcodes (`quickjs-opcode.h`), 32-bit atom operands, and short forms of common opcodes (`SHORT_OPCODES`).
- Dispatch: computed goto (`DIRECT_DISPATCH` is 1 except on Emscripten, line 51-56).
- Front end: the parser emits bytecode directly, with no syntax tree. Two passes over the bytecode then resolve scopes and labels and do peephole changes (`resolve_variables` line 34070, `resolve_labels` line 34679, `code_match`, `skip_dead_code`).
- Closures: a captured variable is a `JSVarRef` (line 450). While the outer frame runs, it points into the frame's stack slot. When the frame returns, the value is copied into the `JSVarRef` ("detached"). Only captured variables get one. This is Lua's "upvalue" model.
- Strings: 8-bit or 16-bit characters (`is_wide_char`), and ropes (`JSStringRope`, line 601) for concatenation.
- Memory: reference counting frees most objects at once. A cycle collector by trial deletion (`gc_decref`, `gc_scan`, `gc_free_cycles`, lines 6687-6756) frees cycles. It runs when the malloc total passes a threshold: 256 KiB at first, then 1.5 times the size after the last run (lines 1795, 2083).
- Allocator: the 2026-06-04 release added its own allocator for small blocks ("11% faster on bench-v8", `Changelog`).

## Baseline: QuickJS, Bare and Node on the `eval_load` cases

Method:

- Machine: archMachine, AMD Ryzen 9 5900HX, 16 threads, load average about 2.4 during the runs.
- Bare: `crates/js/examples/eval_load` at `d89f0299`, release build, `BARE_JS_EXEC=tree` and `auto`.
- QuickJS: `qjs` built with its Makefile (`-O2`, LTO).
- Node 22.18.0 (V8): with its JIT, and with `--jitless` (the Ignition interpreter only).
- The same 8 programs as `eval_load` at scale 1 and 10 (`gen.py` copies their sources). QuickJS and Node run each program through an indirect `eval` and print the result; their output matches Node on every run. Bare checks its own checksum. An `empty` program (`0`) gives the start cost of QuickJS and Node.
- Instructions: `perf stat -e instructions:u`, the minimum of 3 runs. The spread was under 1% on all rows except Node `fib` 10 (3.2%) and Node `loop` 1 (1.2%).
- Peak RSS: `wait4` through a small static C wrapper, the median of 3 runs (spread at most 0.5 MiB, one Node row 1.6 MiB). The wrapper alone has a floor of 1.5 MiB.
- Each count includes the start and end of the process. Bare's `eval_load` also runs one `collect` after the program. Node starts with about 94 M instructions (127 M without the JIT) and 46 MiB.
- Scripts and raw data: `/data/bare-tmp/quickjs-baseline-2026-11-18/` (`h/gen.py`, `h/bench.py`, `h/rss.c`, `h/rss_all.py`, `results.tsv`, `rss.tsv`, `summary.tsv`).

Instructions, millions (scale 10):

| Case     | Bare tree | Bare auto | QuickJS | Node jitless |  Node | Bare auto / QuickJS |
| -------- | --------: | --------: | ------: | -----------: | ----: | ------------------: |
| loop     |     4,917 |     2,373 |     635 |        1,043 |   186 |               3.74x |
| calls    |     3,465 |     1,603 |     594 |          815 |   148 |               2.70x |
| objects  |     2,369 |     1,935 |     512 |          555 |   333 |               3.78x |
| throws   |     1,885 |     1,395 |     870 |        1,763 | 2,172 |               1.60x |
| closures |       639 |       467 |     280 |          277 |   201 |               1.67x |
| fib      |       164 |        58 |      36 |          157 |   107 |               1.64x |
| webpack  |        38 |        51 |      42 |          169 |   138 |               1.22x |
| concat   |       4.6 |       4.0 |     2.1 |          127 |    95 |               1.86x |

Peak RSS, MiB (scale 10):

| Case     | Bare tree | Bare auto | QuickJS | Node jitless | Node |
| -------- | --------: | --------: | ------: | -----------: | ---: |
| loop     |       5.6 |       5.8 |     3.3 |         43.9 | 51.8 |
| calls    |       5.7 |       5.9 |     3.3 |         43.9 | 51.8 |
| objects  |      11.8 |      11.8 |     6.4 |         50.8 | 59.7 |
| throws   |      32.4 |      33.1 |     3.3 |         49.1 | 57.3 |
| closures |      27.4 |      27.7 |    13.7 |         51.6 | 60.2 |
| fib      |       5.8 |       5.7 |     3.3 |         43.8 | 51.7 |
| webpack  |       7.1 |       7.6 |     4.1 |         44.4 | 47.0 |
| concat   |       6.0 |       5.8 |     3.4 |         43.7 | 46.2 |

Scale 1 shows the same order (`summary.tsv`). The start cost: QuickJS 1.2 M instructions and 3.3 MiB for `empty`; Bare 2.8 M instructions and 5.8 MiB for the smallest case (`concat` 1).

What the numbers show:

- QuickJS, an interpreter with no inline caches, uses 1.6 to 3.8 times fewer instructions than Bare `auto` on every case but `webpack`. Bare `tree` beats it on `webpack` 10 (38 M against 42 M).
- QuickJS uses fewer instructions than V8's interpreter on `loop`, `calls` and `throws`, after Node's start cost is taken off (for example `loop` 10: 635 M against about 916 M). V8's interpreter is ahead on `objects` (about 428 M against 512 M), `closures` (about 150 M against 280 M) and `fib` (about 30 M against 36 M). The two are level on `webpack` (about 42 M each). Hypothesis: V8's inline caches and context slots. Not measured by op.
- V8's JIT wins only on the long loops (`loop` 10, `calls` 10). On short programs its start cost (94 M instructions, 46 MiB) is larger than the whole run of the others.
- Bare's memory floor is 2.4 MiB above QuickJS (5.7 against 3.3 MiB). Cause not measured (hypothesis: binary size, the Rust allocator, the counting allocator of the example).
- `throws` 10: Bare peaks at 32.4 MiB, QuickJS at 3.3 MiB. Reference counting frees each `Error` at once. Bare collects only between tasks, so every `Error` of the loop stays until the end. `closures` 10 shows the same effect at half the size.

## What this means for the design

These are inputs for the owner, not plan changes.

- The S2 gate asks for `eval_load` rows at least 2 times faster than today's `byte` mode. QuickJS is already 1.6 to 3.8 times below `auto` with a plain interpreter. A gate of "at or below QuickJS on instructions and RSS on every `eval_load` row" is a stronger and measurable target (open question).
- Section 6.11 item 1 (collect only between tasks) has a measured cost: a task that makes a lot of short-lived garbage holds all of it to the end (`throws` 10: 10 times the QuickJS peak). Item 2 of 6.11 already measures the largest allocation in one task on the journey. This row says the answer can be large. The young generation or collection at allocation inside a task stays on the table.
- QuickJS keeps captured variables in the frame until it returns (open upvalues), then moves them out. This is an alternative to context objects for section 6.3: a closure that never outlives its frame costs no heap context. It is not compared in the design yet.
- QuickJS reaches its numbers without inline caches or a JIT. This supports 6.10 item 3 (design and measure a lean interpreter first) and the round 2 view that the interpreter, not the JIT, sets most of the gain.

Not measured: per-op instruction costs of QuickJS, the staging journey or the typing page on QuickJS (it has no DOM), wall time on an idle machine, and `eval_load` at scale 100.

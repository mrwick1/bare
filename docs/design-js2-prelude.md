# Design: the cost of the prelude at start (js2, S2.9 decision 1, section 10 of `design-js2-superinstructions.md`)

## 1. Summary

`script::load` compiled the whole prelude (`crates/js2/src/prelude.js`, 14.2 KB) at each start. VM setup was 9.73 M instructions after #768, against 0.8 M without the prelude. Most prelude functions never run in a given script.

Built in this PR: option A, a stub for each prelude function that waits for its first call. Setup is 4.01 M. A function that no script calls costs its pre-parse and nothing else. Option A is the lazy compile of `design-engine-rewrite.md` section 6.10, so it needs no new decision.

Not built, for owner review: what is left. 2.35 M of the 4.01 M is the pre-parse of the 14.2 KB (it reads every function body for early errors and capture facts), and 0.80 M is the compile of the wrapper function that the prelude is. Option B′ (the pre-parse facts as generated data) removes the first. Option B (a compiled image) removes both. Section 5 gives the numbers and section 6 the questions.

## 2. The cost before this change

Measured with callgrind on `script_run` of `var x = 1;` (9.84 M under callgrind, 9.73 M with `perf stat`).

| Part                                                                                 | Instructions |
| ------------------------------------------------------------------------------------ | -----------: |
| `compile_function` of the 43 functions of the prelude and its wrapper                |       3.92 M |
| `parse_function`, the full parse of the same functions (the second read of the text) |       2.26 M |
| Pre-parse of the whole text (`parse_script_lazy`, the first read)                    |       2.35 M |
| `Vm::new` (`scan_flags` decodes every word, `Feedback` for each code)                |       0.60 M |
| Realm, built-in install, process start, run of the prelude body                      |        0.7 M |

The parts add to 9.8 M. Cost by byte: 9.73 M over 14.2 KB is 685 instructions for each byte; compile alone is 275 for each byte. The prelude has 43 functions and compiles to 44 codes: 1,458 words (5.8 KB), 90 sites, 123 atoms and 630 root maps. The code tables are 10.1 KB and the root maps are 10.4 KB.

The loader compiled each function when it loaded it, because the function table of `Vm` is fixed when the `Vm` is made. The compiler could not run inside a running VM.

## 3. Options

All counts are instructions of the whole process unless said otherwise. "Measured" is a run of this branch. "Hypothesis" is an estimate.

### Option A: compile at the first call (built)

Data. A function that waits for its first call has a stub in the program: a `Code` with one word (`Op::Lazy`), no parameters and `CF_ARGS`. `Lazy` (in `script.rs`) holds the source text (`&'static str`), the `Facts` of the pre-parse, the shared atoms and `pending`: `(code index, Child)` sorted by index (a `Child` is the fact id and the context chain of the closure). `Vm::lazy` holds it as a `LazyCompile` trait object, so `interp.rs` does not depend on the compiler.

Algorithm. A call enters the stub as it enters any function. The stub has `CF_ARGS`, so the call makes the array of all arguments in the frame record. `Op::Lazy` calls `LazyCompile::compile`. It parses that one function (`parse_function`), compiles it, puts the code at the same index and makes a stub for each function inside it. Then the interpreter gives the code a new `Feedback`, makes the frame again (the parameters come from the array; the array is dropped when the code has no `CF_ARGS`) and runs from pc 0. A closure keeps its code index, so no closure changes. A later call takes the ordinary path, with no test and no extra word.

Why a stub op and not a flag test in `enter!`: a flag test is 2 to 3 instructions on every call, and the S2.9 work counts them. The stub op costs only the first call, and it costs one array of arguments.

Complexity. Load: O(size of the text) for the pre-parse, plus the compile of the wrapper function. First call of a function: O(its size). Memory: a stub is a `Code` of one word. The `Facts` (43 `FnFacts` of 52 bytes and 145 capture spans: about 3.4 KB) stay for the life of the `Vm`.

Measured (archMachine, release, `perf stat -e instructions:u`, minimum of 3 runs; callgrind for the split):

| Measure                                      | Before                      | After                       |
| -------------------------------------------- | --------------------------- | --------------------------- |
| `script_run` of `var x = 1;`                 | 9.73 M                      | 4.01 M                      |
| malloc: bytes made over the run (DHAT)       | 804 KB                      | 248 KB                      |
| malloc: blocks made over the run (DHAT)      | 4,153                       | 778                         |
| malloc: peak bytes live (DHAT `t-gmax`)      | 90.2 KB                     | 81.8 KB                     |
| peak RSS, 5 runs (launcher with `getrusage`) | 5.15 MiB (5256 to 5300 KiB) | 5.03 MiB (4972 to 5252 KiB) |

The RSS is mostly the floor of the VM: 2.2 MiB for an empty Rust program, and about 3 MiB for the realm and its heap. The prelude is about 0.1 MiB of it. The spread from run to run is as large as the change.

First call of a function (the extra cost over a later call, minimum of 3 runs):

| First call of  | Functions compiled | Extra instructions |
| -------------- | -----------------: | -----------------: |
| `a.forEach(f)` |                  5 |              355 K |
| `a.find(f)`    |                  5 |              352 K |
| `a.flat()`     |                  4 |              430 K |
| `a.sort()`     |                  6 |              944 K |

The average is 58 K instructions for each function (callgrind: 348 K for the 6 functions of one run). `flat` and `sort` cost more for each function because their functions are longer. Before this change the same first call cost 8 K to 18 K more than a later call, but the 5.7 M was paid at the start.

Steady state is the same. 1,000 calls (count at 1,100 calls minus count at 100 calls, divided by 1,000): `forEach` 8,378 before and 8,277 after; `sort` 23,871 and 23,794; `find` 6,898 and 6,903.

Risk. Low. The compile runs in the middle of a run and touches no heap object: it makes atoms in the shared table, which a collection never frees and which has a lock. No collection can happen inside `Op::Lazy` (it has no `gate!`). A compile error becomes a `SyntaxError` that the call throws. `Vm` was not `Send` before and is not now. A function that uses `super` is not made lazy, because the `CF_LINK` flag of a parent depends on its children. The prelude has none.

A finding from the measure: a string literal is a shared atom (`LoadAtom` loads it as it is), but `typeof` gave a run-time atom when its names were not in the shared table at `Realm::new`. The loader used to compile the whole prelude first, so the words were there. With lazy compile they are not, and each `typeof x === 'object'` fell to a comparison of the contents (+690 instructions for a call of `forEach`). The names of `typeof` are now shared atoms always. A test pins it.

### Option B′: the pre-parse facts as generated data (not built)

Data. The `Facts` of the prelude as a generated Rust source file in the crate (about 4 KB), made by an example (`cargo run -p bare-js2 --example gen_prelude_facts`), and a test that makes it again and compares it (CI fails when `prelude.js` changes and the file does not). The parser gets a replay mode: it parses the top level with the known facts and jumps over each function at its start offset. `parse_function` already does this for the functions inside one function (`Facts::find`).

Complexity. Start: O(tokens of the top level), not O(text). The full parse of one function and its compile stay as in option A, so the first call costs the same.

Measured (a prototype that is not in the PR): the replay of the top level of the prelude is 12.4 K instructions, against 2.35 M for the pre-parse. The prototype took the facts from a pre-parse in the same process, so it could not drop that pre-parse in the same run. Hypothesis: setup is 4.01 M - 2.35 M + 0.02 M (the copy of the facts) = about 1.7 M.

Code size. About 25 lines in `parse.rs`, a generator example of about 40 lines and 4 KB of data.

Risk. Medium. A change of the layout of `Facts` needs a new generated file (the test says so). The early errors of the prelude are found when the file is made, and not at each start. That is the point.

### Option B: a compiled image (not built)

Data. At build time, compile the prelude to a serialized `Vec<Code>` (words, numbers, atoms by text, funcs, sites, root maps, handlers, lines) and read it at start. No lex, parse or compile at run time, not even at the first call.

Complexity. Start: O(size of the image) to make the `Code` values. Each `Vec` is a copy. A mapped image with offsets would avoid the copy, but `Code` would have to hold slices.

Hypothesis, from the part of option A that stays: 4.01 M - 2.35 M (pre-parse) - 0.80 M (compile of the wrapper) + the cost to read the image (about 0.1 M for 20 KB) = about 1.0 M, near the 0.8 M that setup costs without a prelude. First call of a function: no compile. Peak memory: all 44 codes are in memory at start (20 KB, measured on the old eager load), unless the image is mapped and a function is read at its first call.

Risk. High. It needs a serializer for each field of `Code` and a verifier run on the image at load: the interpreter trusts `verify`, so an image that this compiler did not make in this process must be verified, or the build must be trusted. The atoms are by text and must be interned at start (or lazily). The root maps and the numbers of the ops are part of the format, so each op that another agent adds changes the image. A test must compare the image with a fresh compile. A build script cannot use the crate that it builds, so the image comes from a separate step with a checked-in generated file (as option B′), or from a `build.rs` that includes the compiler modules by path.

### Option C: built-ins in Rust (not built)

Move the helpers to native Rust and keep only the cold ones in JavaScript.

This does not remove the start cost. The functions that matter most are the methods that take a callback (`forEach`, `map`, `sort`). A native cannot call a script function: the engine has no re-entry from Rust (`Vm::call` needs an empty stack), and this is the reason for the prelude. With option A, a function that no script calls costs no compile, so a native only saves the compile at the first call and some steady-state time, and only for the helpers that do not call back: `search` (3), `pairs` (6), `copyData` (4 and 5), `len`, `int`, `obj`, `check`. Hypothesis: about 1.5 KB of the 14.2 KB, so about 0.25 M of the pre-parse (165 for each byte) and 60 K to 120 K of first-call compile for a typical script. It can be a good change for its own sake, but it is no answer to the start cost.

### Option D: a faster pre-parser (not built)

The pre-parse costs 165 instructions for each byte (2.35 M for 14.2 KB) and the compile costs 275. Both also run on each user script, and the `eval_load` rows at scale 1 are bound by them. A gain here helps the prelude and the user text. It is the parser's own work and not a decision of this document. It is listed because it makes B′ less needed: at 50 instructions for each byte, the pre-parse of the prelude would be 0.7 M.

## 4. Comparison

| Option                | Setup                   | A cold function costs | First call of a function | Peak memory at start                   | Code and data                             | Risk   |
| --------------------- | ----------------------- | --------------------- | ------------------------ | -------------------------------------- | ----------------------------------------- | ------ |
| Before                | 9.73 M                  | its compile           | none                     | malloc peak 90 KB                      |                                           |        |
| A (built)             | 4.01 M measured         | its pre-parse         | 58 K for each function   | malloc peak 82 KB                      | +130 lines of Rust, 1 op                  | low    |
| B′ (A and facts data) | about 1.7 M hypothesis  | nothing               | the same as A            | lower than A (no parse tree)           | +25 lines, +40 lines generator, 4 KB data | medium |
| B (image)             | about 1.0 M hypothesis  | nothing               | none                     | higher than A (all 44 codes) if copied | format, serializer, verifier, 20 KB data  | high   |
| C (native helpers)    | 0.3 M less (hypothesis) | its pre-parse         | none for the helpers     | the same                               | native code, new tests                    | low    |
| D (faster pre-parser) | up to 1.6 M less (hyp.) | its pre-parse         | the same                 | the same                               | the parser                                | medium |

## 5. Recommendation

Build A now (done). The plan already holds this design. It takes setup from 9.73 M to 4.01 M and it costs nothing on a call.

For the rest I suggest B′ as the next step, if the owner wants a start near 1.7 M. It is a small parser change and a generated file, and the compiler stays the only source of bytecode: one format, and no question about a verifier. Option B gains about 0.7 M more and costs a format. I suggest not building B before the S5 decision on the JIT, because a JIT may need the bytecode in another form, and a format that is fixed now could be thrown away.

## 6. Questions for the owner

1. Is a generated, checked-in file (B′) acceptable, with a test that fails when it is old? The other way is a `build.rs` that includes the parser modules by path. I prefer the file: it does not build the lexer and the parser a second time in another crate.
2. Does the start need to be below 4.0 M for the S2 gate? The gate is the net slope (section 10 of `design-js2-superinstructions.md`), and the net slope did not change. A start of 4.0 M still leaves `concat` and `webpack` at scale 1 above the 1.2 M of an empty program in QuickJS.
3. Should the realms of a session share the lazy state? Today each `Vm` holds its own `Facts` (about 3.4 KB) and its own compiled code. The design (6.10) shares source and code by realm. That needs a shared `Program`.
4. Should lazy compile apply to user scripts too, as the design says? It is not done here. A function that is never called would skip its compile. The pre-parse finds every early error today (`parse_function` says that a syntax error in the second pass is a bug), so errors do not move. A function that the compiler does not support yet (`unsupported`) would fail at its first call and not at load. I did not change this behavior.

## 7. Not measured

Windows and macOS. Miri on the new `Op::Lazy` arm (it adds no `unsafe`). Wall time (instruction counts only; the machine had a load average of 11 to 40). A lazy compile inside a getter, a setter, a constructor with fields, or a native's `Outcome::Enter` (all use the same `enter!`; in the new tests only `forEach`, `reduce`, `Array.from`, `Array.of`, `sort` and `map` are first calls, and each runs under constant collection too). The first call on a cold CPU cache.

## 8. Decisions (owner, 2026-11-20)

- Next step: build B′, the generated, checked-in pre-parse facts of the prelude, with a test that fails when the file is old.
- Lazy compile applies to user scripts too. One PR, measured on `eval_load` and the bundle-like program (instructions and peak RSS).
- Shared lazy state and code across the realms of a session: decide later, with S2.8, when the browser slice shows how many realms a page makes.
- Question 2 (start below 4.0 M for the S2 gate) was not asked. The gate stays the net slope.

# Design: less memory for compiled code (2026-11-26)

Status: measured, not built. The owner decides. This document changes no product code.

Owner request (2026-11-26): measure the memory of compiled code on Teams, then write a design. This follows `design-feedback-table.md` section 7. Slice A0 of that design is merged (#1128).

## 1. Summary

- **Compiled code holds about 370 MiB on Teams.** That is 227 MiB of `Code` records and 142 MiB of tables inside them. Teams idles at 1190 to 1242 MiB PSS after #1128.
- **Most of it is per-function overhead, not bytecode.** `size_of::<Code>()` is 384 B. 67% of all codes are stubs (`Op::Lazy`): functions that never ran. They cost 105 MiB of records. The bytecode words cost only 26 MiB.
- **The chunks have 164,194 empty slots (60 MiB).** A lazy compile hands its `Vec<Code>` to `Codes` as a chunk with its spare capacity.
- **Root maps cost 49 MiB (57 MiB with capacity).** About half of the maps repeat inside one function.
- **Code that compiled and never ran costs 3.9 MiB.** So flushing unused code gains little. Section 7 of the feedback design is wrong on that lever.
- **Recommendation, in 4 slices:**
  1. C1: a stub is no longer a `Code` record. Stubs share a few canonical stub codes. The function's own data stays in the lazy compiler. Expected gain: about 125 MiB.
  2. C2: a chunk has no spare room. Expected gain: most of the remaining spare room, about 20 to 40 MiB.
  3. C3: the tables of a code are boxed slices with exact length. Expected gain: about 27 MiB of capacity slack, and a smaller record.
  4. R1: root maps store each distinct map once per code. Expected gain: about 22 MiB.
- **Total expected gain: about 190 to 210 MiB (hypothesis).** That puts Teams idle near 1000 MiB, below Chrome (1198 to 1271 MiB).

## 2. How this was measured

- Machine: archMachine, shared with other agents. Memory only; no wall time.
- Build: main after #1128, `bare-drive` release, with a probe on a scratch branch (not shipped). At `Realm` drop it prints, per realm:
  - the codes, the stubs, the compiled codes, and `Rc::strong_count` of `prog`;
  - the chunk slots in use and in capacity, the free slots and the retired slots;
  - the heap bytes of each `Code` field (capacity times element size), and the length bytes of `words`, `atoms` and the root maps;
  - an estimate of deduplicated root maps: distinct maps times map size, plus 5 B per safepoint (a 4 B `pc` and a 1 B index).
- Journey: `mem-journey.steps` with the saved Teams session. It opens Teams, opens one chat, waits for the editor and idles 20 s.
- Runs: 2. The first probe run gave the field split. The second run added the slack and the dedup fields. The counts of the main realm differ by less than 1% (206,506 and 207,368 codes). Spread is not given for 2 runs.
- Element sizes: `Vec` and `Box<[T]>` capacity only. Allocator size classes are not counted, so the real heap cost is higher.

## 3. The numbers

### 3.1 All realms of the journey (second run)

The journey has 12 realms. Six of them have more than 1,000 codes. `Rc::strong_count(&prog)` is 1 in each realm, so no realm shares code with another.

| Item                                              | Count                | MiB         |
| ------------------------------------------------- | -------------------- | ----------- |
| `Code` records in chunks (capacity)               | 619,971              | 227.1       |
| of which codes in the table                       | 425,623              | 155.9       |
| of which stubs                                    | 286,206              | 104.8       |
| of which free slots                               | 30,154               | 11.0        |
| of which spare chunk room                         | 164,194              | 60.1        |
| Heap tables inside the codes (capacity)           |                      | 142.4       |
| Root maps (capacity / length)                     | 2,848,455 safepoints | 56.7 / 49.2 |
| Root maps, deduplicated (estimate)                |                      | 26.8        |
| `words` (capacity / length)                       |                      | 32.4 / 26.2 |
| `atoms` (capacity / length)                       |                      | 27.2 / 15.2 |
| `lines`                                           |                      | 11.4        |
| `sites`                                           |                      | 6.2         |
| Heap of stubs (`words`, `atoms`)                  |                      | 8.6         |
| Compiled codes that never ran in a nested machine |                      | 3.9         |

The table counts 155.9 MiB for the codes, but chunk "used" slots are 455,777. The difference is the free slots.

### 3.2 Main realm

- 207,368 codes, 137,999 stubs (67%).
- 62,674 chunks. 214,771 slots used, 313,573 in capacity: 98,802 spare slots (36 MiB).
- 1,476,748 safepoints. Root maps 28.2 MiB by length, 32.1 MiB by capacity, about 14.7 MiB deduplicated.

## 4. The current design

- `Code` (`crates/vex/src/bytecode.rs`): 12 `Vec` fields, a boxed slice, `Recipes` (2 `Vec`s), `RootMaps` (a count and 2 `Vec`s) and small scalars. 384 B.
- A stub (`script.rs`, `stub`): a `Code` with one word (`Op::Lazy`), at most one atom (its name), `nregs`, `nparams`, `flags`, `fn_len`, `fn_name`, `span`, `noctor` and `gen_kind`. The other 12 fields are empty. The lazy compiler keeps a `Child` for it.
- `Codes` (`codes.rs`): a table of `*mut Code`. Codes live in chunks. A loader hands over its `Vec<Code>` as a chunk without a copy, so the chunk keeps the spare capacity of the `Vec`. `set` puts the compiled code in a free slot or in chunk room, and retires the stub slot. A retired slot becomes free after `free_retired`. A chunk is freed only when `Codes` drops.
- Root maps (`roots.rs`): for each safepoint, a `pc` (4 B) and a bitmap of the live registers (8 B per 64 registers). The compiler makes them. The collector reads them only for frames on the stack.

Complexity:

- Memory: O(functions × 384 B) for records, also for functions that never run. O(safepoints × 12 B) for root maps.
- Time: a stub costs one `Code` build at load. The root maps cost a build per safepoint at compile. Both are small next to the parse (hypothesis; not measured here).

## 5. How other engines do it

Sources read on 2026-11-26.

- **V8.** A lazy function has a `SharedFunctionInfo` and an `UncompiledData` with only `start_position` and `end_position` (`shared-function-info.tq`). It has no bytecode array. The interpreter entry fills every register with `undefined` (`builtins-x64.cc`, "push undefined as the initial value for all register file entries"). So the collector reads every register of an interpreter frame, and V8 stores no root maps for its interpreter.
- **SpiderMonkey.** A lazy `BaseScript` has no bytecode and only "minimal information" from the syntax parse. Relazification turns a compiled script back into the lazy form under memory pressure (`JSScript.h`). The bytecode and its tables are in `SharedImmutableScriptData`, held by a reference count (`sharedData_`). Hypothesis: it is shared between scripts with equal bytecode; not read in source this round.

The common rule: a function that has not run costs a few words, not a full code record.

## 6. Options

### C1: stubs share canonical codes (recommended)

- Data: the table entry of a stub points to a canonical stub code. The canonical code depends only on `(kept, nparams, flags, noctor, gen_kind)`, so there are a few hundred at most. The program owns them and never retires them. The function's own data (name, length, span) stays in the lazy compiler next to its `Child`, or in `fn_info`.
- Algorithm: a load adds a table entry per stub, not a record. `compile_stub` reads the stub's own data from the lazy compiler. `Codes::set` must not retire a canonical code.
- Memory: 8 B per stub in the table, plus the `Child` that exists today. Gain: 104.8 MiB of records, 8.6 MiB of stub heap and most of the 11 MiB of free slots (they are old stub slots). About 125 MiB.
- Speed: the compiled code path does not change. A load makes fewer records (hypothesis: fewer instructions at load).
- Risk: code that reads a per-function field of a stub through `prog[ci]`. Known readers: `code_info` (name and length, at table growth) and `Function.prototype.toString` (span). To verify: the full list of readers.

### C1-alt: a small stub type in the table

- Data: the table holds an enum: a compiled `*mut Code` or a 16 B stub record.
- Cost: every `prog.at(ci)` must check the kind. That is a branch on the call path of every function.
- Verdict: rejected. C1 keeps `&Code` for every entry and adds no branch.

### C2: no spare room in chunks

- Today: a lazy compile hands over a `Vec<Code>` with spare capacity. Most compiles make a new chunk (62,674 chunks in the main realm).
- Option a: `shrink_to_fit` before `adopt`. Cost: a copy of the codes of each compile.
- Option b: the loader reserves the exact count before it pushes. It knows the count: the function and its children from the facts.
- Recommendation: option b, with option a as a check in debug builds. After C1 the chunks hold only compiled codes, so the spare room falls too. Expected gain: 20 to 40 MiB of the 60 MiB (hypothesis; part of the room is stub room that C1 removes).

### C3: exact tables in a code

- Today: the tables are `Vec`s that grew by push. Slack: `atoms` 12 MiB, `root maps` 7.5 MiB, `words` 6.2 MiB, others smaller.
- Change: the compiler ends each table with `into_boxed_slice`. A `Box<[T]>` has no capacity, and it is 16 B, not 24 B.
- Gain: about 27 MiB of slack. The record falls by about 8 B per table, so by about 100 B (hypothesis: the layout is not computed yet).
- Risk: code that pushes to a table after the build. Known: `stub` pushes its name atom; `interp.rs` sets `roots` of a derived constructor. Both happen before the code goes into the table.

### R1: each distinct root map once per code (recommended)

- Data: per code, a table of distinct bitmaps and, per safepoint, a `pc` and a 1 B or 2 B index.
- Algorithm: the compiler deduplicates with a small hash map at the end of a compile. The collector reads the index, then the bitmap. The binary search on `pc` does not change.
- Gain: 49.2 MiB to about 26.8 MiB, so about 22 MiB.
- Risk: low. The format changes; the rules do not. The verifier checks the maps as today.

### R2: no root maps; fill registers at entry (as V8)

- Data: none. The collector reads every register of a frame.
- Cost: each call writes `undefined` to `nregs - nparams` registers. A dead register keeps its object alive until the next write. `roots.rs` chose maps to avoid this fill.
- Gain: all 49 MiB and the compile work of the maps.
- Verdict: not now. It changes a rule of the collector and costs work on every call. Measure the `calls` row before any decision.

### R3: compute root maps only when a collection needs them

- Data: a cache of maps for codes that were on the stack during a collection.
- Algorithm: the verifier already has the analysis ("a root map names only registers that every path has written", `verify.rs`). The collector runs it for a code on the stack that has no map yet.
- Gain: nearly all 49 MiB. No cost per call.
- Risk: high. A wrong map marks a freed cell. The analysis must give a valid map for every code, not only check one. It also adds work inside a collection.
- Verdict: later. Build R1 first; consider R3 if root maps are still large.

### Ranking

| Option | Gain (MiB, hypothesis) | Work removed   | Risk                              |
| ------ | ---------------------- | -------------- | --------------------------------- |
| C1     | about 125              | stub records   | medium                            |
| C2     | 20 to 40               | spare room     | low                               |
| C3     | about 27               | capacity slack | low                               |
| R1     | about 22               | repeated maps  | low                               |
| R2     | about 49               | all maps       | high: collector rule, call cost   |
| R3     | about 49               | most maps      | high: correctness of the analysis |

The gains overlap: C1 removes stub room that C2 would also remove. The sum of C1, C2, C3 and R1 is about 190 to 210 MiB.

## 7. Not in this design

- **Sharing code between realms.** Four realms load about 58,000 to 65,000 codes each, and none shares `prog`. A realm copies the code cache through `map_atoms` with its own atoms. Sharing needs atoms that do not depend on the realm. That is a larger design.
- **Flushing compiled code.** Code that compiled and never ran in a nested machine is 3.9 MiB. Code that ran once and is now cold is not measured.
- **The `Child` and facts kept for each stub.** The heap profile of 2026-11-26 gave `Loader::children` 56 MiB and parse `close_fn` 43 MiB. To verify how much of it is the `Child`, the facts and the source text.

## 8. Measuring plan after each slice

- The same journey, 3 interleaved runs against main. Idle and peak PSS.
- The probe again: records, chunk room, table slack and root maps.
- The Vex `interp_load` rows and `cost_matrix.sh -g` for Vex, per the cost gate.
- C1 adds a test: a stub's name, length and `toString` before its first call, and `new` on a stub of an arrow.
- Linux and the Windows VM. Miri on `codes.rs` for C1 and C2.

## 9. Open questions for the owner

1. Build C1, C2, C3 and R1 as 4 PRs in that order?
2. Keep R2 and R3 out until R1 is measured?
3. Measure the `Child` and facts memory next (section 7), or code sharing between realms?

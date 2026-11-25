# Design: one lazy feedback table per realm (2026-11-26)

Status: measured, not built. The owner decides. This document changes no product code.

Owner request (2026-11-26): find where the memory of the logged-in Teams journey goes, and write a design for the largest part. Also name the second lever.

## 1. Summary

- **bare uses 18% to 23% more memory than Chrome when idle on Teams.** Idle PSS: bare 1473 to 1495 MiB, Chrome 1198 to 1271 MiB. Peak PSS: bare 1480 to 1558 MiB, Chrome 1557 to 1656 MiB.
- **The largest part is empty feedback vectors.** The jemalloc heap profile gives 1231.5 MB live. Of that, 562.9 MiB comes from `grow_fbs` (`crates/vex/src/interp.rs:667` and `:695`). A probe of the main realm counts 206,157 codes and 8 machines. Each machine has a `Vec<Feedback>` with one 128 B entry per code. Only 4.6% of the entries are used (75,931 of 1.65 M).
- **The cause is the design, not a leak.** Each nesting level has its own machine (`reenter.rs`), and each machine has a full feedback vector. The cost is O(levels × codes × 128 B). The inline caches of one function are also cold again at each level. Each collection walks O(levels × codes) entries.
- **Recommendation: design A1.** Give each realm one table `Vec<Option<Box<Feedback>>>`: 8 B per code. Make the box when the function runs first. All levels share it. Expected gain: about 420 to 520 MiB on Teams (hypothesis). That puts bare idle near 1000 MiB, below Chrome (hypothesis).
- **Build in 2 slices.** Slice 1 (design A0) changes only the element type in each machine. It gets most of the memory gain and has no new aliasing. Slice 2 moves the table to the realm. It removes the cold caches per level and the walks per level.
- **The second lever is compiled code, about 450 MiB.** Section 7 names it. It needs its own measurement and design document.

## 2. How this was measured

- Machine: archMachine, shared with other agents. Memory only; no wall time.
- Build: main `dd32d3d1`, `bare-drive` release. The heap profile used the `profiling` profile (release with symbols).
- Journey: `mem-journey.steps`. It opens Teams with a saved login, waits for the chat list, opens one chat, waits for the editor and idles 20 s. Chrome ran the same steps through CDP (`--headless=new --disable-gpu`, 1280x800) with the same cookies and local storage.
- Runs: 3 per browser, interleaved. A sampler read the PSS of the whole process tree every 250 ms. "Idle" is the sample 3 s before exit. Samples during shutdown are not used.
- Heap profile: jemalloc through `LD_PRELOAD`, `prof:true,lg_prof_sample:17`. Lines were resolved with `addr2line` on the raw `.heap` stacks.
- Probe: a scratch branch (not shipped) prints, at `Realm` drop, the length, capacity and `ready` count of every feedback vector.

### 2.1 Memory table

| Browser | Idle PSS (MiB) | Peak PSS (MiB) |
| ------- | -------------- | -------------- |
| bare    | 1473 to 1495   | 1480 to 1558   |
| Chrome  | 1198 to 1271   | 1557 to 1656   |

malloc in bare: 1261 MB from the system, 1243 MB in use. File pages: 16 MB. So nearly all of the memory is the heap.

### 2.2 Heap profile, live bytes by origin

| Origin                                     | MiB       |
| ------------------------------------------ | --------- |
| `grow_fbs` (feedback vectors)              | 562.9     |
| Compiled code (all sites, section 7)       | about 450 |
| JS shapes and objects                      | about 80  |
| DOM                                        | 26        |
| `Feedback::init` (caches, names, literals) | 25        |
| Style                                      | 13        |
| Images                                     | 8         |

### 2.3 Probe of the main realm

- codes = 206,157. `size_of::<Feedback>()` = 128. machines = 8.
- Length per level: 155,689, 196,966, 196,963, 196,960, 199,820, 206,059, 206,155, 206,157.
- Capacity per level: 155,689, 393,926, 378,876, 256,612, 227,812, 233,840, 302,416, 300,736.
- Used (`ready`) per level: 3, 14, 19, 226, 330, 3,362, 10,947, 61,030.
- Total: 274 MiB of vectors, 75,931 used entries, 16 MiB of side data.

Three other realms have 57,772 to 63,991 codes and 4 to 7 machines: 39 to 73 MiB each. The probe sum is about 431 MiB of vectors and about 30 MiB of side data. The profile gives more (563 MiB) because it counts jemalloc size classes.

## 3. The current design

Data structures:

- `Feedback` (`interp_rt.rs`): 3 boxed slices (`caches`, `names`, `lits`), 3 `Vec`s (`poly`, `protos`, `tmpl`) and `ready`. The size is 128 B, also when empty.
- `Cache`: `shape`, `loc`, `to` (u32 each), `state` (u8), `poly` (u16).
- `Machine` (`reenter.rs:36`): `stack: Vec<Value>` and `fbs: Vec<Feedback>`. `Realm.pool` keeps the idle machines. A nested call (a native calls JS) pops one, and pushes it back after.
- `Vm::new` makes one `Feedback` per code at the start. `grow_fbs` and `compile_stub` resize a vector to `prog.len()`, that is, to all codes of the realm.

Algorithms and cost:

- Memory: O(levels × codes × 128 B), plus the slack of `Vec` growth. The pool never shrinks, so the deepest level reached keeps its full vector.
- Inline caches: each level has its own. A function that a native calls at level 5 starts cold, also when level 0 is warm.
- Collection: `Realm::collect` walks every entry of every machine three times (`walk`, `collected`, `clear`). That is about 1.65 M entries per collection on Teams, for 75,931 that hold data.
- Hot path: `reload!` checks `ci < fbs.len()` and `fb.ready`, then keeps `fb: &mut Feedback` for the frame.

A collection runs only at nesting depth 0 (`Realm::collect` asserts `nest == 0`). So during a collection no nested machine is active.

## 4. How other engines do it

Sources read on 2026-11-26.

- **V8.** A `FeedbackCell` hangs on each closure, and the `FeedbackVector` hangs on the cell. The vector is made only after `invocation_count_for_feedback_allocation` calls (8; `lazy_feedback_allocation` = true, `flag-definitions.h`). All call depths share it. V8 also flushes old bytecode (`flush_bytecode` = true, `bytecode_old_time` = 180).
- **SpiderMonkey.** Each script has one `JitScript` with its `ICScript`. It is made at the warm-up threshold (`baselineInterpreterWarmUpThreshold` = 10, `JitOptions.cpp`). A collection releases it when the script is not active (`JSScript.cpp`, finalize; `ICScript::purgeInactiveICScripts`).
- **JavaScriptCore.** Each `CodeBlock` owns its value profiles and IC data. It is made when the function is linked for its first run. All call depths share it. (Hypothesis: not read in source this round.)

The common rule: feedback is per function, made late, and shared by all depths. No engine keeps a copy per depth.

## 5. Options

### A0: lazy boxes in each machine

- Data: `fbs: Vec<Option<Box<Feedback>>>` per machine. 8 B per code per level.
- Algorithm: as now. `reload!` makes the box when the slot is `None`.
- Memory: 1.65 M × 8 B = 13 MiB for the slots, plus 75,931 × 128 B = 9.3 MiB for the boxes, plus side data. About 40 MiB in place of about 290 MiB for the main realm.
- Gain: most of the memory gain. The caches stay cold per level. The collection still walks O(levels × codes) slots, but each empty slot costs one null check.
- Risk: low. The ownership does not change.

### A1: one lazy table per realm (recommended)

- Data: `Realm.fbs: Vec<Option<Box<Feedback>>>`, one slot per code. `Machine` keeps only its stack.
- Algorithm: `reload!` reads slot `ci`. When it is `None`, it makes the box and calls `init`. The frame keeps a `*mut Feedback`. A box does not move when the table grows, so the pointer stays valid.
- Memory: 206,157 × 8 B = 1.6 MiB, plus at most 75,931 × 128 B = 9.3 MiB, plus side data. About 27 MiB in place of about 290 MiB for the main realm. The used count can be lower than 75,931, because today one function counts once per level.
- Speed: a function is warm at every level. The collection walks O(codes) slots and O(used) boxes once.
- Hot path: one more load (the box) on `reload!` only. The frame keeps the pointer, so each op costs the same as now. To verify with the `interp_load` rows.
- Risk: aliasing across levels. Section 6.

### A1 plus a warm-up threshold

- As A1, but the box is made after N calls (V8: 8, SpiderMonkey: 10). A small counter per code holds the calls.
- Gain on top of A1: at most the side data and boxes of functions that run fewer than N times. That is at most 9.3 MiB plus part of 25 MiB (hypothesis).
- Cost: each IC op must work with no feedback. That touches every cache site in the interpreter.
- Verdict: not now. Measure the used count after A1 first.

### A2: a sparse map per machine

- Data: `HashMap<u32, Feedback>` per machine.
- Memory: low, as A0.
- Speed: a hash on each `reload!`, and caches stay cold per level.
- Verdict: rejected. It is slower than A0 and fixes nothing more.

### A3: a feedback cell per closure (V8 style)

- Data: a pointer in each function object to its feedback.
- Memory: one more word per closure. Teams makes many closures per code.
- Gain: no gain over A1 for an interpreter without a JIT. V8 needs it because closures of one code can have different feedback for its optimizing tiers.
- Verdict: rejected for now.

### Ranking

| Option    | Main realm memory | Caches per level | Walks per collection | Risk   |
| --------- | ----------------- | ---------------- | -------------------- | ------ |
| Now       | about 290 MiB     | cold             | 1.65 M entries       | none   |
| A0        | about 40 MiB      | cold             | 1.65 M null checks   | low    |
| A1        | about 27 MiB      | shared           | 206 K slots          | medium |
| A1 + warm | under 27 MiB      | shared           | 206 K slots          | high   |
| A2        | about 40 MiB      | cold             | per entry            | low    |

A1 removes the work. A0 makes the work cheaper. So A1 is the goal, and A0 is its first slice.

## 6. Safety of A1

- **Aliasing.** Level 0 runs function F and keeps its feedback. A native calls JS, and level 1 also runs F. Today each level has its own entry. With A1 both levels point at the same box. Two live `&mut Feedback` to one box is undefined behavior in Rust. Rule: the frame keeps `*mut Feedback`, and each op makes a short `&mut` that ends before any call that can re-enter. The interpreter already uses this rule for `regs` (`interp.rs`, the `reload!` comment).
- **`init` on a used box.** `Feedback::init` replaces the slices when `caches.len() != code.sites.len()`. An outer level may read the old slices. Rule: `init` runs only on a new box. A replaced code (`compile_stub`) gets a new box. The old box is freed only when no frame holds it. To verify: can a stub compile run while an outer frame runs the same `ci`? Hypothesis: no, because a stub compiles before its first run.
- **Collection.** It runs only at depth 0 (section 3). So no nested frame holds a pointer during `clear`. `clear` does not free boxes; it only resets cache entries.
- **Table growth.** A lazy compile appends codes. The table `Vec` can move, but the boxes do not. A frame never keeps a pointer into the table itself.
- **Checks.** A change to an unsafe module of Vex runs `cargo +nightly miri test -p bare-vex --lib -- <module>::` by hand. Slice 2 adds a test with deep recursion through a native (level 0 and level 3 run the same function and fill the same site).

## 7. The second lever: compiled code

The heap profile gives about 450 MiB to compiled code:

| Site                           | MiB      |
| ------------------------------ | -------- |
| `script.rs` `Lazy::compile`    | 109      |
| `compile_stub`                 | 102      |
| `code_cache` `compile_on_miss` | 99       |
| `Loader::children`             | 56       |
| parse `close_fn`               | 43       |
| `Fc::*` (code generator)       | about 80 |
| bytecode                       | about 23 |
| roots                          | 19       |

Candidate designs (no measurement of each yet):

- **B1: flush old code.** Drop the code of a function that did not run for a time, and keep the stub. V8 does this after 180 s. bare compiles lazily, so the stub path exists.
- **B2: a smaller `Code` layout.** Measure the bytes per field of `Code` (words, sites, atoms, recipes, tables). Pack the fields that most codes leave empty.
- **B3: free parse data after compile.** `Loader::children` and `close_fn` keep 99 MiB. Hypothesis: part of it is parse data that no code reads after the compile.

Next step: a probe of the bytes per `Code` field and per age, then a separate design document.

## 8. Measuring plan after the build

- Same journey, same session, 3 interleaved runs. Idle and peak PSS against the Chrome numbers of section 2.1.
- The probe again: slots, boxes and side data per realm.
- Instructions: the Vex `interp_load` rows and `cost_matrix.sh -g` for Vex, per the cost gate.
- Collection time on Teams: instructions in `Realm::collect` before and after.
- Tests on Linux and on the Windows VM.
- Miri on the changed modules.

## 9. Open questions for the owner

1. Build A1 in 2 slices (A0 first), or A1 in one PR?
2. Leave the warm-up threshold out until A1 is measured?
3. Measure compiled code next (section 7), or first the open Teams bugs?

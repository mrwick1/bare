# Design: the cost of loading large scripts (code that never runs, hashing, page faults)

Status: approved by the owner on 2026-11-24 (section 7). Nothing is built yet.

Base: main `0ccde121`. Machine: archMachine, 2026-11-24. Build: `profiling` profile (release with symbols, frame pointers). Input: the six chunks of the Microsoft Teams data worker, 19.6 MB of minified webpack JS (`precompiled-web-worker-inner*.worker.js`), run with `run_script` in one Vex page. Each number is from 3 runs; instruction counts vary by under 0.01%. Wall times are under a load average of 1 to 9 and are upper bounds.

## 1. Summary

The owner asked (2026-11-24) for a plan that removes the work for code that never runs, makes hashing cheaper and handles page faults. The trigger: bare loads the Teams worker code with 3,883 M instructions, V8 with about 1,920 M. Running JS is 0.2% of that. The rest is reading code.

Recommendations, in build order:

| Step | Change                                                                                      | Measured or estimated gain on the Teams chunks                                                          | Size           |
| ---- | ------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | -------------- |
| 1    | F1. Keep a compile result in the code cache only when the same script is seen a second time | measured: −17.5 MB peak (−19%), −25% page faults, −99 M instructions (−2.6%)                            | S              |
| 2    | H1. A fast hasher (FxHash, written in the crate) for the parser and compiler maps           | measured: −110 M instructions (−2.8%)                                                                   | S              |
| 3    | C1. A code cache on disk, keyed by the script text; plus a fix for the cache that thrashes  | measured for the memory cache: a second load costs 54 M against 2,228 M (−97.6%). Disk read: to measure | M              |
| 4    | F2. One copy of the source text, shared from the download to the cache                      | estimate: −19.6 MB peak, about −24% page faults                                                         | S to M         |
| 5    | C2 or C3. A cheaper skim of function bodies on a first load                                 | estimate: 3.9 G to about 2.0 G (C2) or 2.8 G (C3)                                                       | M (C2), L (C3) |

Rejected with data: transparent huge pages (more kernel time), arena reuse (the faults are first touches, not re-faults).

## 2. Measurement

### 2.1 Where the instructions go

`perf record` at 4,999 Hz, 1,903 samples, self time by part:

| Part                             | Share |
| -------------------------------- | ----- |
| Parser and scopes                | 41.0% |
| Lexer                            | 23.3% |
| Other (memchr, sort, unresolved) | 11.0% |
| Kernel (page faults, file read)  | 8.9%  |
| Compile, bytecode, unit load     | 8.0%  |
| Hashing and atoms                | 4.6%  |
| Allocation and copies            | 2.8%  |
| Interpreter                      | 0.2%  |

69% of the samples have `Parser::parse_function_body` on the stack. These are the lazy functions: webpack wraps each module in a function, and almost none runs at load. Inside those samples:

| Leaf                       | Share of the lazy samples |
| -------------------------- | ------------------------- |
| Grammar and node building  | 42.0%                     |
| Lexer                      | 34.6%                     |
| Scope and capture tracking | 15.6%                     |
| Other                      | 7.8%                      |

Cause, from the code: a lazy body is parsed in full. `function_inner` in `parse.rs` says: "Check the body (every early error is found now) and keep no nodes of it: `parse_function_body` drops them after each statement." The nodes are built, then dropped.

### 2.2 Other engines on the same input

| Engine                       | Instructions                                     | CPU    |
| ---------------------------- | ------------------------------------------------ | ------ |
| bare (Vex)                   | 3,883 M                                          | 0.39 s |
| node 22 (V8), lazy (default) | 2,020 M (1,920 M without the 95 M of node start) | 0.25 s |
| node 22 (V8), `--no-lazy`    | 5,940 M                                          | 0.90 s |
| QuickJS 2026-06-04           | 62,060 M                                         | 4.0 s  |

- V8 parses an inner function with its preparser: the same grammar, but no nodes. It records only what the outer function needs: the end, and the names that the function uses from outside. A function is parsed in full when it first runs. `--no-lazy` shows the size of that saving on this input: 3.1 times.
- JavaScriptCore does the same. Its parser is a template over a tree builder. `ASTBuilder` makes nodes. `SyntaxChecker` makes none and is used for lazy bodies. It also keeps the end of each function in a source cache, so a second parse jumps over it.
- QuickJS compiles every function when it reads the script. That explains most of its 62 G.
- Chrome also keeps a code cache: V8 stores compiled code with the HTTP cache entry of a script after the script is seen again. A warm load then does almost no parse work.

### 2.3 A lexer-only skim (an upper bound for the cheapest skim)

A scratch program (not committed) lexed all 19.6 MB with the Vex lexer and a regexp-or-divide rule from the token before (the rule of a token-level reader), and kept the identifier spans. Result: 5,600,475 tokens, 1,297,275 identifiers, 0 errors, 1,250 M instructions. 39% of that was the scratch loop (a string compare per keyword), so the lexer itself is about 0.7 to 0.9 G (about 37 to 45 instructions per byte, 130 to 160 per token).

### 2.4 Page faults

- 20,117 to 20,417 minor faults per run (603 for an empty run). Peak RSS (VmHWM) 90.1 to 90.7 MB. 20,300 faults of 4 KiB are 79 MiB: the faults are the first touch of each page the process uses. They are not re-faults. So the fault count follows the peak memory.
- Kernel time is about 0.04 s of 0.39 s (zsh `time`, 5 runs). `clear_pages` (the kernel zeroes a new page) is 2.4% of the samples.
- Where the faults happen (`perf record -e page-faults -c 1`, 24,172 faults, by the first bare frame):

| Site                                                                                    | Share |
| --------------------------------------------------------------------------------------- | ----- |
| `script::Lazy::script_in` (compile of the unit, with the source copy into an `Rc<str>`) | 18.5% |
| reading the file (in the browser: the download body)                                    | 15.2% |
| `code_cache::PortableUnit::capture`                                                     | 10.0% |
| `Parser::close_fn` (the facts of each lazy function)                                    | 7.1%  |
| `script::Loader::add`                                                                   | 7.0%  |
| `Facts::clone` (for the cache)                                                          | 4.3%  |
| others, each under 3.5%                                                                 | 37.9% |

- The code cache keeps a second copy of the compile result of each script (source, atom units, facts, pending list, code), also of a script that is never loaded again. Three chunks of 11.3 MB of source give 27.6 MB of cache entries (2.4 times the source).

### 2.5 The code cache today

`code_cache.rs` is a memory cache on the page thread, keyed by the hash and length of the text and the mode. The cap is `MAX_BYTES` = 32 MB. Eviction is least recently used. An entry also goes after 2 idle realms.

- A hit is cheap: the 3 chunks above cost 2,228 M on the first load and 54 M on the second (−97.6%).
- The six Teams chunks need about 37 MB, over the cap. A second pass over all six gives 0 hits and 12 misses: a sequential reuse over a set larger than the cap always misses in LRU order.
- The cache lives in one process. `bare-drive` starts one process for each journey, so a journey never hits an entry of an earlier journey.

### 2.6 Hashing

The parser and compiler maps use the std `HashMap` with SipHash: `atom_ix: HashMap<Vec<u16>, u32>`, `num_ix`, `index` and `cap` (`Cow<str>` keys) in `compile.rs`, and `index` and `declared` in `parse.rs`. `atom_units` also allocates a `Vec<u16>` key and a `Vec<u8>` for each new atom, and takes the `SharedAtoms` lock once for each atom. A scratch build with an in-crate FxHash for the maps of `compile.rs`, `parse.rs` and `compile_module.rs`: 3,883 M to 3,773 M (−2.8%), faults unchanged.

## 3. Code that never runs

### C1. A code cache on disk (recommended first)

**Data.** A directory per profile (next to the cookie store). One file for each entry: a header (format version, bare build id, identity = hash and length of the text, mode), then the portable unit of `code_cache.rs` written as bytes (atom units, facts, pending list, code), without the source text. An index file maps the identity to the file and its last use.

**Algorithm.** On a script load, after the memory cache misses: look up the identity in the index. A hit reads the file, checks the header and compares the text length and a second hash (a collision check without a stored copy of the text), then binds the unit as a memory hit does today. A miss compiles. Admission follows Chrome: the first sight of an identity records it in the index only; the second sight writes the entry; later sights read it. Writes happen after the load, outside the task. A size cap per profile (for example 256 MB) with LRU by last use; this cap protects disk space, not an algorithm.

**Memory-cache fix.** The cap of 32 MB is under the working set of one Teams load (37 MB). Change the eviction so that a set larger than the cap keeps most of its entries (for example: do not insert when the cache is full of entries of the same realm; or size the cap from the realm's own working set). To decide in the build, with the cost matrix.

**Complexity.** A hit: O(size of the unit) to read and bind, no parse. A miss: as today, plus one index entry.

**Expected gain.** Warm loads of the same scripts: the memory hit measured −97.6% on the compile work. A disk hit adds the read and the decode of the unit (to measure; the unit of the 3 chunks is about 16 MB without the source). Teams, Outlook and Gmail load the same bundles on each start until the site deploys a new build, so most real loads are warm.

**Risk.** Medium. A stale or broken file must never run: the header carries the build id and a format version, and any mismatch is a miss. Disk space per profile. A privacy note: the cache holds compiled code of sites the profile visited, as Chrome's does.

### C2. A token-level skim with late early errors

**Data.** No nodes. For each lazy function: its end offset, and the identifier names inside it that are not after `.` (a superset of its captures).

**Algorithm.** When the parser reaches the `{` of a lazy body, a skimmer reads tokens to the matching `}`. It picks the regexp or divide goal from the token before, and rescans `}` as a template tail when a template expression ends. It counts braces. Each identifier that is not a property name goes on the capture list. Nested functions are not recorded; they are skimmed again when the outer function is first parsed in full (as V8 does for a preparsed inner function).

**Early errors.** The skimmer finds no early error inside a lazy body. The error is found when the function first runs and is parsed in full, and it throws then. Chrome throws it when the script loads. Only an invalid script sees the difference: pages that work in Chrome have no early error. This is a deviation from the spec, so it needs the owner's decision. A test262 run shows its size.

**Captures.** The list is a superset (object keys and labels are included). `parse.rs` already allows an extra capture ("an extra name may be listed as a capture, never a missing one"). An extra capture can move an outer variable from a register to the environment: a small run-time cost in the outer function (to measure).

**Complexity.** O(tokens) with a small constant: no nodes, no scopes.

**Expected gain (estimate).** The lazy bodies are 69% of 3,883 M = 2,680 M. A lexer-only skim of all 19.6 MB costs about 0.7 to 0.9 G (2.3). So the total goes from 3.9 G to about 2.0 G, close to V8's 1.9 G. Peak memory goes down too: no node arenas for lazy bodies (to measure).

**Risk.** Medium. The regexp-or-divide choice from one token is wrong for rare code (`a\n/re/` after a keyword-like name, `)` before a regexp after `if (x) /re/.test(s)`). A wrong choice gives a wrong function end. Mitigation: the skimmer stops and falls back to the full lazy parse when it is unsure (after `)` of `if`, `while`, `for`, `with`), and the full parse later checks the end offset that the skimmer gave. Work: medium (a new skimmer of a few hundred lines, and a fallback path in `parse_function` for inner functions without facts).

### C3. A syntax checker with no nodes (the JavaScriptCore and V8 model)

**Data.** The parser becomes generic over a builder: one builder makes nodes, the other returns small tagged values and keeps only what the grammar decisions need (is it a pattern, an assignment target, a call).

**Algorithm.** The same grammar and the same early errors for lazy bodies. No node is allocated. Scope and capture tracking stay.

**Expected gain (estimate).** It removes most of the node building (42% of the lazy samples) but keeps the grammar decisions and the scopes. Estimate: 3.9 G to about 2.8 G.

**Risk.** High for the work: `parse.rs` is 164 KB and every production must work with both builders. The hot path of the full parse must not get slower (generic code is monomorphized, so it should not; to verify on `interp_load` and the cost rows). Exact: no deviation from the spec.

### C0. Parse in the background only (not chosen)

`parse_ahead.rs` already parses on a worker thread while the page waits. It hides wall time but does not cut instructions. It stays as it is.

### Choice

C1 first: it makes the work unneeded on every warm load, needs no change to the parser and no change to the spec. For the first load, the owner chooses between C2 (the larger gain, a spec deviation for invalid scripts only) and C3 (exact, smaller gain, larger build).

## 4. Hashing

### H1. FxHash in the crate (recommended)

A 20-line hasher (the multiply-rotate hash of rustc), in `crates/vex/src`, used by the maps of `parse.rs`, `compile.rs` and `compile_module.rs`. No new dependency. HashDoS: the keys are names from the page's own script, and a hostile script can only slow its own page (as with any slow script). Measured: −110 M (−2.8%). The build script compiles `parse.rs` too, so the hasher module must be reachable from both (a `#[path]` module).

### H2. Remove the hashing in `atom_units` (next)

Use the hash that the atom table computes (`Chars::hash`) as the key of `atom_ix`, and look up by the units without a `Vec<u16>` copy. Intern ASCII names without the `Vec<u8>` copy. Batch the `SharedAtoms` lock for one compile. Gain: to measure (the atom path is about 1.1% of the samples plus its allocations).

### H3. A perfect hash for keywords (small)

`Keyword::from_bytes` is 1.1% of the samples. A table on (length, first byte) or a generated perfect hash. To measure.

## 5. Page faults

The faults follow the peak memory (2.4). So the plan cuts peak memory.

### F1. Cache admission on the second sight (recommended first)

Do not capture a `PortableUnit` on the first compile of a script. Record the identity only; capture on the second sight. Measured with a scratch switch: peak 90.5 MB to 72.9 MB, faults 20,300 to 15,200, instructions −99 M. With H1: 3,674 M (−5.4% in total). This also fits C1 (the same admission rule).

### F2. One copy of the source text

Today the browser holds the decoded body as a `String`, and `compile_on_miss` copies it into an `Rc<str>`. Make the download give an `Rc<str>` once, and pass it to Vex, the parse-ahead job and the cache. Estimate: −19.6 MB of peak on the Teams chunks, about −24% of the faults. To verify: the parse-ahead job copies the text too (`parse_ahead.rs`, "copies the text after that").

### F3. Transparent huge pages (rejected)

`GLIBC_TUNABLES=glibc.malloc.hugetlb=1`: faults 20,300 to 13,000 (−36%), but system time 0.04 s to 0.05 to 0.35 s over 5 runs (the kernel zeroes and compacts 2 MiB pages). Peak RSS unchanged. Worse.

### F4. Reuse arenas between scripts (rejected)

The fault count matches the peak memory (79 MiB of faults, 90 MB peak), so freed memory is already reused by the allocator. Reuse would not cut the first touches.

### F5. Fewer bytes per lazy function (later)

`close_fn` writes the facts of each lazy function (7.1% of the faults). C2 changes this data. Study it with C2.

## 6. Build order and checks

1. F1 and H1 together (small, measured): one PR. Check: the Teams chunks, `interp_load`, the `cost_rows.txt` rows of Vex, 3 runs, both allocators.
2. C1, the disk cache and the memory-cache fix. Check: cold and warm Teams loads, disk hit cost, the journeys with a warm profile.
3. F2. Check: peak RSS on Teams and on the journeys.
4. C2 or C3, after the owner's choice. Check: test262 counts (for C2, the early-error tests that change), the journeys, the cost matrix.
5. H2 and H3 when measured.

## 7. Decisions of the owner (2026-11-24)

1. First load: **C2**, the token-level skim. Early errors in a function that has not run are found when it first runs.
2. Disk cache: 256 MB per profile, next to the cookie store of the profile.

## 8. Not measured

- The cost of a disk hit (read and decode of a unit).
- C2 and C3 gains: estimates from the profile and the lexer-only scratch run.
- F2: an estimate from the fault sites.
- Wall time on an idle machine.
- The journeys and the cost matrix: all numbers here are from the Teams worker chunks only.

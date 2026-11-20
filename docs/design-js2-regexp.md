# Design: the RegExp engine of js2 (S4)

Status: design, revised for the owner's decision of 2026-11-21 (option d, the hybrid). Waits for owner review. No engine code changed. Date: 2026-11-21. Base: main `b0a0df89`.

`crates/js2` has no RegExp engine. `Op::NewRegExp` throws (`interp.rs`: "regular expression literals are not supported yet"). Pages use regular expressions in `String.prototype.replace`, `split`, `match`, `RegExp.prototype.test` and `exec`, so S4 and the browser slice (S5) need one.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "old engine" is `crates/js` (`bare-js`). "js2" is `crates/js2`.
- "run" is one call of the matcher on one input (`Regex::exec` in the old engine). "compile" is pattern text to program.
- "backtracker" is the matcher of the old engine (a backtracking VM). "linear side" is a Pike VM and a lazy DFA over the same program. The time of the linear side is O(program length × input length).
- "memo" is the result table for `test()` of the old engine (`docs/progress.md`, R2-A build 2).
- All instruction counts are `perf stat -e instructions:u`, minimum of 3 runs. The spread of the 3 runs was below 0.01% in every row.

## Decisions (owner, 2026-11-21)

1. Engine: option (d), the hybrid. A linear-time engine runs the patterns that need no backtracking feature. A backtracker runs all other patterns: backreferences, lookaround, and anything else that the linear engine cannot do exactly as ECMAScript says.
2. Bar for each step: no test that passes on the old engine fails on js2.
3. Step budget: charged to `Vm::fuel`, plus a cap of 50 M steps for each run. The cap is for the backtracking side.
4. Compiled patterns live in a new `Blob` cell layout.
5. js2 depends on `bare-unicode` for `\p{}` and case folding. `regexp_check.rs` uses it to check property names.
6. The `test()` memo comes after S5, and only if a profile asks for it.
7. The generic `Symbol.replace` and `Symbol.split` live in the prelude. An unmodified RegExp gets a Rust fast path.
8. The R1 numbers are taken without a look at the 4 census journeys that failed (`curl-se-home`, `duckduckgo-html-search`, `lobsters-comments-back`, `nextjs-docs`).

Sections 1 to 8 are the first pass (2026-11-20). They stay as the record of why. Section 9 works out the hybrid with new measurements. Section 10 is the build plan. Section 11 lists what is still open.

## 1. Summary

The old engine already has a complete, tested matcher in one standalone file (`crates/js/src/regexp.rs`, 2,550 lines). It does not use any JavaScript value. It works on `&[u16]`.

The census of the 48 public journeys (section 3) says what matters:

- 3,513 patterns are compiled on 32 journeys. Only 990 of them (28%) ever run.
- The matcher runs 119,107 times in all 48 journeys, after the memo. At 1.7 K to 3.8 K instructions a run this is about 0.2 to 0.45 G instructions (hypothesis: the product of the run count and the measured cost per run). The 48 journeys cost 43.4 G (`docs/progress.md`, R2-A build 2). Matcher speed is below 1% of the work.
- No pattern uses lookbehind or a named group. 10 use `\p{}`. 19 use the sticky flag `y` (one journey, 2,584 runs).

So the matching speed is not the problem to solve. The problems are: do not compile what never runs, keep a compiled pattern small, keep the semantics exact, and keep catastrophic backtracking bounded.

Chosen: option (d), the hybrid (section 9). The new measurements of section 9 say:

- 94.4% of the census runs (112,437 of 119,107) are on patterns that the linear side can run. 5.6% need the backtracker: 4.9% lookahead, 0.7% backreference, 0.0% `u` or `v` only.
- A lazy DFA gives 3 to 20 times fewer instructions for `test()` after a warm-up of 34 to 60 runs for each pattern. A Pike VM for captures is 2.4 to 6 times slower than the backtracker. So the backtracker stays first for `exec`. The linear side takes over for hot `test()` and when the backtracker runs out of steps.
- The main gain is safety: `(a+)+b` with n = 28 gives a result in 0.5 ms and not a budget error after 120 ms. The speed gain is at most about 0.5% of the 43.4 G instructions (hypothesis).
- The own Pike VM and DFA cost +28 to +35 KB of binary and 13.6 K instructions of compile (about 450 lines in the prototype). The `regex-automata` crate costs +107 KB, 20 times more compile, and cannot do three ES rules exactly (section 9.2).

The build plan (section 10) has 5 steps: R0 baseline, R1 backtracker, R2 linear side, R3 String methods and the Symbol protocol, R4 the `test()` memo if a profile asks.

Options (a), (b) and (c) are kept in section 8 with the reason why they lost.

## 2. How this was measured

- Machine: archMachine (AMD Ryzen 9 5900HX, Linux 7.2.15). Other agents ran at the same time. Only instruction counts are used. Wall time is used only for the catastrophic case, where the difference is 100 times or more.
- Throwaway tools, not in the repository: `/data/bare-tmp/rx-design/` (a bench crate with the old engine, `regress` 0.12.0 and `regex` 1.13.1; a C harness for QuickJS `libregexp` from `quickjs-2026-06-04`; a copy of the tree with a log line in `Regex::new` and `Regex::exec_in`).
- Journeys: the 48 `tools/drive/public/*.steps`, run live on 2026-11-20 with the instrumented copy, `BARE_SEED=1`, 4 at a time. 44 passed and 4 exited with status 1 (`curl-se-home`, `duckduckgo-html-search`, `lobsters-comments-back`, `nextjs-docs`). The partial runs of those 4 are in the numbers. One run each.
- Engine rows (section 6): the same 47 staging patterns (`tools/js/regexp_inventory.jsonl`, unique) and 19 Porter stemmer patterns (the rules of `elasticlunr.js`, written from the published stemmer), on 20 short inputs and 2,000 words. All engines return the same result on all rows: the checksum of all spans is equal (`sum=` values in the logs), except for the `regex` crate (it skips 5 patterns and has Unicode `\w`).
- test262 counts: the old engine, `tools/fixtures/js/test262-js2-skip.txt` rules and an empty skip list, checkout `/data/claude-tmp/test262`. I did not check that the commit equals the pin of `test262-subset.txt`.

## 3. The census

### 3.1 Calls on the 50 workloads of 2026-11-18

From `docs/census/2026-11-18.tsv` (50 workloads: the staging journey, the typing page and the 48 public journeys; total calls, then number of workloads):

| Native or expression                                          |      Calls | Workloads |
| ------------------------------------------------------------- | ---------: | --------: |
| `RegExp.prototype.test`                                       |    217,866 |        24 |
| `String.prototype.replace` (any pattern)                      |     24,857 |        26 |
| `RegExp.prototype.exec`                                       |     24,024 |        20 |
| `String.prototype.split` (any separator)                      |     18,969 |        33 |
| `String.prototype.replaceAll`                                 |     10,937 |         3 |
| `String.prototype.match`                                      |     10,612 |        26 |
| regexp literal evaluated                                      |      5,992 |        34 |
| `new RegExp(...)` / `RegExp(...)`                             |      1,444 |        25 |
| `String.prototype.search`                                     |         29 |         4 |
| `matchAll`, `Symbol.replace`, `Symbol.split` called by script | 0 (no row) |         0 |

The `replace`, `split` and `match` rows count string arguments too. The census cannot tell a regexp argument from a string argument (to verify in S4.R3).

### 3.2 Patterns on the 48 journeys

Source: the log of every `Regex::new` and `Regex::exec_in` (the engine runs after the memo; a memo hit does not call the engine).

| Measure                                           |                               Value |
| ------------------------------------------------- | ----------------------------------: |
| Journeys with at least one compile                |                            32 of 48 |
| Compiles                                          |                               3,513 |
| Unique (flags, pattern) pairs                     |                               1,611 |
| Compiled patterns that ran at least once          |                  990 of 3,513 (28%) |
| Largest count of compiled patterns in one journey | 382 (`substack-home`); only 166 ran |
| Engine runs                                       |                             119,107 |
| Average input length                              |                           267 units |
| Pattern length in characters                      |       median 19, p90 82, max 16,831 |
| Program length in old instructions                |       median 12, p90 56, max 19,029 |
| Runs per unique pattern                           |         median 0, p90 46, max 7,871 |

Where the runs are: `rust-book-search` 64,227, `mdn-reference` 26,973, `substack-home` 7,062, `react-docs` 3,611. The top patterns are the Porter stemmer rules (`^(.+?)e$` 7,871 runs). Lazy quantifiers are in 98 patterns and 47% of all runs (56,534).

Features (unique patterns that have the feature, and the runs of those patterns; a feature is found by a text scan of the decoded pattern, so a count can be off by a few):

| Feature                                     | Unique patterns |   Runs |
| ------------------------------------------- | --------------: | -----: |
| no flag                                     |           1,047 | 90,785 |
| `g`                                         |             232 | 22,121 |
| `i`                                         |             292 |  3,039 |
| `m`                                         |              42 |  1,286 |
| `y` (sticky), one journey (`mdn-reference`) |              19 |  2,584 |
| `u`                                         |              12 |     13 |
| `s`, `d`, `v`                               |               0 |      0 |
| `\p{...}` or `\P{...}`                      |              10 |     13 |
| lookahead `(?=` `(?!`                       |              99 |  6,209 |
| lookbehind                                  |               0 |      0 |
| named group                                 |               0 |      0 |
| numbered backreference                      |              17 |    877 |
| lazy quantifier                             |              98 | 56,534 |
| counted repeat `{n,m}`                      |             137 |  1,994 |
| `\b` or `\B`                                |              77 |  3,630 |
| class `[...]`                               |             843 | 43,194 |
| alternation                                 |             553 | 46,738 |
| capture group                               |             654 | 78,992 |
| anchored at the start (`^`)                 |             617 | 70,839 |

The `u` flag shows on 7 journeys at compile (`github-docs-home`, `mdn-reference`, `nodejs-org-home`, `python-docs-search`, `python-docs-search-results`, `react-docs`, `substack-home`) and almost never runs. Without `u`, `\p{L}` is plain text, so those 10 patterns still need the property tables when they run.

### 3.3 Fixtures and libraries

Regexp literals, found with Acorn on the files that are in `tools/fixtures` and on 6 real libraries (`/data/bare-tmp/s25b/corpus`):

| Source                                                                                                                       | Literals | Unique |
| ---------------------------------------------------------------------------------------------------------------------------- | -------: | -----: |
| 15 staging bundles (`tools/js/regexp_inventory.jsonl`)                                                                       |       57 |     47 |
| `jquery.js` 103, `clangd.js` 135, `lodash.js` 39, `axios.js` 23, `reactdom.js` 5, `webpack-runtime.js` 1, `lex-samples.js` 4 |      310 |    270 |

In the staging bundles: 31 of 47 unique patterns have no flag, `g` 13, `i` 3, lazy quantifier 4, lookahead 2, no lookbehind, no named group, no `\p{}`, no sticky. The libraries add: backreference 7, `m` 1, lookahead 8. They have no `u`, `y`, `\p{}` or lookbehind. These libraries also call `new RegExp(` with a built string (jquery, lodash, clangd, axios: dynamic patterns), so compile must work at run time, not only for literals.

### 3.4 test262 size

| Directory                                                         | Tests |               Old engine, no skip rules: pass / fail |
| ----------------------------------------------------------------- | ----: | ---------------------------------------------------: |
| `built-ins/RegExp`                                                | 1,879 |                                            999 / 880 |
| of which without `property-escapes` (613) and `unicodeSets` (114) | 1,114 |                                            914 / 200 |
| `annexB/built-ins/RegExp`                                         |    62 |                                              11 / 51 |
| `built-ins/String/prototype/match`                                |    51 |                                               43 / 8 |
| `.../matchAll`                                                    |    25 |                                               18 / 7 |
| `.../replace`                                                     |    55 |                                               51 / 4 |
| `.../replaceAll`                                                  |    45 |                                               40 / 5 |
| `.../search`                                                      |    43 |                                               37 / 6 |
| `.../split`                                                       |   120 |                                              113 / 7 |
| `language/literals/regexp` | 240 | baseline file before #790 (pass / fail / skip): old 49 / 52 / 83, js2 52 / 49 / 83 (the 56 `named-groups` tests are a separate row) |

The old failures are, by cause (the 200 failures outside `property-escapes`): 152 "regexp literal: invalid pattern (early SyntaxError)" (the old engine reports these at run time; js2 reports them at parse time since #790), 13 `RegExp.escape` (not built), 12 `$262.createRealm`, 23 on the `Symbol.match`, `Symbol.search` and `Symbol.split` protocol (non-RegExp receivers, RegExp-like objects). The `property-escapes` failures are mostly the runner's step limit (365 `limit: Steps`): each test loops over all code points. With the js2 skip rules (cross-realm and others) the old engine counts `built-ins/RegExp` 576 pass, 10 fail, 1,293 skip.

I did not measure how many of these pass with `property-escapes` given more steps. The recorded work says the property sets are checked against Node (`docs/progress.md`, 2026-11-09).

## 4. The old engine

File: `crates/js/src/regexp.rs` (2,550 lines), wrapper `eval_regexp_exec.rs` (376 lines) and `eval_builtins.rs` (the JS objects, `lastIndex`, the memo). Data: `bare-unicode` (`property.rs`, `props.rs` 387 KB of source, `tables.rs` `CASE_FOLD` and `CASE_FOLD_INV`).

Algorithm and data:

- Parser: pattern as UTF-16 units to a tree (`Node`: `Char`, `Any`, `Class`, `Cat`, `Alt`, `Repeat`, `Group`, `Look`, `BackRef` ...). Annex B rules for non-`u` patterns. `v` set notation. `\p{}` sets are built at compile time from `bare_unicode::property_ranges` as sorted `(u32, u32)` ranges.
- Emitter: tree to `Vec<Inst>`. `Inst` is a Rust enum of at most 12 bytes: `Char(u16)`, `CharI`, `Any`, `Class(k)`, `Split(a, b)`, `Jmp`, `Mark`, `GroupStart`, `Close`, `Reset(lo, hi)`, `EmptyCheck`, `Look`, `BackRef`, `Backward` (lookbehind runs the next instruction backwards), `Match`. A counted repeat is unrolled, so `a{1000}` is 1,000 instructions (`MAX_PROGRAM` is 131,072).
- Matcher: a backtracking VM with an explicit stack of `Frame` (16 bytes: `Branch`, `Restore`, `Look`). It never recurses, so a hostile pattern cannot overflow the Rust stack. A register file `Vec<u32>` (two for each group, one for each loop mark) is cleared at every start position.
- Search accelerators: a set of the ASCII units that can start a match (`FirstSet`, 128 bytes), and an "anchored at `^`" flag.
- Unicode: `u` mode reads code points (`AnyU`, `ClassU`). Case folding without `u` is `Canonicalize` of 22.2.2.7.3 with Rust `to_uppercase`. With `u` it is simple case folding from `bare-unicode`. The oracle shows 3 known differences from Node at U+A7CF, U+A7D3 and U+A7D5 (Unicode version).
- Budget: `Limits { max_steps: 50,000,000, max_input_units: 16 Mi, max_stack_frames: 4 Mi }`. `charge()` counts instructions, scans, register resets, back-reference compares and stack work. A limit is an error, never "no match".
- Result: `Match { spans: Vec<Option<(usize, usize)>> }`, one allocation for each successful run.
- Memory: a compiled pattern is 916 bytes on average (66 patterns, live bytes after compile: 60,496). The scratch (registers and stack) is kept between runs (largest 196,644 bytes).
- Other: a process-wide `AtomicU64` id on each pattern, used as the memo key.

Verification that exists: 57 staging literals against Node on 9,006 inputs, 3 generated corpora of about 20,000 cases each, 846 fixed oracle cases (`tools/js/regexp_cases.tsv`, 1,591 lines), all 65,536 `canon` values, and the test262 rows of section 3.4.

Cost on our rows (section 6): 1.65 K instructions for a `test` of a staging pattern, 3.84 K for a Porter rule on a word, 25.8 K to compile a pattern. The old record agrees: about 4.2 K in the engine for each run on `rust-book-search` (`design-heavy-js-journeys.md`, section 5.1). Its cost shape: the near-miss case is quadratic inside the budget; the `Match` vector is one allocation for each success.

Weak points that a port can fix:

1. `Regex::new` runs when the literal is evaluated, not when the pattern first runs. 72% of the compiles are wasted (section 3.2).
2. The input must be `&[u16]`. A js2 one-byte string would need a copy to wide units.
3. A `Vec<Inst>`, a `Vec<ClassSet>`, a boxed name list and a boxed first set: 916 bytes, many small allocations, owned by Rust and not by the heap.
4. The registers are reset (`nregs` writes and a charge) at every start position that passes the first-unit filter.
5. The step budget is its own counter and not the fuel of the realm.

## 5. How other engines solve it

Sources read on 2026-11-20: <https://v8.dev/blog/regexp-tier-up>, <https://v8.dev/blog/non-backtracking-regexp>, the source of `regress` 0.12.0 (README, `lib.rs`, `api.rs`), the source of QuickJS 2026-06-04 (`libregexp.c`, `libregexp-opcode.h`). Rows marked "not re-read" are from the published design, not checked today.

| Engine              | Design                                                                                                                                                                                                                                                                                                                                                                             | Catastrophic backtracking                                                                                                                                                                                                                                                                             | Unicode and strings                                                                                         |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| V8 Irregexp         | Pattern to bytecode, interpreted at the first run; recompiled to native code at the second run (`ticks` = 1), at once for a global replace with a function and for long subjects (about 1,000 characters or more). Tier-up shipped in v7.9. The interpreter is about 1.45 times faster than the old one on RexBench and it cut the code size of the heap by 4 to 7% on real sites. | An experimental second engine (v8.8, breadth-first, O(pattern length × input length)). Fallback after 50,000 backtracks with `--enable-experimental-regexp_engine-on-excessive-backtracks`, off by default. Only for patterns with no backreference, no lookaround, no `u`, no `i` and no big repeat. | One-byte and two-byte subjects have different code.                                                         |
| SpiderMonkey        | Uses Irregexp (a fork) with its own bytecode and native tiers (not re-read).                                                                                                                                                                                                                                                                                                       | Not re-read.                                                                                                                                                                                                                                                                                          | Not re-read.                                                                                                |
| JavaScriptCore YARR | A bytecode interpreter and a JIT for the same program; patterns the JIT cannot do run in the interpreter (not re-read).                                                                                                                                                                                                                                                            | Not re-read.                                                                                                                                                                                                                      | Not re-read.                                                                                                |
| QuickJS `libregexp` | Pattern to a compact byte-coded program (45 opcodes, 1 to 10 bytes each), run by a backtracking loop with an explicit stack. 3,448 lines of C. No JIT. Reads 8-bit, 16-bit and UTF-16 buffers with one flag.                                                                                                                                                                       | No budget in the engine. The host gives `lre_check_stack_overflow` and `lre_check_timeout` callbacks.                                                                                                                                                                                                 | `libunicode.c` (2,124 lines) and a 247 KB table.                                                            |
| Boa                 | Uses the `regress` crate (not re-read).                                                                                                                                                                                                                                                                                                                                            | Same as `regress`.                                                                                                                                                                                                                                                                                    | Same as `regress`.                                                                                          |
| `regress` 0.12.0    | Parser, IR, optimizer, bytecode emitter, a classical backtracking interpreter (explicit stack) and a "toy" Pike VM for tests. Targets ES2018 plus `v`. Inputs: UTF-8, ASCII, UTF-16, UCS-2 (feature `utf16`).                                                                                                                                                                      | None. No budget, no linear-time fallback. Measured in section 6.                                                                                                                                                                                                                                      | `\p{}` is built in (1 MB of tables). The pattern must be a `&str`.                                          |
| `regex` crate       | Finite automata (lazy DFA, Pike VM, one-pass, bounded backtracker). Linear time.                                                                                                                                                                                                                                                                                                   | By design.                                                                                                                                                                                                                                                                                            | No backreference, no lookaround. `\w`, `\d`, `\s` are Unicode by default. UTF-8 input only. Not ECMAScript. |
| `fancy-regex`       | Wraps `regex` and adds backtracking for backreferences and lookaround (not measured: it depends on `regex` and is not ECMAScript either).                                                                                                                                                                                                                                          | Backtracking part: a configurable limit (not re-read).                                                                                                                                                                                                                                                | As `regex`.                                                                                                 |

What we take from them:

- V8 interprets a regexp first and compiles it only when it runs again. This is "do not pay for what never runs", the same as our lazy compile (section 3.2).
- V8 and QuickJS keep the program as bytes in a flat buffer. A flat program is small (QuickJS: 120 bytes on average for our 66 patterns) and easy to put in a heap cell.
- V8 falls back to a linear engine only for patterns with no backreference and no lookaround. These are the same two features that `regex` lacks, so a hybrid has the same hole (section 8, option d).

## 6. Measurements on our patterns

All rows use the same 20 inputs or 2,000 words, `test` means the first match only, `exec` means all capture spans are read. Compile counts include the 66 patterns.

| Row (calls)                                                   | Old engine | `regress` 0.12.0 |                QuickJS `libregexp` |                    `regex` crate |
| ------------------------------------------------------------- | ---------: | ---------------: | ---------------------------------: | -------------------------------: |
| 47 staging patterns, `test` (282,000) instructions per call   |      1,653 |            3,672 |                              5,355 | 373 (43 patterns, 258,000 calls) |
| the same, `exec` with all spans (94,000)                      |      1,668 |            3,700 |                              5,373 |                              838 |
| 19 Porter rules on words, `test` (190,000)                    |      3,842 |            3,515 |                              3,436 | 429 (18 patterns, 180,000 calls) |
| the same, `exec`                                              |      3,841 |            3,511 |                              3,451 |                              982 |
| compile one pattern (19,800 compiles)                         |     25.8 K |           59.2 K |                             13.1 K |        349.8 K (18,300 compiles) |
| compiled size of one pattern (66 patterns)                    |      916 B |          1,474 B |                              120 B |                     not measured |
| allocations, `test` staging row (282,000 calls)               |     29,998 |          527,441 |                       not measured |                     not measured |
| stripped binary, one pattern compiled and run (empty: 346 KB) |    +368 KB |          +818 KB | C, `gcc -O2`: whole harness 137 KB |                        +1,748 KB |

Catastrophic case, `(a+)+b` on `aaaa...` (n units of `a`), no match exists:

|   n | Old engine           | `regress`                                  |
| --: | -------------------- | ------------------------------------------ |
|  20 | 84 ms, no match      | 54 ms, no match                            |
|  24 | 117 ms, budget error | 871 ms                                     |
|  28 | 121 ms, budget error | 13,735 ms (16 times for each 4 more units) |

What the table says:

- The old engine is the fastest backtracking engine on short tests (the first-unit set skips most start positions) and on par on Porter rules (3.4 K to 3.8 K for all three; the cost is the lazy `.+?` before an alternation).
- `regress` is 2.2 times slower than the old engine on short tests, 2.3 times slower to compile, allocates 18 times more, has no budget and no sticky flag (`y` is ignored), and adds 818 KB.
- QuickJS compiles in half the instructions and keeps a pattern in one eighth of the bytes. Its staging rows are slow. Hypothesis: it has no first-unit set (I found none in `libregexp.c` by search; not tested).
- The `regex` crate shows what a linear engine gives on a run: 4 to 9 times fewer instructions for `test`. It costs 14 times more to compile and 1.75 MB. The rows skip the patterns it refuses (4 of 47 and 1 of 19) and give a different answer for `\w` (Unicode). They also use UTF-8 input, so a js2 string would need a conversion (not measured). They are a lower bound of a linear engine, not a design.

Limits of these numbers: one machine, 20 inputs and 2,000 words that I chose (the words come from `docs/*.md`), the Porter patterns written from the published stemmer and not copied from the page, no memo in any row, and no JS call overhead. The compile counts do not include the parse of the pattern by the lexer.

## 7. What js2 needs from the engine

### 7.1 Strings

js2 strings are inline cells: `OneByte` (Latin-1), `TwoByte`, and `Rope` / `Indirect` (`string.rs`). A substring is a copy (no slice cells).

- A rope must be flattened before a run (`Str::flatten`, O(n) once, cached by `Indirect`). The matcher then reads a plain slice.
- The matcher must read `&[u8]` and `&[u16]`. Two ways: copy a one-byte input to a reusable `u16` scratch (about 270 instructions for the average 267 units, hypothesis: 1 instruction for each unit, which is 7% to 16% of a run), or make the loop generic over the unit type (a second copy of the loop in the binary; size not measured). A one-byte input also cannot match a literal of 256 or more, so compile can reject some patterns for it at once (hypothesis; not in the census).
- The matcher does not allocate and does not call the collector during a run. The heap does not move cells, so a slice stays valid for the run.

### 7.2 `lastIndex`, `g`, `y`

`lastIndex` is an own data property of each RegExp object (writable, not enumerable, not configurable). In js2 it is the first inline slot of a fixed shape, so the read is one load. The wrapper does the spec steps of `RegExpBuiltinExec`: `ToLength(lastIndex)` (a small-integer fast path), the search from that index, `lastIndex = end` for `g` and `y`, and `0` after a failure. The matcher only knows `y` (match at the index only) and the start index, as in the old engine. 19 sticky patterns run 2,584 times in the census, so `y` must not search forward and then check the position (that is how `regress` would have to do it).

### 7.3 The `exec` result

An array with `index`, `input`, `groups` (and `indices` for `d`). It is built from a cached shape, so the property adds are not paid at each call. The capture strings are copies. `test` makes no array and no string. The old engine's `Match` vector is replaced by a caller-owned slice of `u32` pairs (no allocation for each run).

### 7.4 Named groups and `d`

0 uses in the census. The old matcher supports both. Named groups need a `groups` object with null prototype; `d` needs the `indices` array. Both are built from the same span slice. They come after the first S4 pass (section 10, S4.R3).

### 7.5 `Symbol.replace` and `split` protocol

The census (section 3.1) shows `replace`, `split`, `match`, `test`, `exec`, and `search` with a RegExp. It shows no script-defined `Symbol.replace` or subclass `exec`. test262 has many tests of that protocol (about 23 of the old failures are there).

- Fast path in Rust: the argument is a RegExp object with the unmodified shape, and `RegExp.prototype` still has its original `exec`, `flags` getters and `Symbol.*` methods. One realm flag says so; a write to `RegExp.prototype` or a subclass clears it. The fast path runs the matcher in a loop with no property reads.
- Generic path in the prelude (JS, lazy stubs as in `design-js2-prelude.md`): the spec algorithms of `RegExp.prototype[Symbol.replace]`, `[Symbol.split]`, `[Symbol.match]`, `[Symbol.matchAll]`, `[Symbol.search]` calling `exec` by property lookup. Cost when unused: its pre-parse only. This path needs well-known symbols (an S4 `Symbol` item; not checked whether it is built).

### 7.6 Unicode tables

`\p{}` needs General_Category, Script, Script_Extensions and the binary properties by exact name. `bare_unicode::property_ranges(name, value)` does this with case-sensitive names and sorted ranges, and the old engine already uses it. js2 uses `icu_properties` (compiled data) only for ID_Start and ID_Continue in the lexer. Recommendation: js2 depends on `bare-unicode` for `\p{}` and for `fold` (simple case folding). The checker `regexp_check.rs` (#790) then calls `property_ranges(name, value).is_some()` for the names it accepts without a look today. That closes the 242 parse-time tests of `built-ins/RegExp/property-escapes` that #790 lists as accepted (non-existent names and values). The sets are built at compile, never at run (old engine). Cost: `props.rs` and `tables.rs` are in the binary (the old-engine harness binary is +368 KB over an empty one, most of it these tables, hypothesis: not split by symbol).

### 7.7 Garbage collection and the lifetime of a compiled pattern

The js2 heap does not move cells and has no finalizer hook (`gc.rs` has weak tables for the host, no destructor for a cell; checked by grep). A `Box<Regex>` in a cell would leak. Choices:

1. A realm table `Vec<Option<Box<Regex>>>` and an index in a hidden slot. The collector clears an entry after marking when no RegExp object uses it (like the host weak references that `Roots::weak` clears).
2. The program as bytes in a heap cell with its own layout (`Blob`, no children) or in a one-byte string cell that no script can reach. The RegExp object points to it from a hidden slot. The sweep frees it with the object. No Rust ownership, no table, no destructor. Needs a flat program (no `Vec`, no `Box`).
3. A cache keyed by (source atom, flags) so many RegExp objects (one for each evaluation of a literal) share one program. With 2.2 compiles for each unique pattern in the census (3,513 / 1,611), the cache saves compile time, not only memory.

Decision 4 chooses choice 2 plus 3: a `Blob` layout and a cache keyed by (source atom, flags). It is built in S4.R1 (section 10). Choice 1 is not used.

### 7.8 Fuel and the step budget

`Vm::fuel` counts down at loop back edges (`interp.rs`). The old engine charges its own `max_steps` (50 M) for each run. Plan: the matcher charges steps to `Vm::fuel` in blocks (the exchange rate of a matcher step to a fuel unit must be measured: a task of S4.R1, section 9.7). It stops with the same "fuel out" result as a loop. Two reasons to keep a per-run cap too: a regexp in a native callback has no back edge, and the per-run cap bounds the time of one call (120 ms for the 50 M budget in the table of section 6).

CLAUDE.md says that a limit that protects against a slow algorithm is a design problem. For a backtracking engine the limit is the standard answer (V8 has none and can hang; V8's linear engine is still experimental). A linear fallback (option d) is the way to lift it later.

### 7.9 The memo

The old engine's `test()` memo (`R2-A build 2`) took `rust-book-search` from 13,442 M to 10,190 M (-24.2%): 987,649 `test` calls became 64,227 engine runs. js2 needs a memo only if the port of that journey shows the same loop. The key `(program id, from, string identity)` is safe in js2 only with a generation in the id, because a freed cell address can come back (the old engine used a process counter for the pattern and a slot generation for the string). Not in the first S4 steps (section 10, S4.R4).

## 8. The options

Each option has data, algorithm, complexity, memory, code size, and an expected cost on our rows. "Expected" numbers come from section 6 and 3.2, and are hypotheses until a step measures them.

### (a) Port the old engine's matcher into js2

In the hybrid (d), this port is the backtracker side (S4.R1).

Data: as section 4 (`Vec<Inst>`, class sets, register file, `Frame` stack), later flattened into one buffer in a `Blob` cell (S4.R1, decision 4).

Algorithm: backtracking VM, first-unit set, anchored flag. Complexity: O(n) for simple patterns; exponential for nested quantifiers, cut off by the budget (section 7.8). Counted repeats unroll: O(count × body) code.

Catastrophic protection: the step budget, charged to fuel and capped for each run. Measured: `(a+)+b` stops at 117 to 121 ms for any n.

Memory: 916 bytes for each compiled pattern now; with lazy compile the largest journey keeps 166 patterns (152 KB, hypothesis: 166 × 916 B). After the flat image (S4.R1) the size is bounded by the encoding: QuickJS shows 120 bytes on the same patterns (hypothesis for ours; the old class tables are larger than QuickJS's ranges).

Code size: 2,550 lines to port as is. The port changes the input type, the result type, the budget and the program owner: about 400 changed lines (hypothesis: from the list of five weak points in section 4). Binary: about +368 KB with the Unicode tables (measured in the harness), of which the tables are shared with `\p{}` and `fold` that js2 needs under every option.

Cost on our rows: the row of section 6, 1.65 K and 3.84 K per run. The census total is about 0.2 to 0.45 G of 43.4 G. Lazy compile removes 72% of the 3,513 compiles: 3,513 × 25.8 K = 90.6 M becomes 990 × 25.8 K = 25.5 M (the compile of a pattern that runs is paid once; hypothesis: the compile count in the new engine equals the count of runs of the old one).

Conformance: the starting point is known (section 3.4: 914 of 1,114 outside `property-escapes` and `unicodeSets`; the rest are protocol tests that S4.R3 builds).

Risks: the old code has `u32` position limits and `usize` spans; the `Scratch` buffers are kept across calls (peak 196 KB) and need a release rule (a `release()` exists).

### (b) A compact backtracking bytecode (libregexp style) with a step budget on fuel

Data: a byte-coded program (opcodes of 1 to 10 bytes: `char`, `range`, `split_goto_first`, `loop`, `save_start`, `lookahead`, `back_reference`, ...). QuickJS has 45 opcodes. The parser and the emitter are new Rust code; the VM is new.

Algorithm: the same class (backtracking, explicit stack). Complexity as (a).

Catastrophic protection: a counter in the dispatch loop (QuickJS has none; only the host callbacks). A budget on every backward branch.

Memory: 120 bytes for each compiled pattern on our 66 (measured in C). Compile: 13.1 K instructions, half of the old engine.

Code size: QuickJS needs 3,448 lines of C for `libregexp` and 2,124 for `libunicode` (the tables 247 KB). A Rust version that matches the old engine's feature list (Annex B, `v`, lookbehind, named groups, `\p{}`, `i` with `u`) is a rewrite of about 2,500 lines, plus the verification again: the 57 literals, 60,000 generated cases and 846 oracle cases would run again, and test262 would start from zero for this code.

Cost on our rows: QuickJS on the staging row is 5.4 K for each run (3.2 times the old engine). A first-unit set might close this (hypothesis). Porter rows: 3.4 K, no gain over the old engine (-10%). The census saving is the compile: (25.8 K - 13.1 K) × 990 = 12.6 M of 43.4 G (0.03%, hypothesis). Memory saving: 166 × (916 - 120) B = 132 KB on the largest journey (hypothesis).

### (c) Adopt the `regress` crate

Data: its IR and bytecode, the classical backtracking backend.

Algorithm: backtracking, no budget. Complexity: exponential on `(a+)+b` (13.7 s at n = 28, measured). A budget would need a change to the crate (a fork) because the loop is internal.

Measured (section 6): 2.2 times more instructions on short tests, 2.3 times on compile, 1.6 times more bytes for each pattern, 18 times more allocations, 818 KB more binary. 8% fewer instructions on Porter rules.

Gaps: no sticky flag (`y` is ignored; the census has 19 sticky patterns and 2,584 runs); the pattern is a `&str`, so a pattern with a lone surrogate cannot be given; `d` indices are not produced as a flag (the match ranges are); `lastIndex` is not a concept (the caller passes a start); the input as UCS-2 is built with a feature that is off by default. `\p{}` tables are built in (1 MB of source), a second copy next to `bare-unicode`.

Gains: no engine code to write or maintain (45,000 lines in the crate; 1,000 lines of that are tables), a test suite of its own, and Boa uses it.

Cost on our rows: worse than (a) on all rows but one. Not recommended.

### (d) Hybrid: a linear-time automaton for patterns without backreferences and lookaround, backtracking for the rest. CHOSEN (owner, 2026-11-21)

The first pass (2026-11-20) recommended (a) and listed (d) as "measure later". The owner chose (d). Section 9 holds the measured design. This section keeps the first-pass analysis and marks what the new numbers changed.

Data: the program of (a) as the NFA, plus a lazy DFA or a Pike VM over it. First-pass guess: a DFA state is a sorted set of program counters and a transition table of `[u32; 256]` for each state. Measured (section 9.2): a state is a row of byte classes (not 256 entries); a pattern has 7 to 16 states on our rows.

Algorithm: `test()` on a pattern with no backreference and no lookaround runs the DFA from the start index, one transition for each unit. Captures (`exec`, `replace`, `match`) need the backtracker or a Pike VM.

Complexity: O(n) for each run of the DFA; O(program length × n) for the Pike VM. V8's experimental engine is O(pattern length × input length) and also excludes `u`, `i`, backreferences, lookaround and big repeats.

First-pass cost estimate: 2.1% of `rust-book-search` and nothing on the other 47 journeys (hypothesis). The new estimate is in section 9.8. The hole of the hybrid is the same as V8's: patterns with backreferences or lookaround (section 9.8) stay on the backtracker, so the budget stays (decision 3).

### Comparison

|                        | (a) port                       | (b) libregexp-style                                  | (c) `regress`                    | (d) hybrid, chosen                                                |
| ---------------------- | ------------------------------ | ---------------------------------------------------- | -------------------------------- | ----------------------------------------------------------------- |
| Run, staging `test`    | 1.65 K                         | about 5.4 K measured in C (first-unit set not there) | 3.67 K                           | 0.47 K to 0.54 K (own DFA, after warm-up, section 9.2)            |
| Run, Porter `test`     | 3.84 K                         | 3.4 K                                                | 3.5 K                            | 0.22 K to 0.25 K (own DFA, after warm-up)                         |
| Run, `exec`            | 1.65 K, 3.84 K                 | 5.4 K, 3.4 K                                         | 3.7 K, 3.5 K                     | as (a): the backtracker runs first                                |
| Compile                | 25.8 K                         | 13.1 K                                               | 59.2 K                           | as (a) + 1.2 K (Pike) + 13.6 K for the first DFA row set          |
| Bytes for each pattern | 916 B, 120 B flat (hypothesis) | 120 B                                                | 1,474 B                          | as (a) + 2 to 4 KB for a pattern that tiers up to the DFA         |
| Budget on catastrophic | yes, 120 ms                    | to build                                             | none, 13.7 s                     | linear patterns: result in 0.5 ms. Others: budget as (a), 120 ms  |
| Sticky `y`             | yes                            | yes                                                  | no                               | yes                                                               |
| New code               | about 400 changed lines        | about 2,500 lines                                    | 0 (a fork if a budget is wanted) | (a) + about 450 lines (prototype: Pike, DFA, eligibility)         |
| Conformance start      | 914 / 1,114 test262            | zero                                                 | not measured                     | as (a), plus a differential test of the linear side               |
| Census gain over (a)   | none (the base)                | 0.03% of 43.4 G                                      | negative                         | at most about 0.5% of 43.4 G (hypothesis) and bounded worst case  |
| Binary                 | +368 KB (with tables)          | about the same                                       | +818 KB                          | (a) + 28 to 35 KB                                                 |

## 9. The hybrid in detail

Tools of this section (throwaway, not in the repository): `/data/bare-tmp/rx-hybrid/` (a harness with the old engine, an own Pike VM and lazy DFA over its program, `regex-automata` 0.4.19, a differential runner). Copies of the sources and logs are in `/data/bare-tmp/rx-keep/hybrid/`. The census data is the `Regex::new` and `Regex::exec_in` log of section 2 (3,513 compiles).

### 9.1 Shape

One parser, one tree, one program for each pattern. Two executors read the same program:

- The backtracker runs every pattern. It is the old matcher (section 4) with the changes of section 8, option (a).
- The linear side runs the patterns that the emitter flags (section 9.4). It has two parts: a Pike VM (all capture spans, O(program length × n)) and a lazy DFA (yes or no, one table lookup for each unit).

The parser and the emitter exist once. The flags are set at emit time from the tree: no second parse.

### 9.2 The linear engine: `regex-automata` or our own

Both were built and measured on the same rows as section 6. The rows are the patterns that every engine in the table accepts (39 staging patterns, 14 Porter rules; 741 and 28,000 calls for each rep). The cost for each call is the steady cost: (instructions of 100 reps - instructions of 20 reps) / (80 reps × calls). The spread of 3 runs is below 0.002%.

(i) `regex-automata` 0.4.19 with the minimal features (`std`, `nfa-thompson`, and one engine). The NFA is built from the old program with `thompson::Builder`. Limits, from the source and from the differential run:

- It reads bytes. A two-byte input needs another path.
- `m` knows only LF. ES also uses CR, U+2028 and U+2029 for `^`, `$` and `.`.
- It has no "reset the captures of a group at each iteration" rule. The Pike VM and the bounded backtracker of the crate gave 96 wrong spans on 177,594 runs. All 96 are a capture inside a quantified group, for example `(?:(a)|b)*` on `ab`: ES gives group 1 as undefined (the second iteration resets it), an engine without the reset keeps `a`.
- `is_match` was right on all 177,594 runs (hybrid DFA and Pike VM).
- The one-pass engine accepts almost no pattern of the census (not counted; it refused the patterns that I tried).

(ii) Our own Pike VM and lazy DFA over the shared program (prototype: 706 lines with the eligibility walk and helpers; `Pike` about 140 lines, `Dfa` about 250 lines). The DFA state is the ordered list of kernel program counters plus the kind of the previous unit. Assertions (`^`, `$`, `\b`, `\B`) are resolved when a transition is built, with the kind of the previous and the next unit, so the match is reported one unit late. Units below 256 map to byte classes from cut points of the program. Units of 256 or more use a sorted cut table (binary search).

Measured, instructions for each call (steady):

| Engine                              | `test` staging | `test` Porter | `exec` staging | `exec` Porter |
| ----------------------------------- | -------------: | ------------: | -------------: | ------------: |
| old backtracker (the base)          |          1,731 |         4,490 |          1,731 |         4,490 |
| own DFA, u16 input                  |            544 |           245 |              - |             - |
| own DFA, u8 input                   |            469 |           219 |              - |             - |
| `regex-automata` hybrid DFA         |            417 |           326 |              - |             - |
| own Pike VM, u16 input              |         10,923 |        10,720 |         10,923 |        10,720 |
| own Pike VM, u8 input               |         10,505 |        10,438 |         10,505 |        10,438 |
| `regex-automata` Pike VM            |         12,136 |        10,914 |         12,960 |        11,522 |
| `regex-automata` bounded backtracker |             - |             - |          6,380 |         5,286 |

On all patterns that the own engines accept (45 staging, 18 Porter): old 1,596 and 3,924; own DFA (u8) 475 and 206; own Pike (u8) 10,148 and 9,220.

Compile, warm-up, memory and size:

|                                  | Compile, 39 staging / 14 Porter (instructions) | Warm-up for one pattern, staging / Porter (instructions) | Live bytes for each pattern after runs, staging / Porter | Binary (stripped, over old: 710,768 B) |
| -------------------------------- | ---------------------------------------------: | -------------------------------------------------------: | -------------------------------------------------------: | -------------------------------------: |
| old backtracker                  |                                 12.1 K / 12.2 K |                                                        - |                                                  590 / 628 |                                      base |
| own Pike VM (extra)              |                                   1.2 K / 1.2 K |                                                   0.5 K / 0.5 K |                                              1,429 / 2,802 |                              +9.0 KB (u16) |
| own DFA (extra)                  |                                  13.6 K / 5.7 K |                                                 39 K / 260 K |                                              2,045 / 4,405 |                             +19.8 KB (u16) |
| own Pike VM and DFA              |                                   see rows above |                                                  see rows above |                                                  see rows above |   +28.2 KB (u16), +34.9 KB (u8 and u16) |
| `regex-automata` hybrid DFA      |                                 279 K / 111 K |                                                 71 K / 454 K |                                              3,643 / 5,784 |                                   +107.2 KB |
| `regex-automata` Pike VM         |                                 264 K / 96 K |                                                  0.5 K / 0.5 K |                                              3,192 / 4,964 |                                    +74.9 KB |
| `regex-automata` bounded backtracker |                              262 K / 94 K |                                                  1.3 K / 1.3 K |                                              1,828 / 1,712 |                                    +69.6 KB |

Notes on this table:

- "Warm-up" is the first rep minus the steady rep, for one pattern over all its inputs. For the DFA it is the cost of building the states on demand (7.2 states for each staging pattern and 15.9 for each Porter rule).
- The break-even run count for the own DFA is 34 runs (staging: 38.3 K / (1,596 - 475)) and 60 runs (Porter: 224.9 K / (3,924 - 206)). This sets the tier-up threshold of section 9.5.
- Compile here is lower than the 25.8 K of section 6. Hypothesis: the section 6 set has the larger Porter rules and a different harness. Compare rows inside this table only.
- Live bytes are what the harness counts after a run over all inputs. It does not split the program from the scratch (not measured).
- The `regex-automata` binary sizes are the delta over the old engine alone (the old engine stays for the backtracker). The same crate with no old engine: Pike VM +68.0 KB, hybrid +104.3 KB, backtracker +64.4 KB, one-pass +77.5 KB over an empty program (347,312 B).
- The own engine in the first measurement was 25 times worse in compile (343 K) because it allocated a table for each unit. Byte classes from cut points fixed it (13.6 K). The numbers above are after the fix.

Catastrophic case, `(a+)+b` with n units of `a` (wall time; the machine was loaded, so the old engine shows 262 ms here and 121 ms in section 6):

| Engine                                      | n = 28                | n = 4000              |
| ------------------------------------------- | --------------------- | --------------------- |
| old backtracker, budget 50 M                | budget error, 262 ms  | budget error, 276 ms  |
| own Pike VM                                 | no match, 0.01 ms     | no match, 1.38 ms     |
| own DFA                                     | no match, 0.01 ms     | no match, 0.03 ms     |
| backtracker for 100 K steps, then Pike VM   | no match, 0.5 ms      | no match, 1.9 ms      |
| `regex-automata` hybrid DFA                 | no match, 0.06 ms     | no match, 0.14 ms     |

For n = 1,000 the Pike VM runs 16,996 thread steps (17 for each unit).

Result. The two options match on `test()`. Option (i) loses on the rules that ES needs (reset of captures, line terminators, two-byte input), on compile (20 times more), and on binary size (+107 KB against +28 to +35 KB). It gives a safe yes or no only for one-byte input without `m`, and its spans are wrong for 96 runs. Recommended: (ii), our own engine. This is owner question 1.

### 9.3 Exact ES semantics on the linear side

Each rule is a place where a naive automaton gives a different answer from ES. The differential test of S4.R2 (section 10) checks each one.

1. Priority. ES takes the first match by priority, not the longest. The Pike VM keeps its threads in priority order and cuts the lower threads when one thread matches. The DFA state is an ordered list for the same reason (hypothesis: for a yes or no an unordered set would give fewer states; not tried).
2. Capture reset. At the start of each iteration of a quantified group, the captures inside the group become undefined (`Reset(lo, hi)` in the program). The Pike VM restores them with `Restore` entries on an explicit stack. A mutation test confirmed that the differential run catches a missing reset on either slot.
3. Empty check. An iteration of a loop that matches the empty string is rejected (`Mark` and `EmptyCheck`). The DFA drops it, because it cannot change whether a match exists (hypothesis; 0 differences on 8,465 patterns). The prototype gives a nullable loop flag B only: the Pike VM does not run it, because the value of a capture can differ (2 unique patterns, 257 runs, 0.2%). Not tested: whether the Pike VM could do it exactly.
4. Case. `i` without `u` uses `Canonicalize` of 22.2.2.7.3 (upper-case mapping). The table for units below 256 comes from the old engine's `canon`. A two-byte input with `i` goes to the backtracker.
5. `\w`, `\W`, `\b`, `\B`. ASCII only without `u`. The kind of a unit is a word unit or not.
6. Line terminators. `.`, `^` and `$` with `m` use LF, CR, U+2028 and U+2029.
7. Sticky and start index. `y` means: match at the start index only. The DFA start state depends on the kind of the unit before the start index, so `^` and `\b` see the real context. The start states are cached (8 entries).
8. Units. The input is a sequence of UTF-16 units. Outside `u` mode no pair of surrogates is joined.
9. Errors are not "no match". Running out of fuel or of the state cap makes the linear side give up and the caller uses the next engine.

### 9.4 What forces the backtracker

The emitter sets one of three flags for each program.

| Flag | Meaning                                              | Set when                                                                                   |
| ---- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| NONE | The linear side cannot run it                        | backreference, lookahead or lookbehind, `u` or `v` flag                                    |
| B    | The DFA and the Pike VM give a yes or no             | no feature of NONE                                                                         |
| C    | The Pike VM also gives exact capture spans           | B, and no nullable loop (a loop whose body can match the empty string)                     |

Run-time conditions that send one call to the backtracker, with no change to the flag: a two-byte input with `i`; a DFA that reaches the cap of 512 states (it gives up for that call and the backtracker runs; 16,040 gave-up runs in 332,504 in the differential run, mostly test262 stress patterns: not split).

Why each feature stays on the backtracker:

- Backreference: the match depends on the text of an earlier capture. An automaton cannot keep it in a bounded state.
- Lookaround: needs a nested match at the same position. A linear version exists (a sub-automaton for each assertion) but it is a design of its own. Not in this plan (question 3).
- `u` and `v`: code points and simple case folding with `bare-unicode` tables. 13 runs in the census. Not worth a second set of rules.

### 9.5 The dispatch rule

The rule is set at compile time, and compile is lazy: the first run of a pattern builds the program. The program header holds the flag and a run counter.

| Call                                  | Rule                                                                                                                                   |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `test()`, flag NONE                   | The backtracker, with the 50 M cap and fuel.                                                                                           |
| `test()`, flag B or C, run 1 to 63    | The backtracker. If it passes 100 K steps, the DFA takes this call at once.                                                            |
| `test()`, flag B or C, run 64 and up  | The DFA. If it gives up (state cap, two-byte `i`), the backtracker runs.                                                               |
| `exec()`, flag NONE or B              | The backtracker, with the 50 M cap and fuel.                                                                                           |
| `exec()`, flag C                      | The backtracker. If it passes 100 K steps, the Pike VM runs the same call from the start. The result is the same by the rules of 9.3.  |

Why these numbers:

- 64 runs is the break-even of the DFA (34 to 60 runs, section 9.2). Of the 3,270 compiles with flag B or C, 136 compiled patterns pass 64 runs, and the runs after run 64 are 97,905 (82.2% of all runs). Threshold 8: 302 patterns, 91.2%. Threshold 256: 64 patterns, 67.6%. V8 tiers up at the second run, but its native compile costs less than 64 runs of its interpreter; ours is the DFA states.
- 100 K steps for the fallback: on the census patterns with 20 inputs and 300 words (515,520 runs) the largest run is 56,724 steps; 18 runs pass 10 K and none passes 100 K. The fallback never fires on the census. It fires only for a pattern that is slow, which is the case that it is for.
- The backtracker stays first for `exec` because the Pike VM is 2.4 to 6 times slower (section 9.2). The cost of the fallback is at most 100 K steps of waste for each slow call.
- A run counter and a flag are the only state. No profile, no dynamic tiers.

### 9.6 Shared parser and tree

`crates/js2/src/regexp.rs`: parser, tree, emitter, backtracker (the port of section 8, option (a)). `crates/js2/src/regexp_linear.rs`: the Pike VM and the DFA. The linear file reads the program through a small view (the `Inst` kind and its operands), so a change of the program encoding touches one place.

The checker `regexp_check.rs` (#790) is a separate syntax check today. To verify in R1: whether it can call the new parser, so that there is one parser. Not looked at.

### 9.7 Budget and fuel on each side

Backtracking side (decision 3): each step is charged to `Vm::fuel` in blocks of 256 steps, and to a counter with the cap of 50 M steps for each run. A step costs about 50 instructions in the old engine (hypothesis: not re-measured for this file). A back edge of the interpreter decrements fuel by 1 and costs less than 50 instructions. The exchange rate is set in R1 by measurement. The rule: a regexp that runs as long as a loop uses the same fuel. The first pass asked this as a question; it is now a task of R1.

Linear side: the DFA charges 1 step for each 16 units, the Pike VM 1 step for each thread step, both to fuel in blocks. They have no per-run cap, because their time is O(program length × input length) and the input limit (16 Mi units) stays. A native callback has no back edge, so the charge is the only place where fuel can end the run. Fuel out is an error, as in the old engine, and never "no match".

CLAUDE.md says that a limit that protects against a slow algorithm is a design problem. The hybrid lowers the problem: a pattern with flag B or C cannot reach the 50 M cap in practice, because the fallback at 100 K steps hands it to a linear engine. The cap stays for flag NONE (lookaround, backreference). Removing it there needs a linear engine for those features (question 3).

### 9.8 Where the census lands

From the census log: 3,513 compiles, 1,611 unique (flags, pattern) pairs, 119,107 runs. "Unique" counts a pattern once. "Runs" are the engine runs after the memo.

| Class                                            | Unique | Compiles |    Runs | Share of runs |
| ------------------------------------------------ | -----: | -------: | ------: | ------------: |
| Flag B or C (the linear side can run it)         |  1,492 |    3,270 | 112,437 |         94.4% |
| of which flag C (exact captures on Pike)         |  1,490 |    3,265 | 112,180 |         94.2% |
| of which flag B only (nullable loop)             |      2 |        5 |     257 |          0.2% |
| Flag NONE (backtracker only)                     |    119 |      243 |   6,670 |          5.6% |
| of which lookahead, no backreference             |     90 |      184 |   5,780 |          4.9% |
| of which backreference                           |     17 |       46 |     877 |          0.7% |
| of which `u` or `v` only                         |     12 |       13 |      13 |          0.0% |

Inside flag C:

| Subset                              | Unique | Compiles |   Runs | Share of runs |
| ----------------------------------- | -----: | -------: | -----: | ------------: |
| lazy quantifier (the Porter rules)  |     73 |      145 | 55,633 |         46.7% |
| `i` flag                            |    262 |      499 |  2,500 |          2.1% |
| `m` flag                            |     38 |       45 |    417 |          0.4% |
| program longer than 256 instructions |    16 |       33 |    329 |          0.3% |
| program longer than 1,024           |      4 |        4 |      2 |          0.0% |

- The lazy Porter rules (`^(.+?)e$` 7,871 runs and its 10 neighbours) are all flag C. These are the hot patterns. After 64 runs they run on the DFA for `test()` and the whole 55,633 runs are within reach of the linear side.
- The 99 lookahead patterns (90 without a backreference, 9 with one) and the 17 backreference patterns (46 compiles) all run on the backtracker: 6,670 runs, 5.6%. The hottest of them has 426 runs. 83 of the 99 lookahead patterns are "simple" (no capture, no backreference, no nested lookaround): the first candidates if a later step puts lookahead on the linear side (4.1% of runs, question 3).
- Lookbehind and named groups have 0 uses in the census. The old engine supports both, and S4.R3 carries them.

Gain, with these numbers. `rust-book-search`: 64,227 engine runs, 96% `test()` (first pass, from `docs/progress.md`). If all are Porter `test()` runs at the DFA cost: (3,924 - 206) × 64,227 = 239 M of 10,190 M instructions (2.3%), against about 4.3 M of warm-up (19 rules × 225 K, hypothesis: 19 rules). Over the 48 journeys: about 0.5% of 43.4 G (hypothesis: the 239 M plus small shares of the other journeys; no journey was run with a DFA). The split of the 119,107 runs between `test()` and `exec()` was not logged (not measured).

### 9.9 Memory and heap layout

- The program, the class sets and the run counter are one `Blob` cell (decision 4): no `Vec`, no `Box`, no finalizer. A new `Layout::Blob = 21` (the layouts end at `Elements = 20`). The collector frees the Blob when no RegExp object points to it. Many objects with the same (source atom, flags) share one Blob through a cache keyed by those two (the census has 2.2 compiles for each unique pattern).
- The DFA table is a second `Blob` that the first one points to. It grows to a cap of 512 states. At the cap the DFA gives up for that call. Measured: 7.2 states for each staging pattern and 15.9 for each Porter rule; 2.0 to 4.4 KB of live bytes for each pattern that ran.
- The Pike VM thread lists and the backtracker stack are scratch on the realm (not in a cell). They are released after a run above a set size, as the old engine does (`release()`).
- The matcher reads `&[u8]` or `&[u16]`. Decision for R1: a generic loop over the unit type (the prototype cost +6.7 KB for the second unit type: +34.9 KB against +28.2 KB), not a copy of every one-byte input to wide units.

## 10. Build plan

Each step is one PR of about 400 changed lines or fewer, tests not counted. A check marked (R) records numbers in `docs/progress.md` with machine, inputs, run count and spread.

Checks that every step R1 to R3 runs (the "common checks"):

- `cargo test --locked -p bare-js2`.
- test262: the directories of the step (below). Bar (decision 2): no test that passes on the old engine fails on js2.
- The differential runner: the old engine and js2 run the same script or the same (pattern, flags, input) list. The lists are the census patterns (1,611 unique) and the 846 oracle cases (`regexp_cases.tsv`, 1,591 lines). Zero differences.
- The regex row (`cost_rows.txt`): instructions for each run on the 66-pattern set of section 6.
- Startup unchanged: `script_run` of `var x = 1;` stays at 4.01 M instructions. The engine is not built before the first literal or constructor runs.
- Binary size: the stripped size of the `script_run` example before and after the step, in the PR text.

| Step  | What                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Checks (besides the common checks)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S4.R0 | Baseline. Run test262 `built-ins/RegExp`, `annexB/built-ins/RegExp`, `built-ins/String/prototype/{match,matchAll,replace,replaceAll,search,split}` on the old engine with the js2 skip rules and write the pass list (a `test262-js2-baseline` change). Add the regexp directories to `test262-js2-subset.txt`. Add a regexp row to `cost_rows.txt` for js2: the 66-pattern set of section 6, plus a row for `(a+)+b` with n = 28 and the 19 Porter rules on 2,000 words. | The old counts of section 3.4 reproduce. The rows exist and have 3 runs each. The old engine's numbers are the base of R1 and R2: 1.65 K and 3.84 K for each run. (R)                                                                                                                                                                                                                                                                                                                                                                                     |
| S4.R1 | The backtracker behind the `Blob`. `crates/js2/src/regexp.rs`: parser, tree, emitter and matcher of the old engine; input `&[u8]` or `&[u16]` (a generic loop); result in a caller slice; no `AtomicU64`. Compile at the first run, not at the literal. Programs in a `Blob` (`Layout::Blob = 21`, marker change in `gc.rs`), shared through a cache keyed by (source atom, flags). Steps charged to `Vm::fuel` in blocks, plus the cap of 50 M steps for each run; the exchange rate is measured here. `Op::NewRegExp` builds the object with `lastIndex`; `RegExp` constructor, `test`, `exec`, `lastIndex`, `g`, `y`. `js2` depends on `bare-unicode`; `regexp_check.rs` checks property names with it. | test262 `built-ins/RegExp` outside `property-escapes` and `unicodeSets`. The `regexp_check` change closes the 242 parse-time `property-escapes` tests. Regex row: not more than +5% over 1.65 K and 3.84 K. The catastrophic row ends with a budget error, as in the old engine, and the fuel of the realm drops by the exchange rate. Compiled bytes for each pattern on the 66 patterns: report (target below 916 B). Peak RSS on `rust-book-search` and `substack-home` reported. Miri on the `Blob` code (the CI list of `unsafe` modules grows by one). (R) |
| S4.R2 | The linear side. `crates/js2/src/regexp_linear.rs`: Pike VM and lazy DFA over the program of R1, for `u8` and `u16` input. Flags NONE, B and C set by the emitter. The dispatch of section 9.5: the run counter (tier-up at run 64), the fallback at 100 K steps, the DFA table in a second `Blob`, the cap of 512 states, charge to fuel (section 9.7).                                                                                                                  | A differential test of the linear side against the backtracker on all census patterns, all patterns of the test262 directories of R1 that have flag B or C, and the oracle cases, for `test()` and `exec()`, with `from > 0` and with `y`. Zero differences. A seeded fault (no capture reset) must make the test fail. The Porter `test` row after warm-up: at least 10 times fewer instructions than R1 (measured prototype: 18 to 20 times). The staging `test` row: at least 2.5 times fewer (prototype: 3.2 to 3.7). The `exec` rows: not worse than R1 by more than 1%. `(a+)+b` with n = 28 and n = 4000 return "no match" in under 5 ms. Binary: not more than +40 KB over R1 (prototype: +34.9 KB with both unit types). Startup unchanged. (R)                  |
| S4.R3 | `String.prototype.match`, `replace`, `replaceAll`, `split`, `search`, `matchAll`; `RegExp.prototype[Symbol.*]` with a Rust fast path for an unmodified RegExp (one realm flag: shape and prototype checks) and the generic spec algorithms in the prelude (decision 7); the `exec` result shape; named `groups` and `d` indices; lookbehind. `RegExp.prototype.flags`, `source`, `toString`, `compile` (Annex B). `\p{}` and `u` and `v` case folding with `bare-unicode`. | test262: the six String directories and the rest of `built-ins/RegExp`, at the old engine's counts or better. A replace-heavy row (`replace` with `g` and a function, 10,000 matches) within 5% of the old engine. The differential runner is clean on the String methods. (R)                                                                                                                                                                                                                                                                                                                                                                               |
| S4.R4 | Only if a profile of the daily journeys on js2 asks for it (decision 6): the `test()` memo with a generation in the program id (section 7.9). It starts after S5.                                                                                                                                                                                                                                                                                                    | `rust-book-search` instructions with and without the memo, 3 runs. The DFA of R2 may remove the need: measure with R2 first.                                                                                                                                                                                                                                                                                                                                                                                                                             |

The plan does not touch startup: no regexp code runs before the first literal or constructor call. The old engine stays the default until S5 passes (plan section 1). Decision 8 means the R1 numbers are taken on the journeys that pass; the 4 failed journeys are not looked at.

## 11. Questions for the owner

The first-pass questions are answered (decisions 1 to 8). These are still open. Each has a recommended answer.

1. Which linear engine: (i) `regex-automata` or (ii) our own Pike VM and lazy DFA? Recommended: (ii). (i) gives the same `test()` speed (417 and 326 against 544 and 245 for the own DFA on u16), but it costs +107 KB of binary against +28 to +35 KB, 20 times more compile (262 K against 13.6 K), reads bytes only, and gives 96 wrong capture spans on the ES reset rule. We would still write the translation, the LF-only `m` workaround and the two-byte path.
2. A limit for the binary size of the linear side? Recommended: +40 KB over R1 for the stripped `script_run`, checked in R2. The prototype with both unit types is +34.9 KB. If R2 passes the limit, it drops the `u8` copy of the Pike VM first (the `u16` pair is +28.2 KB).
3. Should the linear side take lookahead later? 90 census patterns (4.9% of runs) have lookahead and no backreference. Recommended: no, not in this plan. Their cost is on the backtracker with the 50 M cap and fuel. Reopen only if a profile of js2 shows a lookahead pattern as a top cost. This keeps the cap that CLAUDE.md calls a design smell, but only for lookaround and backreferences.
4. Are the thresholds right: tier-up at run 64 and fallback at 100 K steps? Recommended: yes. 64 is the measured break-even of the DFA (34 to 60 runs). 100 K steps is above every census run (largest 56,724). R2 re-measures both on the js2 journeys and may change them.

## 12. Not measured

- The census is one live run on one day. A site that changes its scripts gives other patterns. The memo hides repeated `test()` runs (the run count is after the memo). The split of the 119,107 runs between `test()` and `exec()` was not logged.
- The old log counts compiles by `Regex::new`. A pattern compiled for a syntax check only (`Regex::check`) is not counted.
- Matcher cost on the real journey inputs (the rows use 20 chosen inputs and 2,000 words). The census patterns in section 9.2 are run on 20 inputs and 300 words for the step counts.
- No journey was run with the linear side. The 0.5% gain of section 9.8 is a product of numbers (hypothesis).
- The prototype is built over the old engine's `Inst` program, not over a js2 `Blob`. The DFA table in a `Blob`, the fuel charge, and the exchange rate of a step to a fuel unit are not built.
- Differential coverage: 8,465 patterns (the census, the staging, Porter and oracle patterns, and a crude scan of test262 literals) with 332,504 DFA runs and 324,636 Pike runs, 0 differences; one run ended with the budget error of the old engine and was skipped. The test262 scan was a text scan of regexp literals, not the test262 harness. Random inputs were used for the rest. Patterns built at run time with `new RegExp(...)` in the test262 files are not in it.
- Whether the Pike VM can run nullable loops exactly (section 9.3, item 3), and whether an unordered DFA state would give fewer states (item 1).
- Whether `regexp_check.rs` can share the new parser (section 9.6).
- `regex-automata`: the one-pass engine and the full DFA were not measured beyond a first try. Only 0.4.19 and the minimal features were tried.
- JSC YARR, SpiderMonkey and Boa are not re-read. `fancy-regex` is not measured.
- The cost of widening a one-byte input (not needed: R1 uses a generic loop); the cost of `Symbol.replace` through the prelude.
- Windows and macOS (the matcher is portable Rust; the old engine's tests run there, not rerun). The prototype ran on archMachine only.
- The old engine's pass count with a larger step limit on `property-escapes`.

## Decisions on the hybrid (owner, 2026-11-21)

1. Linear engine: our own Pike VM and lazy DFA over the shared program, not `regex-automata`.
2. Binary limit: +40 KB over R1. If R2 is over the limit, drop the u8 copy of the Pike VM first.
3. The linear side does not take lookahead. Reopen only if a js2 profile asks.
4. Tier-up to the DFA at run 64. Fallback from the backtracker after 100 K steps.
5. Build order: R0, R1, R2, R3. R4 only if a profile asks.

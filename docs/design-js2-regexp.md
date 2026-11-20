# Design: the RegExp engine of js2 (S4)

Status: design, for review by the owner. No engine code changed. Date: 2026-11-20. Base: main `b0a0df89`.

`crates/js2` has no RegExp engine. `Op::NewRegExp` throws (`interp.rs`: "regular expression literals are not supported yet"). Pages use regular expressions in `String.prototype.replace`, `split`, `match`, `RegExp.prototype.test` and `exec`, so S4 and the browser slice (S5) need one.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "old engine" is `crates/js` (`bare-js`). "js2" is `crates/js2`.
- "run" is one call of the matcher on one input (`Regex::exec` in the old engine). "compile" is pattern text to program.
- "memo" is the result table for `test()` of the old engine (`docs/progress.md`, R2-A build 2).
- All instruction counts are `perf stat -e instructions:u`, minimum of 3 runs. The spread of the 3 runs was below 0.01% in every row.

## 1. Summary

The old engine already has a complete, tested matcher in one standalone file (`crates/js/src/regexp.rs`, 2,550 lines). It does not use any JavaScript value. It works on `&[u16]`.

The census of the 48 public journeys (section 3) says what matters:

- 3,513 patterns are compiled on 32 journeys. Only 990 of them (28%) ever run.
- The matcher runs 119,107 times in all 48 journeys, after the memo. At 1.7 K to 3.8 K instructions a run this is about 0.2 to 0.45 G instructions (hypothesis: the product of the run count and the measured cost per run). The 48 journeys cost 43.4 G (`docs/progress.md`, R2-A build 2). Matcher speed is below 1% of the work.
- No pattern uses lookbehind or a named group. 10 use `\p{}`. 19 use the sticky flag `y` (one journey, 2,584 runs).

So the matching speed is not the problem to solve. The problems are: do not compile what never runs, keep a compiled pattern small, keep the semantics exact, and keep catastrophic backtracking bounded.

Recommendation (section 9): option (a), a port of the old matcher into js2, in three steps:

1. Port the matcher and compile it lazily, at the first run. The program stays a Rust value in a realm table.
2. Flatten the program into one buffer in a heap cell, so the collector frees it, and make the matcher read one-byte strings without a copy.
3. Put the `exec` fast paths and the `replace`, `split` and `match` protocol on top, with the generic spec path in the prelude.

Options (b), (c) and (d) lose on the numbers of section 6 or have no gain that the census can show (section 8).

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

The `replace`, `split` and `match` rows count string arguments too. The census cannot tell a regexp argument from a string argument (to verify in S4 step 3).

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

0 uses in the census. The old matcher supports both. Named groups need a `groups` object with null prototype; `d` needs the `indices` array. Both are built from the same span slice. They come after the first S4 pass (section 9, step 3).

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

Step 1 of section 9 uses choice 1 plus 3. Step 2 moves to choice 2 plus 3.

### 7.8 Fuel and the step budget

`Vm::fuel` counts down at loop back edges (`interp.rs`). The old engine charges its own `max_steps` (50 M) for each run. Plan: the matcher charges steps to `Vm::fuel` in blocks (a step of the matcher is about 10 to 30 instructions, a back edge more; the exchange rate must be measured: owner question 3). It stops with the same "fuel out" result as a loop. Two reasons to keep a per-run cap too: a regexp in a native callback has no back edge, and the per-run cap bounds the time of one call (120 ms for the 50 M budget in the table of section 6).

CLAUDE.md says that a limit that protects against a slow algorithm is a design problem. For a backtracking engine the limit is the standard answer (V8 has none and can hang; V8's linear engine is still experimental). A linear fallback (option d) is the way to lift it later.

### 7.9 The memo

The old engine's `test()` memo (`R2-A build 2`) took `rust-book-search` from 13,442 M to 10,190 M (-24.2%): 987,649 `test` calls became 64,227 engine runs. js2 needs a memo only if the port of that journey shows the same loop. The key `(program id, from, string identity)` is safe in js2 only with a generation in the id, because a freed cell address can come back (the old engine used a process counter for the pattern and a slot generation for the string). Not in the first S4 steps (section 9, step 4).

## 8. The options

Each option has data, algorithm, complexity, memory, code size, and an expected cost on our rows. "Expected" numbers come from section 6 and 3.2, and are hypotheses until a step measures them.

### (a) Port the old engine's matcher into js2

Data: as section 4 (`Vec<Inst>`, class sets, register file, `Frame` stack), later flattened into one `u32` buffer in a heap cell (step 2).

Algorithm: backtracking VM, first-unit set, anchored flag. Complexity: O(n) for simple patterns; exponential for nested quantifiers, cut off by the budget (section 7.8). Counted repeats unroll: O(count × body) code.

Catastrophic protection: the step budget, charged to fuel and capped for each run. Measured: `(a+)+b` stops at 117 to 121 ms for any n.

Memory: 916 bytes for each compiled pattern now; with lazy compile the largest journey keeps 166 patterns (152 KB, hypothesis: 166 × 916 B). After the flat image (step 2) the size is bounded by the encoding: QuickJS shows 120 bytes on the same patterns (hypothesis for ours; the old class tables are larger than QuickJS's ranges).

Code size: 2,550 lines to port as is. The port changes the input type, the result type, the budget and the program owner: about 400 changed lines (hypothesis: from the list of five weak points in section 4). Binary: about +368 KB with the Unicode tables (measured in the harness), of which the tables are shared with `\p{}` and `fold` that js2 needs under every option.

Cost on our rows: the row of section 6, 1.65 K and 3.84 K per run. The census total is about 0.2 to 0.45 G of 43.4 G. Lazy compile removes 72% of the 3,513 compiles: 3,513 × 25.8 K = 90.6 M becomes 990 × 25.8 K = 25.5 M (the compile of a pattern that runs is paid once; hypothesis: the compile count in the new engine equals the count of runs of the old one).

Conformance: the starting point is known (section 3.4: 914 of 1,114 outside `property-escapes` and `unicodeSets`; the rest are protocol tests that step 3 builds).

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

### (d) Hybrid: a linear-time automaton for patterns without backreferences and lookaround, backtracking for the rest

Data: the program of (a) as the NFA, plus a lazy DFA or a Pike VM over it. A DFA state is a sorted set of program counters; a transition table (`[u32; 256]` for Latin-1 units, a fallback for other units; at most 64 states for each pattern, then cleared, as RE2 does: `design-heavy-js-journeys.md`, section 5.3).

Algorithm: `test()` on a pattern with no backreference and no lookaround runs the DFA from the start index. One transition for each unit. Captures (`exec`, `replace`, `match`) still need a backtracker or a Pike VM.

Complexity: O(n) for each run; O(states × 256) to build. Linear time removes the need for a budget on those patterns. V8's experimental engine is O(pattern length × input length) and also excludes `u`, `i`, backreferences, lookaround and big repeats.

Memory: up to 64 KB for each pattern that uses the DFA (1 KB for each state, hypothesis from the earlier design; not measured).

Code size: 400 to 600 lines (the earlier design's estimate), plus the care for case folding and `u` mode.

Measured evidence: the `regex` crate row of section 6 shows 4 to 9 times fewer instructions for a `test`, and 14 times more for compile. That crate is not a design for us (UTF-8, Unicode `\w`, refuses 5 of 66 patterns), but it shows the shape: cheaper runs, dearer compile.

Cost on our rows: by the census, 119,107 runs; 96% of the old engine's runs on `rust-book-search` were `test()` (progress, R2-A). If a DFA saves 3.3 K for each Porter `test` (3,842 → about 430), the saving on `rust-book-search` is 64,227 × 3.3 K = 212 M of 10,190 M (2.1%, hypothesis) and nothing on the other 47 journeys. The compile is dearer (a DFA state is built at the first use). The hole of the hybrid is the same as V8's: patterns with `i`, `u`, backreferences or lookaround (99 lookahead patterns, 292 `i` patterns, 17 backreference patterns in the census) stay on the backtracker, so the budget stays.

### Comparison

|                        | (a) port                       | (b) libregexp-style                                  | (c) `regress`                    | (d) hybrid                              |
| ---------------------- | ------------------------------ | ---------------------------------------------------- | -------------------------------- | --------------------------------------- |
| Run, staging `test`    | 1.65 K                         | about 5.4 K measured in C (first-unit set not there) | 3.67 K                           | 0.4 K for a DFA run (crate lower bound) |
| Run, Porter `test`     | 3.84 K                         | 3.4 K                                                | 3.5 K                            | 0.43 K (crate lower bound)              |
| Compile                | 25.8 K                         | 13.1 K                                               | 59.2 K                           | more than (a)                           |
| Bytes for each pattern | 916 B, 120 B flat (hypothesis) | 120 B                                                | 1,474 B                          | 916 B plus DFA                          |
| Budget on catastrophic | yes, 120 ms                    | to build                                             | none, 13.7 s                     | yes, except linear patterns             |
| Sticky `y`             | yes                            | yes                                                  | no                               | yes                                     |
| New code               | about 400 changed lines        | about 2,500 lines                                    | 0 (a fork if a budget is wanted) | (a) + 500 lines                         |
| Conformance start      | 914 / 1,114 test262            | zero                                                 | not measured                     | as (a)                                  |
| Census gain over (a)   | none (the base)                | 0.03% of 43.4 G                                      | negative                         | up to 2.1% of one journey               |
| Binary                 | +368 KB (with tables)          | about the same                                       | +818 KB                          | (a) + 1 to 2 KB                         |

## 9. Recommendation

Build (a), in steps, and measure (d) after the browser runs journeys. Reasons, each from a number above:

1. The matcher is below 1% of journey instructions, so a faster matcher is not the goal (section 1).
2. The old matcher is the fastest of the three backtrackers on our short tests, bounds the catastrophic case at 120 ms, supports sticky, and has a known test262 and oracle record.
3. The wasted work is the compile: 72% of 3,513 compiles are never used. Lazy compile removes it in any option.
4. (b) saves 0.03% of 43.4 G and starts conformance from zero. (c) is worse on every row but one and has no budget and no sticky. (d) adds code for 2% of one journey.

Build plan. Each step is one PR of about 400 changed lines or fewer, tests not counted. Each step runs `cargo test --locked -p bare-js2` and the checks below. A check marked (R) records numbers in `docs/progress.md` with machine, inputs, run count and spread.

| Step  | What                                                                                                                                                                                                                                                                                                                                                                                                                                          | Checks                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S4.R0 | Baseline. Run test262 `built-ins/RegExp`, `annexB/built-ins/RegExp`, `built-ins/String/prototype/{match,matchAll,replace,replaceAll,search,split}` on the old engine with the js2 skip rules and write the pass list (a `test262-js2-baseline` change). Add the regexp directories to `test262-js2-subset.txt`. Add a regexp row to `cost_rows.txt` for js2 (a run of the 66-pattern set of section 6).                                       | The old counts of section 3.4 reproduce. The row exists. (R)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| S4.R1 | Move the matcher to `crates/js2/src/regexp.rs`: input `&[u8]` or `&[u16]`, result in a caller slice, steps charged to `Vm::fuel`, no `AtomicU64`. Compile at the first run, not at the literal. `Op::NewRegExp` builds the object with `lastIndex` and the pattern atoms; `RegExp` constructor, `test`, `exec`, `lastIndex`, `g`, `y`. Programs in a realm table with a cache keyed by (source atom, flags). `js2` depends on `bare-unicode`. | test262 `built-ins/RegExp` outside `property-escapes` and `unicodeSets`: no test that passes on the old engine fails on js2. The oracle cases `regexp_cases.tsv` (846 rows) run on the js2 matcher: 0 differences. The differential runner (old against new on the same script) is clean on the regex cases. The regex row: instructions for each run not more than +5% over the old engine's 1.65 K and 3.84 K. Startup unchanged: `script_run` of `var x = 1;` stays at 4.01 M (the engine is not built before the first literal runs). (R) |
| S4.R2 | Flat program: encode `Inst` and the class sets into one `u32` buffer in a heap cell (new layout, or a one-byte string cell with a hidden slot; owner question 4). Drop the realm table. A generic loop over `u8` and `u16` input. A "release" of the scratch after a run above a size.                                                                                                                                                        | The R1 checks again. Compiled bytes for each pattern on the 66 patterns: report (target: below 916 B). Peak RSS on `rust-book-search` and `substack-home` not above the R1 numbers. Miri on the cell code (the CI list of `unsafe` modules grows by one). (R)                                                                                                                                                                                                                                                                                 |
| S4.R3 | `String.prototype.match`, `replace`, `replaceAll`, `split`, `search`, `matchAll`; `RegExp.prototype[Symbol.*]` with the fast path (shape and prototype check) and the spec path in the prelude; the `exec` result shape; `groups` and `d`. `RegExp.prototype.flags`, `source`, `toString`, `compile` (Annex B). Property escapes with `bare-unicode`, `\p{}` names checked in `regexp_check.rs`.                                              | test262: the six String directories and the rest of `built-ins/RegExp` at the old engine's counts or better. `regexp_check` closes the 242 `property-escapes` parse tests. The differential runner is clean. A replace-heavy row (`replace` with `g` and a function, 10,000 matches) is within 5% of the old engine. (R)                                                                                                                                                                                                                      |
| S4.R4 | Only if a journey on js2 shows it: the `test()` memo with a generation in the program id (section 7.9).                                                                                                                                                                                                                                                                                                                                       | `rust-book-search` instructions with and without the memo, 3 runs.                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| S4.R5 | Only if a profile of the daily journeys on js2 shows regexp time above 5%: design the lazy DFA for `test()` (option d) in its own document.                                                                                                                                                                                                                                                                                                   | The profile.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

The plan does not touch startup: no regexp code runs before the first literal or constructor call. The old engine stays the default until S5 passes (plan section 1).

## 10. Questions for the owner

1. Which option? Recommended: (a), the port, in the steps of section 9.
2. Is a conformance bar of "no test that passes on the old engine fails on js2" right for S4.R1 to R3? Recommended: yes. Tests that the old engine fails (the `RegExp.escape` family, `createRealm`) wait.
3. Keep a step budget? Recommended: yes, and charge it to `Vm::fuel` plus a per-run cap of 50 M steps. The exchange rate of a matcher step to a fuel unit is set in S4.R1 by measurement.
4. A new `Blob` cell layout (marker change in `gc.rs`) or a one-byte string cell that no script can reach, for the compiled program? Recommended: the new layout, because a hidden string cell can leak into `typeof`, `==` and atom-table paths by mistake.
5. Should js2 depend on `bare-unicode` for `\p{}` and case folding, and should `regexp_check.rs` use it to check property names? Recommended: yes (one set of tables for both engines; it closes 242 early-error tests).
6. Is the memo (S4.R4) in S4 or only after S5? Recommended: after S5, if the browser slice shows the loop.
7. Where do the generic `Symbol.replace`/`split` algorithms live? Recommended: the prelude (JS, lazy stubs), with Rust fast paths only for the unmodified RegExp.
8. Do we build the 4 journeys that failed in my live run before the S4.R1 numbers are taken? Recommended: no. They failed on the old engine for reasons I did not look at (`curl-se-home`, `duckduckgo-html-search`, `lobsters-comments-back`, `nextjs-docs`). The numbers of section 3.2 include their partial runs.

## 11. Not measured

- The census is one live run on one day. A site that changes its scripts gives other patterns. The memo hides repeated `test()` runs (the run count is after the memo).
- The old log counts compiles by `Regex::new`. A pattern compiled for a syntax check only (`Regex::check`) is not counted.
- Matcher cost on the real journey inputs (the rows use 20 chosen inputs and 2,000 words).
- JSC YARR, SpiderMonkey and Boa are not re-read. `fancy-regex` is not measured.
- Memory of a DFA for option (d); the cost of widening a one-byte input; the size of a second `u8` copy of the loop; the cost of `Symbol.replace` through the prelude.
- Windows and macOS (the matcher is portable Rust; the old engine's tests run there, not rerun).
- The 4 failed journeys and the old engine's pass count with a larger step limit on `property-escapes`.

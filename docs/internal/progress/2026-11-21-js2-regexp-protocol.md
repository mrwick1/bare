# 2026-11-21 — RegExp symbol methods and named captures (S4.R3)

This change started as a PR stacked on #854, [String methods with a RegExp](2026-11-21-js2-string-regexp.md).

## Changes

- String methods read the applicable symbol on object patterns before they coerce their receiver. The RegExp symbol methods run in Rust and use the runtime property operations in spec order. RegExpExec calls a custom `exec` and checks its result. Split and matchAll construct the matcher through `Symbol.species`.
- The fast path checks the instance and prototype shapes and the original function values in their slots. A write can replace a function or getter without changing the shape. The slot check catches these writes. The match loop uses spans directly when these checks pass.
- The compiled image stores capture names. Match results expose those names through `groups` and `indices.groups`. A name shared by disjoint alternatives uses the capture that matched. Replace supports `$<name>` and passes the groups object to a functional replacer.
- MatchAll makes a RegExp String Iterator. Each call to `next` runs one RegExpExec. The iterator releases the matcher and input when it finishes.
- The Annex B `escape` and `unescape` globals encode and decode UTF-16 units. Unescape preserves malformed escapes.
- The String split entry point and the well-known Symbol table have the hooks needed to reach the new methods.

## Instruction counts

The base is PR #854 at `406ba818` with main at `da9ba9af` merged into it. Both builds include that main commit. Linux on archMachine. Each cell gives the minimum of three `perf stat -e instructions:u` runs. The maximum spread was 1.32%. The glibc runs used `glibc.malloc.tcache_count=65535`. The jemalloc runs used `LD_PRELOAD=/usr/lib/libjemalloc.so.2`.

| Row | glibc base | glibc branch | Change | jemalloc base | jemalloc branch | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Startup | 728,975 | 730,386 | +0.194% | 4,432,199 | 4,433,055 | +0.019% |
| loopfn | 875,307,961 | 875,308,764 | +0.000% | 879,071,043 | 879,072,318 | +0.000% |
| calls | 589,365,060 | 589,366,417 | +0.000% | 593,128,374 | 593,128,796 | +0.000% |
| fib | 260,439,635 | 260,440,312 | +0.000% | 264,201,849 | 264,201,659 | -0.000% |
| closures | 1,965,214,094 | 1,965,215,763 | +0.000% | 1,968,886,697 | 1,968,891,627 | +0.000% |
| objects | 712,141,698 | 712,143,338 | +0.000% | 715,902,534 | 715,903,400 | +0.000% |
| RegExp test and exec | 430,918,389 | 429,843,437 | -0.249% | 434,859,327 | 433,796,547 | -0.244% |
| String patterns | 250,535,103 | 226,448,001 | -9.614% | 257,590,971 | 231,442,248 | -10.151% |
| String methods with RegExp | 826,301,865 | 803,220,540 | -2.793% | 848,553,987 | 823,829,519 | -2.914% |

Every row meets the +0.5% gate with the default release build. String patterns now visit match positions without building a list of spans.

- Startup ran `script_run` with `var x = 1;`. The `interp_load` arguments were `loopfn 3000000`, `calls 1000000`, `fib 27`, `closures 1000000`, and `objects 1000000`.
- The RegExp and String rows follow the loops in the #854 progress entry. The RegExp loop used `caresses, running, hoped, walked, cats, electric, filing, agreed`. The String loop used a function that returns `'-'`.
- The String methods with RegExp row ran 20,000 turns of `match`, `search`, text and functional `replace`, and `split` on `'ab 12-cd 34-ef 56'`. It reused `/[a-z]+/g`, `/\d/`, `/(\d+)/g`, and `/[\s\-]+/`.

## Verification

- The four initial tests failed on #854. The duplicate-name test also failed before the parser change. The tests run with normal and forced collection.
- `cargo test --locked -p bare-js2 --lib -- --test-threads=4` passed all 754 tests.
- `cargo test --locked -p bare-js-api --features suite` passed 32 unit tests, 152 engine tests, 1 lifetime test, and 5 doc tests.
- `cargo check --locked -p bare-js2` passed.
- Built `bare-drive` and the `type_load` example in release mode. Ran `tools/journeys/triage.sh -e new -j 3` for the three RegExp journeys. All passed on archMachine after the main merge. The journeys were `rust-book-search`, `python-org-docs-link`, and `python-docs-search-results`.

### test262

Ran `test262_js2.sh -u` with the test262 checkout at `e4fbf577`. A temporary copy of the runner fixtures added all Annex B directories and RegExpStringIteratorPrototype. It enabled the five RegExp symbols, named groups, duplicate named groups, match indices, String matchAll, and String replaceAll. The final comparison reran the same 149 directories on js2 in both merged trees with a 480-second directory timeout. This avoided the default timeout in the generated Unicode property tests.

| Directory | Pass before | Pass after | Fail before | Fail after | Skip |
| --- | ---: | ---: | ---: | ---: | ---: |
| Annex B | 196 | 229 | 801 | 768 | 89 |
| RegExp | 1,142 | 1,406 | 366 | 102 | 371 |
| RegExpStringIteratorPrototype | 0 | 17 | 17 | 0 | 0 |
| String | 1,008 | 1,048 | 68 | 28 | 147 |
| Total | 2,346 | 2,700 | 1,252 | 898 | 607 |

The comparison found zero lost passes by test path. The Annex B gains include 15 escape tests and 18 unescape tests.

## Not tested

Windows and macOS locally. Cross-realm cases excluded by the runner. Binary size and peak memory.

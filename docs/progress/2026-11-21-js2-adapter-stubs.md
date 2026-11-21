# 2026-11-21: Complete js2 browser adapter operations

- Read a symbol description through a shared realm reference. Preserve the difference between a missing description and an empty string.
- Create the iterator prototype when the browser creates the realm. Use the same object for built-in and host iterators. Check constructibility with the engine's constructor test.
- Use the realm wall clock for `Date()`, `new Date()` and `Date.now()`. A virtual session without replay uses its scheduler time plus an epoch offset. A replay session uses the process clock. The old engine keeps its existing behavior.
- Print compact JSON for objects. Show cycles, thrown getters, functions, dates, nodes, and the entries of maps and sets. Honor the output cap. Ignore `toJSON`.
- Count dead object, string and symbol cells during an explicit collection. This uses the mark bits and the free lists after marking. In-task collections skip the counting pass. External growth can trigger a collection between tasks. The threshold is the greater of 4 MiB or one quarter of the native bytes that survived the previous browser collection.
- Borrow the current host call's arguments in place. Use the installed host for named and indexed property sources.
- Both engines give `[object HTMLElement]` for a div until the first read of `HTMLDivElement`. Both give `[object HTMLDivElement]` after that read.
- js2 has no ArrayBuffer, TypedArray, or DataView cells. There are no backing bytes for `bytes_of` to borrow.

## Checks

- `cargo test --locked -p bare --lib browser_new` passed 15 tests on archMachine. The baseline run on the head of #854 reproduced six adapter failures.
- The browser tests include a loopback session with virtual time and no replay. A 25 ms timer advances `Date.now()` and `performance.now()` by 25 ms.
- `cargo test --locked -p bare-js2 --lib api` passed 38 tests. The tests cover a borrowed argument range and exact collection counts for rooted and dead objects, strings, and symbols.

## Cost

The run used archMachine. Count user-mode instructions with `tools/js/instr.sh`. Take the minimum of three runs for each row and allocator. The base is `74f6611e` with the same three RegExp predicate edits as this branch. The extra `raw_collect` row runs `owner_load` with `raw_collect(&[])` in place of its regular collection call.

| Row | glibc base → new | Change | jemalloc base → new | Change |
| --- | ---: | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 887,300,960 → 887,306,686 | +0.001% | 890,700,092 → 890,706,779 | +0.001% |
| `interp_load calls 1000000` | 599,358,287 → 599,364,360 | +0.001% | 602,727,069 → 602,733,973 | +0.001% |
| `interp_load fib 27` | 262,974,280 → 262,980,798 | +0.002% | 266,373,387 → 266,380,198 | +0.003% |
| `interp_load closures 1000000` | 1,971,206,831 → 1,971,213,921 | +0.000% | 1,974,571,624 → 1,974,528,073 | -0.002% |
| `interp_load objects 1000000` | 713,133,777 → 713,140,793 | +0.001% | 716,531,471 → 716,538,594 | +0.001% |
| `gc_load chain 100000 0` | 34,011,516 → 34,017,711 | +0.018% | 37,312,618 → 37,339,208 | +0.071% |
| `gc_load chain 100000 10` | 289,800,077 → 289,806,305 | +0.002% | 295,007,949 → 294,895,172 | -0.038% |
| `gc_load tree 100000 0` | 25,711,388 → 25,718,102 | +0.026% | 29,032,519 → 29,038,755 | +0.021% |
| `gc_load tree 100000 10` | 222,667,118 → 222,673,144 | +0.003% | 226,007,700 → 226,013,579 | +0.003% |
| `owner_load 10000 1000 10` | 60,341,658 → 60,367,719 | +0.043% | 63,776,843 → 63,802,927 | +0.041% |
| `raw_collect 10000 1000 10` | 60,349,123 → 61,170,705 | +1.361% | 63,757,146 → 64,610,244 | +1.338% |

The explicit counted collection exceeds the limit under both allocators. The hypothesis is that the pass over unmarked cells and free slots accounts for the increase.

With `RUSTFLAGS="-C codegen-units=1"`, the counted row goes from 60,409,620 to 61,153,303 instructions under glibc (+1.231%) and from 63,814,640 to 64,529,991 under jemalloc (+1.121%).

## Journeys

`tools/journeys/triage.sh -e new -j 4` ran once on archMachine against the live sites. The result was 46 pass, 3 fail, and 1 skip. This matches [the run on #854](2026-11-21-js2-string-regexp.md). The same three journeys failed.

- `public:curl-se-home` found a hidden docs link.
- `public:duckduckgo-html-search` found too few results.
- `public:lobsters-comments-back` found four `a.u-url` links where the test expects one.
- The staging journey skipped because the login configuration was absent.

## Not tested

The full test suite, Windows, Miri, test262, and a session with replay.

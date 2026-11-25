# Read punctuation lookahead on demand

The DEV replay puts 246 million exclusive instructions in the largest `Lexer::scan_at` call edge. The punctuation scanner read three bytes ahead before it knew which token it was reading. It now reads the bytes its token branch needs. Single-byte punctuation such as a brace or comma needs no lookahead.

## DEV replay

Use the DEV tape from the November 23 public recordings. Build release at `2e8c5c74` with and without this change. Run on archMachine, AMD Ryzen 9 5900HX. Use the virtual clock and `BARE_SEED=1`. Count user-mode instructions with perf. Alternate the two binaries and keep the minimum of three runs for each allocator. The glibc runs use `glibc.malloc.tcache_count=65535`. Each replay passed and returned the same set of 16 replies, including multiplicity, status, and byte count.

| Allocator | Before | After | Change |
| --- | ---: | ---: | ---: |
| glibc | 4,482,249,484 | 4,459,370,558 | -0.510% |
| jemalloc | 4,577,218,021 | 4,554,341,664 | -0.500% |

Maximum spread across three runs was 0.036% in any group.

## Narrow checks

The lexer and parser rows use the minimum of three runs through `instr.sh` under each allocator.

| Row | glibc before | glibc after | jemalloc before | jemalloc after |
| --- | ---: | ---: | ---: | ---: |
| js2_lex_load small 1 1 | 114,768,978 | 114,769,200 | 118,076,375 | 118,076,532 |
| js2_parse_load full stmts 1 | 9,490,945 | 9,491,249 | 13,084,152 | 13,081,526 |

All Vex targets type-check. The targeted tests pass.

Coverage excludes the full Test262 subset, other browser journeys, wall time, and Windows.

# 2026-11-21 Restore the atom table load limit

The linear probe scans contiguous groups of occupied slots. At three-quarter load the probe replay for `bundle_like` had a 38-slot miss. The source atom table had 86 distinct full hashes for 87 strings at exit. Twenty-five of its 128 home buckets had more than one string, with at most four strings per bucket. At half load, 16 of 256 home buckets had more than one string, with at most three per bucket. There were 385 tag matches on 5,023 lookups in each replay. This includes 384 hits. The extra work was slot visits.

A trace at the full-matrix head `852c7036`, whose js2 code includes #842, showed the same pattern. At three-quarter load, linear probing gave a maximum lookup of 38 slots. Double hashing reduced it to 16, with 460 tag matches in both cases. The combined slot arrays fell from 6,144 bytes at half load to 4,096 bytes with double hashing at three-quarter load. This trace counts all callers of `find`.

## Options

- Keep linear probing and mix the hash again. This keeps the 8-byte slot and an expected constant-time lookup. A 32-bit xor, multiply and xor reduced the maximum probe from 38 to 26 at three-quarter load on `bundle_like`. The mean went from 2.41 to 2.64. A new mix also runs before the first probe.
- Use a quadratic probe like the [V8 string table](https://raw.githubusercontent.com/v8/v8/main/src/objects/string-table.cc). This keeps the 8-byte slot and uses an incrementing stride. It reduced the mean to 2.15 and the maximum to 11. Keys with the same home slot follow the same path. V8's [Swiss property dictionary](https://raw.githubusercontent.com/v8/v8/main/src/objects/swiss-name-dictionary.h) uses grouped control bytes and a separate slot array.
- Use double hashing, as in the hash table under the [SpiderMonkey atom set](https://raw.githubusercontent.com/mozilla/gecko-dev/master/js/src/vm/AtomsTable.h). Its [probe](https://raw.githubusercontent.com/mozilla/gecko-dev/master/mfbt/HashTable.h) uses an odd stride and checks a stored hash before matching. An odd stride from the existing 16-bit tag reduced the mean to 1.61 and the maximum to 12. The home index and 8-byte slot stay the same, and the second hash uses bits already read for the tag.
- Keep more hash bits to avoid string compares. [QuickJS](https://raw.githubusercontent.com/bellard/quickjs/master/quickjs.c) follows a chain of atom indices and checks the cached hash, type and length before the string. In js2 a wider tag would add space per slot or a cell read. The current tag already rejects all but one failed string comparison on this trace.

## Change

Use an odd stride from the high hash bits for lookup, insert and rebuild. The stride visits every slot because the capacity is a power of two. Growth at three-quarter load leaves an empty slot. Sweep reinserts the retained atoms with the same probe rule. Expected lookup and insert cost is constant with well-distributed hashes. A lookup can still visit the full table in the worst case. Pass the hash already in hand to `put` so a new insert does not read the cell.

## Check

Base `1303414b`, archMachine, Ryzen 9 5900HX, Linux, rustc 1.96.0. Release builds with symbols kept. Each row uses the minimum of three runs of `perf stat -e instructions:u` per build and allocator. Spread is the largest max minus min for either build or allocator on the row. Use the tcache setting from `tools/js/instr.sh` and preload `/usr/lib/libjemalloc.so.2` for jemalloc. Inputs are the checked-in `crates/js2/examples/data` fixtures run with `script_run --eval` and Node's expected result, plus `script_run` on `var x = 1;`.

| Row | Base, glibc | New, glibc | Change | Base, jemalloc | New, jemalloc | Change | Max spread |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 726,573 | 727,015 | +0.061% | 4,067,391 | 4,067,420 | +0.001% | 1,061 |
| `bundle_like` | 127,365,335 | 127,322,888 | -0.033% | 131,004,716 | 130,921,184 | -0.064% | 23,944 |
| `eval_calls` | 71,153,611 | 71,154,922 | +0.002% | 74,528,594 | 74,498,717 | -0.040% | 31,269 |
| `eval_closures` | 12,872,729 | 12,872,467 | -0.002% | 16,254,703 | 16,254,396 | -0.002% | 1,461 |
| `eval_concat` | 11,718,308 | 11,717,876 | -0.004% | 15,066,670 | 15,066,414 | -0.002% | 1,135 |
| `eval_fib` | 4,011,473 | 4,011,590 | +0.003% | 7,381,800 | 7,382,384 | +0.008% | 1,180 |
| `eval_loop` | 87,002,670 | 87,003,671 | +0.001% | 90,350,854 | 90,351,088 | +0.000% | 519 |
| `eval_loopfn` | 47,836,223 | 47,836,126 | -0.000% | 51,205,384 | 51,174,924 | -0.059% | 29,266 |
| `eval_objects` | 67,034,205 | 67,147,247 | +0.169% | 70,583,952 | 70,637,228 | +0.075% | 45,628 |
| `eval_throws` | 29,477,692 | 29,477,425 | -0.001% | 32,866,464 | 32,836,714 | -0.091% | 1,496 |
| `eval_webpack` | 6,650,653 | 6,653,391 | +0.041% | 10,191,136 | 10,190,730 | -0.004% | 1,658 |

The five `interp_load` rows also passed the gate. The largest absolute change was 0.006% on either allocator after subtraction of the zero-turn run.

## Probe replay and slot bytes

A diagnostic build logged each lookup, insertion and sweep. The replay reproduced every baseline probe count. It then ran the same sequence with the other policies. The counts include the empty slot at the end of a miss. The bytes are the combined backing arrays of the shared and runtime table at exit. They exclude the table headers, allocator overhead and string cells.

| Row | Base bytes | New bytes | Base mean / p95 / max | Linear 3/4 | Double hashing 3/4 |
| --- | ---: | ---: | --- | --- | --- |
| `startup` | 2,304 | 1,152 | 1.77 / 5 / 10 | 3.58 / 12 / 38 | 2.22 / 5 / 11 |
| `bundle_like` | 4,096 | 3,072 | 2.26 / 5 / 10 | 2.41 / 5 / 38 | 1.61 / 3 / 12 |
| `eval_calls` | 2,304 | 1,152 | 1.87 / 6 / 10 | 4.51 / 17 / 38 | 2.71 / 7 / 11 |
| `eval_closures` | 2,304 | 1,280 | 1.91 / 5 / 10 | 3.50 / 12 / 38 | 2.11 / 5 / 11 |
| `eval_concat` | 2,304 | 1,280 | 1.85 / 5 / 9 | 3.45 / 13 / 38 | 2.06 / 5 / 11 |
| `eval_fib` | 2,304 | 1,152 | 1.74 / 5 / 10 | 3.73 / 17 / 38 | 2.35 / 6 / 11 |
| `eval_loop` | 2,304 | 1,152 | 1.81 / 5 / 10 | 3.93 / 13 / 38 | 2.59 / 7 / 11 |
| `eval_loopfn` | 2,304 | 1,152 | 1.75 / 5 / 10 | 3.74 / 13 / 38 | 2.27 / 6 / 11 |
| `eval_objects` | 33,280 | 16,896 | 1.23 / 2 / 15 | 1.65 / 4 / 55 | 1.54 / 4 / 22 |
| `eval_throws` | 2,304 | 1,280 | 1.86 / 5 / 9 | 3.49 / 12 / 38 | 2.20 / 7 / 11 |
| `eval_webpack` | 5,120 | 2,560 | 1.70 / 5 / 9 | 2.63 / 8 / 38 | 1.77 / 4 / 19 |

`cargo test --locked -p bare-js2 --lib` passed 742 tests.

## Not tested

Windows, Miri, test262, wall time, peak RSS and adversarial strings. The browser on this base cannot start a js2 session.

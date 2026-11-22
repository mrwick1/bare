# 2026-11-22 — Look up existing index atoms

`Array.prototype.push` checks for inherited setters on empty indices. The descriptor lookup interns the decimal index even when no prototype has the property. In `objects_10`, Massif attributes 3,145,728 bytes at the malloc peak to growth of the runtime atom table on this path. The atoms also occupy js2 heap cells.

Format the index into ten stack bytes and look it up in the atom tables. Use the existing descriptor lookup for a hit. A miss uses no heap cell and creates no table entry. The expected saving is up to 6.1 MiB of index string cells plus the table growth. The change removes formatting allocations, table insertion and subsequent sweeping. The same probes still find existing properties.

## Measurement

Linux on archMachine. Compare release builds at `f29a583d` and this change with the decision benchmark source and runner. RSS uses the same static `wait4` wrapper and fresh non-reflink binary copies. Each cell gives the median and range of three runs.

| Row | Before, MiB | After, MiB |
|---|---:|---:|
| `objects_10` | 21.79 [21.79–21.80] | 21.55 [21.49–21.62] |
| `throws_10` | 12.69 [12.48–12.93] | 12.66 [12.41–12.83] |
| Empty process | 5.21 [5.17–5.45] | 5.16 [5.11–5.30] |

The `objects_10` instructions fell 5.72%, from 1,018.57 M [1,018.57–1,020.97] to 960.34 M [960.34–960.45]. Both versions returned `198902497,1199980`. The five `interp_load` rows each changed by less than 0.001% with default glibc and with `glibc.malloc.tcache_count=65535` (minimum of three `perf stat -e instructions:u` runs). The heap still collects at its original trigger.

The new test of missing index atoms failed on the baseline and passed after the change. The inherited setter test passed on both. The narrow atom and script tests passed (8 and 124 tests). The js2 typecheck passed. The only change in the built-ins lane is the lookup in `bi_object.rs`.

Not tested for this change alone: the browser ramp, test262, Miri, Windows and the full suite.

# 2026-11-22 — Reduce js2 peak memory

## Cause and designs

The collector runs at allocation safepoints in the interpreter. Its default in-task trigger starts at 16 MiB. In `objects_10`, it first ran with 16.44 MiB mapped and 0.60 MiB live. It ran once in the script. `throws_10` mapped 7.69 MiB without reaching the trigger. A forced collection after the script left 8,400 live bytes. The Error constructor has no captured stack. Massif measured 44,784 bytes of live malloc blocks including overhead in `throws_10`.

| Design | Work and complexity | Expected peak saving and CPU cost |
|---|---|---|
| Look up existing index atoms on reads | A stack decimal buffer and the existing atom tables. Average O(1) lookup. A miss leaves no cell and does not grow the table. | Up to 6.1 MiB of decimal index cells and 3 MiB of table growth in `objects_10`. No change in `throws_10`. Fewer allocations and atom sweeps. The number of table probes stays the same. |
| Collect at a lower in-task trigger | Keep the same 64 KiB blocks, allocation gates and mark/sweep. Use a 2 MiB floor and 2 MiB of task slack. A collection still costs time proportional to the live graph, weak tables and blocks. | With the old atom lookup, diagnostic runs reduced peak mapped bytes by 10.56 MiB for objects and 3.50 MiB for throws. More collections can re-scan live data. Lower mapping volume can save allocation work. |
| Format small integer concatenations directly into the result | Make one short string instead of making a string for the number and copying it into a second allocation. Cost is O(digits + string length). Keep ropes for longer results. | At most 6.1 MiB of cells in objects and 2.0 MiB in throws over the entire script. Peak savings depend on which collections run. Fewer cell allocations and copies. Array backing stores and error objects still need collecting. |

QuickJS 2026-06-04 releases values when the last reference drops. Its cycle collector runs at a separate size threshold. Retrofitting reference counts would require changes to every value store and frame lifetime. js2 can remove the index atom allocations first. The remaining data shows that a smaller trigger can reclaim garbage using existing roots.

The 4 MiB trigger experiment raised `interp_load closures 1000000` by 5.37% (1,942.71 M to 2,047.06 M instructions, minimum of three). The collector ran six times instead of three. The same experiment raised `interp_load objects 1000000` by 0.14%. A smaller trigger helps garbage-heavy scripts and adds marking work to scripts that keep their allocations live.

## Changes

[#908](https://github.com/mrwick1/bare/pull/908) skips atom creation on an index lookup miss. Massif measured 3,226,288 bytes of live malloc blocks at the peak of `objects_10` before that change and 139,552 bytes afterward. The js2 mapped heap peaks at a different time. Do not add the two peaks.

This change formats a tagged integer into an 11-byte stack buffer. When the result is shorter than 32 units and fits in Latin-1, it makes only the result cell. The prior path made two cells and allocated a Rust string for the digits. Longer and wide strings use the existing concatenation path. The collector keeps its current trigger.

## Peak RSS

Linux on archMachine (Ryzen 9 5900HX, Rust 1.96.0, glibc 2.44). The baseline is `f29a583d`. Use the unchanged decision benchmark launcher and raw scripts. Run each case in a fresh process through the static `wait4` wrapper. Copy the binary with `cp --reflink=never` for every RSS run. The outputs were `0`, `198902497,1199980` and `762962`, as expected. The first baseline run reproduced the report within 2% on the two target rows. Use a separate release target directory for each tree.

| Row | Baseline, MiB | Index lookups only, MiB | Both changes, MiB |
|---|---:|---:|---:|
| `objects_10` | 21.87 [21.78–22.00] | 21.52 [21.30–21.59] | 16.87 [16.77–16.93] |
| `throws_10` | 12.52 [12.42–12.71] | 12.60 [12.52–12.70] | 10.54 [10.37–10.61] |
| Empty process | 5.34 [5.34–5.37] | 5.28 [5.24–5.51] | 5.27 [5.23–5.33] |

Each RSS cell gives median [min–max] over three alternating runs. Both changes reduce peak RSS by 22.86% on objects and 15.88% on throws.

## Instruction counts

Run `perf stat -e instructions:u` three times on each input. The table shows the min–max range in millions of instructions with default glibc and the change of the minimum. The two target scripts also use fewer instructions.

| Row | Baseline, M | Both changes, M | Change |
|---|---:|---:|---:|
| `interp_load loopfn 3000000` | 866.302–866.302 | 866.301–866.302 | <0.001% |
| `interp_load calls 1000000` | 571.353–571.354 | 571.353–571.353 | <0.001% |
| `interp_load fib 27` | 248.674–248.674 | 248.674–248.675 | <0.001% |
| `interp_load closures 1000000` | 1,942.705–1,942.706 | 1,942.711–1,942.711 | <0.001% |
| `interp_load objects 1000000` | 744.198–744.198 | 744.200–744.201 | <0.001% |
| `objects_10` | 1,018.693–1,020.854 | 937.145–937.154 | −8.01% |
| `throws_10` | 307.879–307.881 | 299.125–299.126 | −2.84% |

Repeating the five micro rows with `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` also kept the absolute change below 0.001% on each row. Both runs pass the +0.5% gate with the default release profile.

## Merge order

Land the index lookup change first. In a separate three-run comparison against `f29a583d`, concatenation alone reduced `throws_10` from 12.46 [12.38–12.63] to 10.46 [10.39–10.58] MiB. It increased `objects_10` from 21.98 [21.80–22.21] to 24.96 [24.81–25.11] MiB. Hypothesis: it moves the first collection later into the array phase while missing-index atoms still grow the atom table. The combined build reduces both peaks.

## Browser and correctness

The headless `bare-drive` profiling builds ran the decision benchmark's `style_100k` fixture with `BARE_JS_EXEC=new` and `BARE_SEED=1`. All six runs passed the completion marker. Peak RSS moved from 46.27 [46.09–46.39] to 45.86 [45.73–46.00] MiB. Median ready time was 204.3 [189.2–223.6] to 191.7 [190.0–230.4] ms. The machine was shared during these runs.

The concat allocation test failed before the change and passed afterward. The value test covers operand order, the signed 32-bit minimum, empty strings, Latin-1 and UTF-16 strings, and long strings. The narrow js2 runtime, string and script filters and the js2 typecheck passed. The targeted test262 comparison covered addition, array push and forEach, and the two own-property queries. Both trees passed the same 627 tests with three failures and four skips. Lost passes: zero.

Raw data and scripts are in `/data/bare-tmp/r22/mem/` on archMachine. Not tested locally: Miri, Windows, the rest of test262, the full workspace suite, public browser journeys and idle-machine time to ready.

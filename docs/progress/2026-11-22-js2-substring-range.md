# Copy only selected string units in js2

`substring`, `slice`, `substr`, and `codePointAt` copied the full source through `Realm::units`. `charAt`, `charCodeAt`, and `at` already indexed flat strings directly. Trim, split, and search scan for boundaries or matches.

The range methods read the stored length and coerce the arguments in order. The native reentry path holds the collector when a coercion calls script. Full ranges reuse the source cell. Empty ranges return before flattening. Partial ranges flatten a rope once and pass the selected span to the existing regexp range helper. On flat sources, only the result span is scanned and copied. A two-byte span narrows if all of its units fit in one byte. `codePointAt` reads at most two units after flattening.

The new test failed on main because a full range of a 256 KiB flat string returned a new cell. The same test checks full-range reuse without flattening a rope, and checks a narrow result and both surrogate halves from short ranges. The coercion test allocates in `valueOf` under a low GC threshold and checks receiver, start, and end coercion order.

## Compatibility

Compare js2 against main `d1e9876f` at test262 commit `e4fbf577`. Both passed all 115 cases. No pass was lost.

| test262 directory                 | Main passes | Branch passes |
| --------------------------------- | ----------: | ------------: |
| `String/prototype/substring`      |          46 |            46 |
| `String/prototype/slice`          |          38 |            38 |
| Annex B `String/prototype/substr` |          15 |            15 |
| `String/prototype/codePointAt`    |          16 |            16 |

## Rust Book search

Run live on archMachine (AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1). Compare matching release builds from main `d1e9876f` with js2, `BARE_SEED=1`, and the benchmark glibc tunable. Rotate main and branch across three trials. Every trial passed with 21 replies and 870,650 bytes.

| Metric | Main | Branch |
|---|---:|---:|
| User-mode instructions, minimum in M [min–max] | 4,187.62 [4,187.62–4,199.59] | 3,984.07 [3,984.07–3,999.18] |
| Driver-reported peak RSS, median in KiB [min–max] | 101,144 [101,032–101,412] | 101,096 [101,068–101,284] |

The instruction minimum fell by 203.55 M (4.86%). RSS in the table comes from the driver report on the same passing trials.

## Stop

The first RSS harness read stderr. The `wait4` helper writes its result to stdout and discards the child streams. The second run read stdout and expected a driver report on disk. The harness did not pass the driver's `--out` option. The two-attempt rule stopped the work before the separate RSS gate or PR.

`t` passed the string range, iterator, regexp, and related built-in checks. The final added assertions for a flat wide source and a short range of a large flat source were not rerun. Typecheck, the string cost rows, the five interpreter micro rows, the other three journeys, Windows, and the full suite were not run.

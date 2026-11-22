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

`t` passed the string range, iterator, regexp, and related built-in checks. The final added assertions for a flat wide source and a short range of a large flat source were not rerun. The owner's session ran the rest on `d1e9876f` against this branch. `cargo test -p bare-js2 --lib bi_string` passed 19 tests, including the final assertions. `cargo check -p bare-js2 -p bare` passed. js2 has no string row in `tools/js/cost_rows.txt`. The five interpreter micro rows (minimum of 3, glibc) did not change:

| Row | Main | Branch | Change |
| --- | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 800,255,842 | 800,255,242 | -0.000% |
| `interp_load calls 1000000` | 556,295,052 | 556,294,825 | -0.000% |
| `interp_load fib 27` | 241,938,008 | 241,937,802 | -0.000% |
| `interp_load closures 1000000` | 1,741,054,588 | 1,741,054,404 | -0.000% |
| `interp_load objects 1000000` | 699,644,090 | 699,644,092 | +0.000% |

The separate `wait4` RSS gate, the other three journeys, Windows and the full suite were not run.

# Cache admission on the second sight and FxHash

Date: 2026-11-24. Base: main cecf834b. Design: `../design-parse-cost.md`, step 1 (F1 and H1).

## Change

- F1: `code_cache::compile_on_miss` keeps no `PortableUnit` on the first compile of a script. It records the key in a `seen` set. The second compile captures the unit and stores it. The third sight is a hit. The `seen` set holds at most 16,384 keys (about 640 KiB); at the cap it starts again.
- H1: the maps of the parser and the compiler use FxHash (`crates/vex/src/fx.rs`, the hash of rustc) in place of SipHash. `build.rs` compiles `parse.rs` too, so it includes `fx.rs`.

## Measurements

Machine: archMachine. Input: the 6 worker chunks of Microsoft Teams (19.6 MB), run in one page by a scratch example (`Browser::<New>::run_script` per file, not committed). Release build. 3 runs each.

| | main | this change |
| --- | --- | --- |
| instructions:u | 3,801.6–3,801.7 M | 3,603.1 M (−5.2%) |
| peak RSS (VmHWM) | 90.1–90.9 MB | 73.0–73.2 MB (−19%) |
| page faults | 20,150–20,371 | 15,179–15,183 (−25%) |
| wall | 0.381–0.386 s | 0.353–0.360 s |

`code_cache_load` on chunk 0 (instructions, minimum of 3 runs):

| realms | main | this change |
| --- | --- | --- |
| 1 | 602.1 M | 572.4 M (−4.9%) |
| 2 | 612.3 M | 1,153.8 M (+88%) |
| 5 | 640.7 M | 1,182.8 M (+85%) |

A script that loads exactly twice now compiles twice. From the third load the cost is the same as on main. V8 uses the same rule: it makes its code cache on the second compile and uses it from the third. The disk cache (C1, next step) uses the same admission. On a reload it gives the hit, so this cost moves to the first run of a profile only.

## Cost gate

- `interp_load` (loopfn, calls, fib, closures, objects): all within ±0.001%.
- `cost_matrix.sh -g 'js2_|code_cache'`: 4 rows. `js2_parse_load full stmts 1` −11.2%. The other 3 rows: no change.
- `cost_matrix.sh -g 'navigation_load|loader_load|browser_load|preload_load|resources_load'`: 46 rows, mean −0.26%, 0 over the rule.

## Tests

- Changed: 3 tests of `code_cache_tests.rs` load each script 3 times now and expect 2 misses and 1 hit.
- New: `code_cache_admit_tests.rs`: the first sight keeps no unit; the `seen` set starts again at its cap; the Fx map keeps distinct keys of each length.
- `t`: 101 passed.

## Not tested

- The journeys and the full cost matrix.
- A page that loads the same script in exactly two realms (cost above).
- test262 counts. The parser output does not change; only the hash of its maps changes.
- Windows.

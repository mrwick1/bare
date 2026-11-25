# Gap census of Teams (Phase 1 tool and first result)

## Change

- `crates/vex/src/census.rs`: with `BARE_CENSUS=FILE`, each miss of a name on a host object (a read or `in`) is written to FILE as `TAG NAME get|has COUNT`, at the counts 1, 2, 4, 8, and so on. `hs_walk` (`api_host.rs`) calls it before `Walk::Miss`. Off, the cost is one load of a `OnceLock` on a host miss.
- `tools/ref/chrome-has.mjs`: asks Chrome which of the census names it has on the same kind of object.
- `docs/internal/teams/gap-census.md`: the method and the ranked gaps of the first run.

## Check

- New test file `crates/vex/src/census_count_tests.rs`: the lines at the powers of 2, and get and has kept apart. It passes.
- Teams offline replay with the census on (archMachine, 1 run): 0 misses on the tape. The census wrote 185,516 lines, and `chrome-has.mjs` found 45 names that Chrome has.
- `interp_load` rows (`tools/js/instr.sh`, minimum of 3, archMachine, glibc; base c9a63b27, which has the same Vex code as the branch base 96e3edd7):

| row | base | new | change |
| --- | --- | --- | --- |
| `loopfn 1000000` | 261,291,794 | 261,291,526 | -0.0001% |
| `calls 1000000` | 549,340,343 | 549,341,302 | +0.0002% |
| `fib 30` | 1,020,973,706 | 1,020,973,659 | -0.0000% |
| `closures 200000` | 358,802,916 | 358,802,636 | -0.0001% |
| `objects 200000` | 142,397,736 | 142,397,785 | +0.0000% |

These rows have no host objects, so the census path does not run in them.
- Not tested: Windows; the census on a live run; Outlook.

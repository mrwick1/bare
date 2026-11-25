# Gap census of Teams (Phase 1 tool and first result)

## Change

- `crates/vex/src/census.rs`: with `BARE_CENSUS=FILE`, each miss of a name on a host object (a read or `in`) is written to FILE as `TAG NAME get|has COUNT`, at the counts 1, 2, 4, 8, and so on. `hs_walk` (`api_host.rs`) calls it before `Walk::Miss`. Off, the cost is one load of a `OnceLock` on a host miss.
- `tools/ref/chrome-has.mjs`: asks Chrome which of the census names it has on the same kind of object.
- `docs/internal/teams/gap-census.md`: the method and the ranked gaps of the first run.

## Check

- New test file `crates/vex/src/census_count_tests.rs`: the lines at the powers of 2, and get and has kept apart. It passes.
- Teams offline replay with the census on (archMachine, 1 run): 0 misses on the tape. The census wrote 185,516 lines, and `chrome-has.mjs` found 45 names that Chrome has.
- `interp_load` rows: see the PR.
- Not tested: Windows; the census on a live run; Outlook.

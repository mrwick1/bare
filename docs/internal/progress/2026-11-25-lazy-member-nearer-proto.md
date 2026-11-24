# A lazy member on a nearer prototype is found at its first read

Date: 2026-11-25. Base: main 04d0b87c.

## Change

- Vex asks the host for a lazy member when a lookup misses on an object with `F_LAZYM` (`crates/vex/src/api_host.rs`). Before, it read only the asked object again.
- bare often answers at the end of the chain (`EventTarget.prototype`) and puts the member on an interface prototype nearer in the chain. Example: `sheet` goes on `HTMLLinkElement.prototype`. The first read then gave `undefined`; the second read gave the value.
- Now, when the host answered and the asked object still has no such member, the walk starts again at the first object, once. The objects that were asked already are not asked again (`hs_lazy` keeps one ask per object and name).
- Why: Teams loads its CSS chunk with `chunkLoader.loadChunk(["griffel"])`. It finds the parsed `<link id=griffel-style>`, reads `link.sheet` and, when that is not set, waits for `load`. The `load` ran long before, so the wait never ended. The React root never rendered, and Teams stayed on its splash screen.

## Cost

- The new code runs only after the host answered a lazy ask: once per object and name until the next collection.
- Vex rows `interp_load`, base 04d0b87c, on arjun-e16, glibc, `tools/js/instr.sh` (minimum of 3 runs of `instructions:u`):

| Row | base | new | change |
|---|---|---|---|
| `interp_load loopfn 3000000` | 800286154 | 800286969 | +0.000% |
| `interp_load calls 1000000` | 556325871 | 556326243 | +0.000% |
| `interp_load fib 27` | 241968755 | 241969039 | +0.000% |
| `interp_load closures 1000000` | 1740264973 | 1740265260 | +0.000% |
| `interp_load objects 1000000` | 699671723 | 699671736 | +0.000% |

- jemalloc was not run: no row is over the rule under glibc.

## Tests

- New: `browser_link_sheet_tests.rs`. It fails on main (`undefined`) and passes with the change.
- `t`: all pass. `bare-vex` tests that match `lazy`: all pass.

## Not tested

- Miri: not run. The change adds no unsafe code.
- test262 counts: not run.
- A live Teams run with this change: not yet.

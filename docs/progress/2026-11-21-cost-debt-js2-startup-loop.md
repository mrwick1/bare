# Reduce js2 dispatch and startup cost

## Iterator dispatch

Run `IterOpen` in the cold dispatcher. When `IterNext` finds a hole, resume that instruction in the cold dispatcher for property lookup. Dense steps still read the element and advance the index in the hot loop. The cold path keeps the safepoint and exception handling for getters and Proxy traps.

The original #876 report measured +2.780% / +2.768% for `loopfn` and +1.175% / +1.168% for `calls` on glibc / jemalloc. Forced single-unit builds still grew by +1.483% / +1.476% and +1.098% / +1.091%. The new iterator checks put setup, collection and exception paths in the same dispatch function as ordinary bytecode. Neither benchmark uses an iterator. Hypothesis: those extra paths changed register allocation in the shared loop.

## Startup

Register `eval` and `Proxy` in the existing global lazy table. First use of `Proxy` adds its static member table. The global table holds the original `eval` function for direct calls and the host API, including after a script replaces the global. Create `Symbol.unscopables` when the engine first checks a `with` binding or makes the properties of `Symbol`.

The combined #889/#890 report measured +1.247% / +0.984% at startup, and +1.618% / +1.029% with forced single-unit builds. The startup script no longer creates either native function or the symbol, or installs the `Proxy.revocable` table. A Rust test checks which values the startup script leaves unmaterialized.

Merging #888 added a startup shape transition for `arguments.length`. After that merge the first fix candidate was +0.792% on jemalloc. Defer construction of the arguments shape to the first arguments object. Pin the shape for reuse. With only that change, the jemalloc minimum fell from 4,472,580 to 4,431,663 instructions in a separate default-build comparison. No arguments object is created by the startup script.

## Instruction counts

archMachine, Rust 1.96.0, release builds. Each cell is the minimum of three user-mode instruction counts from `perf stat -e instructions:u`. Set `glibc.malloc.tcache_count=65535` and preload `libjemalloc.so.2` for the jemalloc runs. Use `interp_load loopfn 3000000`, `interp_load calls 1000000` and `script_run` on `var x = 1;`.

The pre-PR bases are `c81eb892` for the #876 rows and `aded3c3a` for startup, as in the [iterator report](2026-11-21-js2-t262-array-iterators.md) and the [dynamic scope and Proxy report](2026-11-21-js2-dynamic-scope-proxy.md). The before tree is `ef3f809c` at the tip of #889 with #890 included. The fix tree is `889b7181`, which includes main through `d49f13ce`. The release profile already selects one codegen unit for js2 in the later trees. The forced single-unit build sets `RUSTFLAGS="-C codegen-units=1"` on all crates in each tree.

| Build | Row | Allocator | Pre-PR base | Before fix | After fix | Before vs base | After vs base | Spread (base / before / after) |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Release | `loopfn` | glibc | 863,308,846 | 821,305,238 | 800,297,235 | -4.865% | -7.299% | 583 / 938 / 282 |
| Release | `loopfn` | jemalloc | 867,069,752 | 825,105,085 | 804,059,921 | -4.840% | -7.267% | 1,148 / 1,594 / 726 |
| Release | `calls` | glibc | 595,367,728 | 552,355,936 | 544,347,563 | -7.224% | -8.570% | 393 / 1,391 / 346 |
| Release | `calls` | jemalloc | 599,096,008 | 556,124,159 | 548,078,391 | -7.173% | -8.516% | 1,079 / 672 / 818 |
| Release | `startup` | glibc | 732,182 | 740,920 | 728,661 | +1.193% | -0.481% | 211 / 973 / 696 |
| Release | `startup` | jemalloc | 4,437,066 | 4,481,466 | 4,431,177 | +1.001% | -0.133% | 1,344 / 1,096 / 1,295 |
| Forced cgu=1 | `loopfn` | glibc | 809,294,220 | 821,305,744 | 800,297,151 | +1.484% | -1.112% | 1,072 / 351 / 685 |
| Forced cgu=1 | `loopfn` | jemalloc | 813,055,343 | 825,105,931 | 804,059,193 | +1.482% | -1.106% | 931 / 908 / 1,163 |
| Forced cgu=1 | `calls` | glibc | 546,345,784 | 552,356,318 | 544,347,233 | +1.100% | -0.366% | 680 / 318 / 184 |
| Forced cgu=1 | `calls` | jemalloc | 550,073,890 | 556,124,385 | 548,078,784 | +1.100% | -0.363% | 1,296 / 1,150 / 729 |
| Forced cgu=1 | `startup` | glibc | 729,889 | 741,150 | 728,990 | +1.543% | -0.123% | 587 / 1,185 / 865 |
| Forced cgu=1 | `startup` | jemalloc | 4,434,866 | 4,480,788 | 4,430,655 | +1.035% | -0.095% | 1,063 / 893 / 1,882 |

All three rows pass under both allocators in both build modes. The default interpreter deltas also include the change to one release codegen unit since #876. The forced single-unit comparison uses the same setting on each tree.

## Checks

63 focused Rust tests pass on Linux. They cover lazy built-ins, the script API, generator and array iteration, Symbol, dynamic scope and Proxy. The tests include forced collection. The js2 typecheck passes.

The Symbol tests include `unscopables` in the own-name list. The `instanceof` test uses a callable whose prototype property can be defined as a getter. The main merges retain the recorded test262 fixture rows from both sides. The fixture was not regenerated.

Windows, Miri, test262, browser journeys and the full Rust suite were not run locally.

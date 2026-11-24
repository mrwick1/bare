# 2026-11-22 — Correct the OLD baseline for crates.io

## Cause and fix

SvelteKit's `Mn(app, element, ...)` calls `Nn(app, element, ...)` to start the app. OLD in Auto and Byte mode instead called `lt`. A probe in the recorded `CCdz5Gj3.js` showed that `lt` received the app module namespace and the target element. Both lack `origin`, so the equal-origin branch tried to read `slice` on the missing `href`. Tree mode avoided the error on the same tape.

The OLD module linker reserves slots for imports and the default export before it adds lexical declarations. The bytecode scope template omitted those slots. Captured names after the omitted slots read the wrong binding. The template now follows the linker's order. Bytecode slot reads use the existing lookup for a live import. The failing test called the wrong function in Auto and returned 19 instead of 42. The fix also covers changes to a named import after a bytecode closure first reads it.

QuickJS assigns import slots in `add_import`, resolves captured variables to indexed references in `resolve_scope_var`, and wires imports to the exporting module in `js_inner_module_linking`. The local comparison used QuickJS 2026-06-04. OLD uses a scope template to assign indices and a lookup to follow live imports.

## Journey cutoff

The original journey checks that `body` is visible. Both engines report 39 successful replies and 76,696 bytes there. After the fix, both attempt to load `1.BJM5E5nV.js` and `1.BbiiKyV9.css`. Both resources are missing from the supplied tape. The DOM probe at the cutoff found zero headings and forms in both engines. OLD showed the site's error paragraph before the fix.

A separate replay filled the missing resources and waited for a visible heading. Both engines fetched `/api/v1/site_metadata` and `/api/v1/summary` with HTTP 200. The final run reported 54 replies and 123,912 bytes in each engine. OLD raised a later microtask `ReferenceError: Cannot access 'r' before initialization`. js2 raised `ReferenceError: Intl is not defined`. The ratios below apply to the original journey cutoff. This run gives no full-mount ratio.

## Instruction and memory measurements

Baseline `fac39cc0` includes the heads of #944 and #948. After #948 merged, the range to main `60188a32` contained only a design document update. Both builds used Rust 1.96.0 release with symbols on archMachine (Ryzen 9 5900HX). Each cell uses three runs with alternating engine order, `BARE_SEED=1`, `LC_ALL=C` and the glibc thread cache set to 65,535. Replay uses the same 39-reply tape and a virtual clock. Instruction counts use `perf stat -e instructions:u`. Peak RSS uses wait4 in three separate runs. Raw measurements are in `/data/bare-tmp/r22/js2-crates-io`.

Instruction entries give the minimum and min–max range, in millions. RSS entries give the median and min–max range, in KiB.

| Transport | OLD before | js2 before | OLD after | js2 after | js2 / OLD |
|---|---:|---:|---:|---:|---:|
| Tape instructions | 75.794 [75.794–76.013] | 143.721 [143.721–144.020] | 134.080 [134.080–134.307] | 143.772 [143.772–144.010] | 1.90× → 1.07× |
| Live instructions | 87.573 [87.573–88.029] | 282.783 [282.783–298.905] | 270.924 [270.924–280.700] | 283.202 [283.202–292.642] | 3.23× → 1.05× |
| Tape RSS, interleaved | 23,564 [22,752–23,708] | 25,840 [25,068–26,344] | 25,460 [24,916–25,624] | 25,860 [25,332–26,808] | |
| Live RSS | 24,036 [23,688–24,580] | 31,228 [30,500–31,616] | 31,772 [31,464–31,900] | 31,652 [31,072–32,100] | |

The correction raises OLD's RSS as it loads and executes the modules it used to skip. The js2 tape RSS median first rose 500 KiB in two batches. An interleaved rerun moved the median by 20 KiB, within the 1,276 KiB baseline spread. Live js2 RSS rose 424 KiB, within its 1,116 KiB baseline spread.

## Callgrind split

Callgrind 3.25.1 used `--cache-sim=no --smc-check=all-non-file --separate-callers=1`. The split follows the real-pages design. It attributes exclusive instructions by symbol or source line and uses the immediate caller for unclassified work. The units are millions of instructions.

| Subsystem | OLD before | js2 before | OLD after | js2 after |
|---|---:|---:|---:|---:|
| Network, TLS, and URL | 18.67 | 21.29 | 21.29 | 21.28 |
| HTML parse | 0.30 | 0.29 | 0.29 | 0.29 |
| Style | 5.44 | 5.77 | 5.77 | 5.77 |
| Layout and text | 0.19 | 0.18 | 0.18 | 0.18 |
| Paint | 0.07 | 0.07 | 0.07 | 0.07 |
| JS parse and compile | 29.50 | 69.82 | 61.11 | 69.82 |
| JS execution | 9.26 | 16.37 | 24.88 | 16.36 |
| Engine-host boundary | 0.21 | 2.01 | 0.52 | 2.01 |
| GC | 0.00 | 0.64 | 0.00 | 0.64 |
| DOM and browser | 0.93 | 1.44 | 1.57 | 1.44 |
| Images | 0.01 | 0.02 | 0.02 | 0.02 |
| Driver and other | 15.57 | 29.97 | 24.41 | 30.03 |
| **Total** | **80.15** | **147.87** | **140.11** | **147.93** |

Before the fix, OLD parsed 11 modules. After the fix, it parsed 33, the same count as js2. OLD's `module_parse` went from 32.36 M to 56.49 M inclusive and synchronous module execution went from 4.71 M to 17.37 M. The extra work starts after the repaired call. The network count stayed flat because most of those modules had already been fetched as preloads. js2 parsed and compiled the modules OLD left unused. The correction closes 88% of the Callgrind instruction gap. At the corrected cutoff, js2 spends 14.08 M inclusive in 33 `compile_module` calls. OLD runs module top-level statements in its tree evaluator. This change stops at the OLD correctness fix.

## Checks

The OLD module, bytecode scope, and code generation tests pass (24, 5, and 18). Typecheck passes for `bare-js`, `bare-js2`, and `bare`. The module-only test262 runner used OLD Auto mode. It went from 546 to 549 passes out of 599 cases, with 17 skips in each build and no lost passes. The three new passes cover top-level await. The js2 test262 baseline stayed untouched.

All eight `loader_load` rows pass the cost rule. `modules 1` rose 0.526% under glibc and fell 0.069% under jemalloc. The other glibc rows range from −0.143% to +0.103%. The patch changes the OLD engine.

The #948 controls use their recorded tapes. DEV and Python.org use the #927 tapes. Rust-lang.org runs live. Reply sets and pass status agree before and after per engine. Node.js still fails in js2 on the known Intl gap. In the default build, every js2 minimum stays within its baseline spread or falls. Four OLD minima rise above their original three-run spreads.

| OLD control | Before spread, M | Change in minimum, M | Change in minimum |
|---|---:|---:|---:|
| React Docs | 22.715 | −2.162 | −0.032% |
| GitHub Docs | 0.438 | +2.146 | +0.054% |
| Rust Book | 1.243 | +3.785 | +0.051% |
| Node.js | 2.467 | +7.477 | +0.201% |
| DEV | 0.732 | +0.252 | +0.008% |
| Python.org | 0.256 | +0.479 | +0.041% |
| Rust-lang.org | 0.966 | +0.027 | +0.032% |

### Cost debt

The single-codegen-unit rerun also exceeds the run spread in OLD on GitHub Docs (+0.173%), Rust Book (+0.123%), Node.js (+0.437%), DEV (+0.018%), and Python.org (+0.047%). In js2, GitHub Docs rises +0.038% past its spread. These are journey control results. No loader row exceeds the cost rule under both allocators. The cause of the control deltas remains unverified. The PR records the deltas under rapid mode.

Local checks omit the full workspace suite, Windows, Miri, js2 test262, and full-mount parity.

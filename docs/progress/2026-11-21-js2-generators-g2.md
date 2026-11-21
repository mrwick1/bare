# js2 generators in the compiler (step G2 of `docs/design-js2-async.md`)

What is built. The compiler builds `function*` declarations, expressions, methods and class methods, and `yield` (not `yield*`: that is G3). It uses the resumable frames of G0/G1 (#801).

- `compile_gen.rs` (new): `GenInit` at the end of the call (after the parameters are bound), `Yield` with a save list, `RetGen`, `new.target` as `undefined`. The save list is the set of registers that hold a value at the `yield` (`Fc::live`, the same set as the root map), without the temporaries of the `yield`. A resume writes the sent value into register `sent` and the mode into register `mode`. Both belong to the `yield` only, so a `throw()` does not change a variable that the handler reads.
- Resume dispatch after `Yield`: mode 0 continues; mode 1 is `Throw sent`, so the handlers around catch it; mode 2 runs `unwind(0)` (the `finally` blocks and the `return()` of each `for-of` around), then `RetGen`.
- `for-of` over a generator: `IterOpen` keeps no index for a generator, `IterNext` calls `Realm::gen_nested` (cold path), `IterClose` calls `return()` for it (the op is cold now). `gen_nested` runs the generator on a pooled machine, with the collector held, as a native call does.
- `Closure` of a generator makes the function object with the `%GeneratorFunction.prototype%` chain (lazy, cold: `make_generator_function`), and the `prototype` object comes with the function (`fn_props_slow`). `CF_GEN` is a flag of `Code`, derived from `gen_kind` in `code_flags`, so a lazy stub keeps it.
- Async generators and async functions stay unsupported (`unsup`).
- The test262 skip file allows `generators`. The subset has `built-ins/GeneratorPrototype` and `built-ins/GeneratorFunction`.

## Tests

`compile_gen_tests.rs`: 11 tests, each run plain and under constant collection (next and return values; the sent value in expressions; `throw()` into a handler, also with a variable that the handler reads; `return()` runs `finally` blocks, also with a `yield` inside `finally`; a finished generator and a running generator (TypeError); `for-of` with `break`, `throw` and `return`, nested; parameters, `this`, `arguments`; object and class methods, `new g()`, `new.target`; many live values and closures; 20,000 dropped generators; the prototype chain of the spec).

Three older tests assumed that `function*` is unsupported. They now use `async function*` or `async function` for the unsupported case.

`cargo test --locked -p bare-js2 --lib` (archMachine, debug build): 561 pass, 0 fail.

### test262 (`tools/js/test262_js2.sh`, archMachine)

Base: `287805fe`. Branch: this change. Same subset on both, plus the new directories. 0 lost passes. 3,222 new passes.

| Directory | Pass | Fail |
| --- | ---: | ---: |
| `language/expressions/generators` | 84 | 12 |
| `language/statements/generators` | 61 | 12 |
| `language/expressions/yield` | 21 | 42 (all `yield*`, step G3) |
| `built-ins/GeneratorPrototype` | 58 | 0 |
| `built-ins/GeneratorFunction` | 2 | 12 (no `GeneratorFunction` constructor) |

The other failures in the generator directories need `yield*` or the iterator protocol of destructuring (G3), `eval`, or are shared with plain functions (`arguments` as a parameter name, `scope-paramsbody-var-open`, restricted `caller` and `arguments`, the lazy `prototype`).

## Cost

Base: `287805fe`. Release builds, own `CARGO_TARGET_DIR` for each tree, `tools/js/instr.sh` (minimum of 3 runs), machine archMachine. Parts of the hot loop that this change touches: the `Closure` arm (one flag test), the `IterOpen` and `IterNext` arms (one test of the index register on the non-array path), `IterClose` (moved to the cold copy).

| Row | Base (default) | Branch (default) | Change | Base (cgu=1) | Branch (cgu=1) | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 842,396,375 | 848,396,280 | +0.712% | 845,385,091 | 845,384,932 | -0.000% |
| `interp_load calls 1000000` | 587,453,181 | 588,453,226 | +0.170% | 565,434,564 | 562,434,399 | -0.531% |
| `interp_load fib 27` | 260,843,934 | 260,525,663 | -0.122% | 248,752,897 | 249,070,365 | +0.128% |
| `interp_load closures 1000000` | 1,964,380,304 | 1,949,808,510 | -0.742% | 1,934,829,875 | 1,924,401,215 | -0.539% |
| `interp_load objects 1000000` | 722,258,735 | 716,324,612 | -0.822% | 731,305,085 | 717,238,883 | -1.923% |
| `js2_gen_load next 100000` | 170,286,614 | 170,355,585 | +0.041% | 166,309,179 | 165,047,600 | -0.759% |
| `js2_gen_load host 100000` | 101,267,978 | 101,571,357 | +0.300% | 99,246,253 | 98,949,655 | -0.299% |

`loopfn` is +0.71% in the default build (exactly 2 instructions per turn, and the loop has no op that this change touches) and 0.000% at `codegen-units=1`. So it is a code layout effect and it passes the rule (within +0.5% at `codegen-units=1`). I did not run the jemalloc counts or the full cost matrix; the rows above are the js2 micro rows only (hypothesis: the `bare` rows are not affected, because `crates/bare` links no code of this change; not compared).

### The cost of `yield` (compiled generator, `script_run`, default build, 100,000 turns, total instructions of the whole script)

| Script | Instructions | Per turn |
| --- | ---: | ---: |
| `g.next().value` of a generator with 5 locals live | 251,858,276 | 2,519 |
| `o.next().value` of a plain method that returns an object | 101,077,775 | 1,011 |
| `for (v of g(N))` over the same generator | 135,650,177 | 1,357 |

So a `next()` call costs about 1,500 more than a plain method that returns an object. The `for-of` path is 45% cheaper than the method path, because it makes no result object and no native call. The hand-written `js2_gen_load host` row (a Rust loop on `Vm::resume`, no result object) is about 990 per resume. A plan for G3: run the generator frame of `for-of` in the same machine (no nested `run`), with the end signalled by the jump of `IterNext`. I did not do it here.

## Not tested

Windows (the VM was not used). Miri. The browser and `bare` journeys. Wall time. The jemalloc counts and the full cost matrix. The `GeneratorFunction` constructor (absent). `yield*`, async generators, async functions (later steps). The test262 numbers above come from the runner binary that I built before the last edit of the three older tests; the sources of the engine did not change after it.

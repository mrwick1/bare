# Dynamic scope and Proxy in js2

## Name resolution

The parser records direct calls to `eval` and the scope of each `with`.
Keep register and context-slot instructions where no dynamic environment can shadow a name.
Capture local bindings that a direct eval can read, including bindings in enclosing functions.
Reuse the context chain for object environments and for extra bindings in a sloppy eval.
The compiler marks each dynamic scope in the chain.
Resolve a name across those scopes from the innermost scope out.
Keep the resolved reference across a right-hand side that can change the scope object.
Use the same reference for reads, writes, deletes, and the receiver of a call.
Check `Symbol.unscopables` in a with environment.
Strict eval keeps new var bindings in its own scope.

A realm-wide name table would add lookups to ordinary code and would need frame identity to keep recursion correct.
A separate interpreter for affected functions would duplicate control flow, exception handling, and collection rules.
Context slots keep one interpreter and put the extra work at the affected references.

## Proxy

Use a free object kind in `F_KIND`.
Store the target and handler in traced slots.
Give proxies a shape that the property inline cache cannot install.
Use the existing slow path to look up the trap, call it with the handler as receiver, and check the result against the target.
Forward operations with no trap to the target.
Stop ordinary prototype cache construction at a proxy.
A separate cell kind would also keep proxies out of the shape cache.
It would require changes to each path that accepts an object value, including rooting and conversion.

## Checks

Run the listed test262 regressions first and use the pinned baseline to check for lost passes.
Count user instructions for the interpreter workloads, startup, and parsing on both allocators.
Use the minimum of three runs on each build.

## Dynamic scope results

The pinned subset gains 691 js2 passes with no lost passes against `aded3c3a`.
The old-only count falls from 200 to 143.
All 53 assigned cases pass.
Five narrow Rust tests pass on Linux, including forced collection.
The refreshed subset reruns js2 and reuses the old-engine statuses.
Two full runs agreed on every old-engine status, and the old engine and fixtures did not change.

Instruction counts use the minimum of three runs on archMachine.
The startup script is `var x = 1;`.
Parser runs use `bodies 10` in full, lazy and function modes.
The release profile builds js2 with one codegen unit.
No row grows by more than 0.5% on both allocators.

| Row | glibc | jemalloc |
| --- | ---: | ---: |
| loopfn | -0.000% | +0.000% |
| calls | +0.000% | +0.000% |
| fib | +0.002% | +0.016% |
| closures | +0.000% | +0.000% |
| objects | +0.000% | +0.001% |
| startup | +0.348% | +0.839% |
| parse-full | -0.006% | -0.019% |
| parse-lazy | -0.002% | -0.013% |
| parse-fns | -0.007% | +0.018% |

Windows, Miri and the full Rust suite were not run locally.

## Proxy results

Proxy objects use kind 14 and an uncacheable dictionary shape.
The target and handler live in traced slots.
The property-get inline cache keeps its existing hit path.
Prototype cache lookup stops at a proxy.

The pinned subset gains 23 js2 passes with no lost passes.
All 19 assigned cases pass.
The old-only count falls from 143 to 121.
The Proxy test262 directory has 268 passes and 43 skips.
Five narrow Rust tests pass on Linux, including forced collection.
The two touched crates pass cargo check.

These rows compare against the dynamic-scope build and use the same workloads and run count.
No row grows by more than 0.5% on both allocators.

| Row | glibc | jemalloc |
| --- | ---: | ---: |
| loopfn | +0.001% | +0.005% |
| calls | +0.001% | +0.007% |
| fib | +0.002% | +0.001% |
| closures | +0.000% | +0.002% |
| objects | +0.001% | +0.001% |
| startup | +0.840% | +0.146% |
| parse-full | +0.004% | +0.001% |
| parse-lazy | +0.001% | -0.008% |
| parse-fns | -0.004% | +0.001% |

Windows, Miri and the full Rust suite were not run locally.

The combined build grows startup by +1.247% with glibc and +0.984% with jemalloc against `aded3c3a`.
With `RUSTFLAGS="-C codegen-units=1"`, startup grows by +1.618% and +1.029%.
Hypothesis: eager registration of eval and Proxy and the additional well-known symbol add startup work.

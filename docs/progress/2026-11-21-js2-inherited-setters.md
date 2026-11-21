# 2026-11-21 — js2 inherited host setters

A first assignment to `document.adoptedStyleSheets` left the element black and made an own property on the document. Both named store paths looked for a descriptor on each prototype and prepared lazy engine built-ins. They missed lazy host members. A store now asks the host for a missing member of a marked object and checks the descriptor again. The interpreter calls the setter with the original receiver. The own-data inline cache uses the same fast path. The browser marks `ShadowRoot.prototype` so the walk can find its stylesheet adoption setter there.

The browser regression also reproduced the bug with `document.onchange`. It stored a function on the document and an event dispatch did not call it. The same lazy registration covers `on*` handlers on `Window`, `Document`, `HTMLElement`, `SVGElement`, and the body and frameset prototypes. `Document.dir` and `Document.designMode` also use the lazy host path. The eager `Element.className` and `Document.title` setters passed before the fix.

## Instruction counts

Machine `archMachine`. Release builds in separate target directories. Base `d4777ff0`. Each cell uses the minimum of three `perf stat -e instructions:u` samples at each input. Subtract the minimum at N = 0 from the minimum at N for the workload delta. The startup delta compares N = 0. The sample spread was at most 0.132% for startup and 0.006% for a workload. Set `glibc.malloc.tcache_count=65535` and preload `libjemalloc.so.2` for the jemalloc runs.

| `interp_load` case |         N | glibc workload | jemalloc workload | glibc startup | jemalloc startup |
| ------------------ | --------: | -------------: | ----------------: | ------------: | ---------------: |
| objects            | 1,000,000 |       -0.0001% |          +0.0001% |       +0.054% |          -0.003% |
| closures           | 1,000,000 |       +0.0000% |          -0.0022% |       -0.074% |          +0.009% |
| calls              | 1,000,000 |       +0.0001% |          +0.0001% |       -0.041% |          -0.009% |
| loopfn             | 3,000,000 |       +0.0001% |          +0.0001% |       -0.069% |          -0.024% |
| fib                |        30 |       +0.0000% |          +0.0000% |       -0.018% |          +0.005% |

All rows pass the +0.5% rule with both allocators.
The mean workload change is +0.00002% with glibc and -0.00037% with jemalloc. The mean startup change is -0.030% and -0.005%.

## Checks

- The js2 host-setter regression failed before the fix. The plain JavaScript accessor, lazy built-in `__proto__` setter, and inherited read-only property controls passed before and after the fix. The tests cover dot and computed writes and both strict and sloppy code.
- The browser tests check that the stylesheet changes computed color and the event dispatch calls the handler. They also check shadow-root adoption, reflected `document.dir`, and the no-op setter for `document.designMode`. A headless shadow-root smoke test passed with both engines.
- `t` passed 37 distinct js2 tests and 17 browser tests. `cargo check --locked -p bare -p bare-js2` passed.
- `tools/journeys/triage.sh -e new -j 4` returned 46 passes, 3 known failures, and 1 skip. The failures match #858 and #864: `curl-se-home`, `duckduckgo-html-search`, and `lobsters-comments-back`. The React typing control passed. The runner skipped staging.

Windows, Miri, test262, the full workspace suite, and journey replay were not tested locally.

# Complete the enabled typed-array conformance groups

Add lastIndexOf, reverse, toReversed, with, toSorted, and toLocaleString. Also convert a primitive constructor length before reading the new target prototype. The find methods shipped in the preceding change.

The pinned buffer and typed-array groups at `16c1e3c3` passed 2,357 cases, failed 115, and skipped 500. Both candidate verifier modes pass 2,472 cases, fail none, and skip the same 500. All baseline passes still pass. All 20 targeted Vex tests and the all-target check pass.

On archMachine, compare the minimum of three alternating release runs for each of the five `interp_load` rows under glibc and jemalloc. User-mode instruction changes range from -0.0026% to +0.0029%. The maximum spread across runs of one row, build, and allocator is below 0.050%.

| Skip reason | Cases | Result of the audit |
| --- | ---: | --- |
| Detach helper | 280 | The Test262 host script has no `$262.detachArrayBuffer`. Vex implements buffer transfer and detachment. |
| SharedArrayBuffer | 111 | Vex has no SharedArrayBuffer constructor. |
| Immutable ArrayBuffer | 41 | The buffer intrinsic has no immutable-buffer methods. |
| Float16Array | 37 | The typed-array element table has no Float16Array. |
| Cross-realm hooks | 25 | The Test262 host script has no `$262.createRealm`. |
| Proxy allowlist | 6 | All six pass on the baseline and candidate with a temporary `allow:Proxy` rule. The checked-in rule file is unchanged. |

Coverage excludes the full ECMAScript Test262 suite, browser journeys, typed-array throughput, and Windows.

# Add the typed-array find methods

Use one traversal for find, findIndex, findLast, and findLastIndex. Read the current element before each callback. Stop at the first truthy result and return the value passed to that callback or its index.

The four pinned Test262 groups each change from 4 passes and 30 failures at `4b2c91a6` to 34 passes and zero failures. Four cases per group remain skipped. Both release verifier modes give identical per-test results. The 14 targeted Vex tests and all-target check pass.

On archMachine, compare the minimum of three alternating release runs on each of the five `interp_load` rows with glibc and jemalloc. User-mode instruction differences range from -0.0007% to +0.0011%. The maximum spread across three measurements of one row, build, and allocator is below 0.0051%.

Coverage excludes full Test262, browser journeys, predicate throughput, and Windows.

# Add TypedArray.prototype.every and some

Validate the view and callback before traversal. Read each element when its callback runs and stop when the callback determines the result. The two methods share the traversal.

The pinned Test262 groups change from 8 passes and 72 failures at `64887d95` to 80 passes and zero failures. Eight cases remain skipped. Both release verifier modes give identical per-test results. The regression test covers short-circuiting, callback arguments, element mutation, BigInt truthiness, and empty arrays. The targeted Vex tests and all-target check pass.

On archMachine, compare the minimum of three alternating release runs on each of the five `interp_load` rows with glibc and jemalloc. User-mode instruction differences range from -0.0020% to +0.0035%. The largest spread across three measurements of one row, build, and allocator is 0.0071% or less.

Coverage excludes full Test262, browser journeys, predicate throughput, and Windows.

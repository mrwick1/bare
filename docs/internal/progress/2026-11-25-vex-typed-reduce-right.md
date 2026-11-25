# Add TypedArray.prototype.reduceRight

Traverse elements from the highest index to the lowest. Use the last element as the initial accumulator when the caller supplies no initial value. Read each element at the time of its callback so earlier callbacks can change later inputs.

The pinned reduceRight group changes from 6 passes and 40 failures at `6bceb58e` to 46 passes and zero failures. Forward reduce also has 46 passes and zero failures. Four cases in each group remain skipped under the existing rules. Both release verifier modes produce identical per-test results. Targeted tests cover callback mutations, BigInt accumulators, and empty arrays. The all-target Vex check passes.

On archMachine, compare the five `interp_load` rows using the minimum of three alternating user-mode instruction counts per build and allocator. Every difference is below 0.004% across glibc and jemalloc.

Coverage excludes full Test262, browser journeys, reduction throughput, and Windows.

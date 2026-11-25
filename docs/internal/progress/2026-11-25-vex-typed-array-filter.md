# Add TypedArray.prototype.filter

Read each element before calling the predicate. Keep the selected values and create the species result after the last callback. Use the existing typed-array conversion when writing the result. The new tests cover callback arguments, mutation during iteration, BigInt species, and thrown values.

The pinned filter group changes from 16 passes and 65 failures at `04980b96` to 81 passes and zero failures. Six cases remain skipped under the existing rules. Both release verifier modes produce identical per-test output. The targeted Vex tests and all-target type check pass.

On archMachine, compare five `interp_load` rows with the minimum of three instruction counts per build and allocator. The largest absolute difference is 0.002%. This benchmark measures the interpreter without filter workloads.

Coverage excludes full Test262, browser journeys, filter throughput, and Windows.

# Add TypedArray.prototype.copyWithin

Convert the arguments against the original view length. If the copy is nonempty, recheck the view and clamp both ranges to its current length. Use the existing overlap-safe buffer copy to preserve the source bits.

The pinned copyWithin group changes from 8 passes and 51 failures at `9dd0ffc5` to 59 passes and zero failures. Six cases remain skipped under the existing rules. Both release verifier modes produce identical per-test output. Targeted tests cover overlap, byte offsets, NaN payloads, and resizing during conversion. The all-target Vex check passes.

On archMachine, compare the five `interp_load` rows using the minimum of three user-mode instruction counts per build and allocator. The largest absolute difference is 0.005%. These rows do not measure copyWithin throughput.

Coverage excludes full Test262, browser journeys, copy throughput, and Windows.

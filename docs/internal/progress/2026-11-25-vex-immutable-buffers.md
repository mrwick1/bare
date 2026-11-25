# Add immutable ArrayBuffer support

Add ArrayBuffer.prototype.immutable, sliceToImmutable, and transferToImmutable. Use the existing max-length slot to mark immutable buffers. Typed-array indices report read-only, non-configurable descriptors. Reject writes through typed-array mutators, DataView, and the host's mutable byte access. Structured clone preserves immutability and refuses transfers.

Against `bc4a6d3b`, the pinned buffer, typed-array, array iterator, and Math.f16round groups gain 47 passes. They pass 2,866 cases, fail none, and skip 138. All baseline passes remain. The targeted tests and the all-target checks of Vex, the JS API, and bare pass. The new immutable-buffer test also passes under Miri on the initial base.

On archMachine, the minimum of three alternating release runs per build and allocator across the five interp_load rows changes by -0.0013% to +0.0007%. The maximum spread within one row, build, and allocator is below 0.0031%.

Coverage excludes full Test262, browser journeys, dedicated buffer throughput, and Windows.

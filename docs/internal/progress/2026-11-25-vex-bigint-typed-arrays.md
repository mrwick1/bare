# Add 64-bit BigInt typed arrays to Vex

Add `BigInt64Array` and `BigUint64Array`. Keep the full integer bits in buffer views. Convert with ToBigInt on write. Interpret the stored bits as signed or unsigned on read. Shared typed-array methods reject mixed Number and BigInt content types where required. DataView and structured clone support the same 64-bit element types.

## Conformance

Use the pinned Test262 groups for TypedArray, TypedArrayConstructors, and DataView. Compare against `45d60ea7`. Both release modes produce identical per-test results.

| Run | Pass | Fail | Skip |
| --- | ---: | ---: | ---: |
| Baseline | 1222 | 1077 | 452 |
| Both verifier modes | 1836 | 463 | 452 |

614 previously failing cases pass. Every previously passing case still passes. Targeted Vex tests pass for conversion, wrapping, shared buffers, sorting, property access, and cloning. The Vex and Bare libraries type-check.

## Instruction counts

Compare release `interp_load` binaries on archMachine. Count user-mode instructions with perf. Use the minimum of three alternating runs per build and allocator. The five rows are loopfn, calls, fib, closures, and objects. The maximum increase is 0.003% under glibc and 0.001% under jemalloc.

Coverage excludes full Test262, browser journeys, large BigInt typed-array workloads, and Windows.

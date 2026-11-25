# Run cross-realm buffer tests in Vex

Create Test262 realms on the existing heap. Keep their globals and intrinsic state rooted and track function ownership without keeping dead functions alive. Use the function's realm for script and native calls. Restore the caller's realm on return, throw, or completion of a nested run. Resolve buffer prototype defaults through the realm of the new target, including bound functions and proxies.

Add the Test262 createRealm and evalScript hooks. Enable cross-realm cases in the pinned ArrayBuffer, DataView, and TypedArrayConstructors groups. All 24 newly enabled cases pass. Three cross-realm cases still require SharedArrayBuffer. The pinned buffer and typed-array groups, array iterators, and Math.f16round have 2,890 passes, zero failures, and 114 skips. Across the wider targeted conformance selection, all 10,158 baseline passes remain and the same 81 existing failures remain.

The single-realm interpreter loop omits realm switches. On archMachine, compare the minimum of three alternating release runs per allocator and build against `0012f62c`. The five interp_load instruction counts change by -0.614% to -0.120% across glibc and jemalloc. Maximum spread is below 0.005%. The interp_load executable grows by 487,944 bytes with the second interpreter path.

The focused tests and all-target checks pass. Coverage excludes full Test262, browser integration of realm creation, browser journeys, and Windows.

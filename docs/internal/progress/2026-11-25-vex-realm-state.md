# Split the active Vex realm state

Group the global object, lexical bindings, and intrinsic state into RealmState. Keep the existing heap, call machinery, and garbage-collection ownership in Realm. The collector continues to trace the active lexical environment.

Change the global cache tag when the shape table starts watching a different global. Cached slot addresses must be discarded before a later change can activate a second realm. The new regression fails on the previous implementation.

The pinned Function, Object, buffer, typed-array, arrow function, class, let, and const groups have identical per-test results against `b1fc3774`. Both runs have 10,158 passes, 81 failures, and 1,386 skips. The six targeted tests and all-target checks for Vex and bare pass.

On archMachine, compare the minimum of three alternating release runs per build and allocator for each of the five interp_load rows. User-mode instruction counts differ by less than 0.005%.

This change prepares the state boundary. Secondary realm allocation, function provenance, realm activation, and the Test262 hooks remain. Coverage excludes full Test262, browser journeys, and Windows.

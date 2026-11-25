# Allocate realm intrinsics on an existing heap

Extract realm bootstrap allocation from Realm::new. Allocate the global, core prototypes, and their owned shapes in the supplied heap and shape table. Return the root values and shape pins. The caller chooses which global the inline cache watches.

The new test allocates a second intrinsic set on the same heap. It checks distinct identities and uses the collector to verify the returned roots. The test passes under Miri. Three targeted tests and the all-target checks of Vex and bare pass.

Against `d654350f` on archMachine, the minimum of three alternating release runs per build and allocator differs by less than 0.004% across the five interp_load rows. The largest spread across three measurements of one row, build, and allocator is below 0.0051%.

This change allocates realm bootstrap state. Realm activation, function provenance, and Test262 host hooks remain. Coverage excludes full Test262, browser journeys, and Windows.

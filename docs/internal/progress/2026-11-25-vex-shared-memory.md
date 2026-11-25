# Add shared memory to Vex Test262 agents

Add fixed and growable SharedArrayBuffer backing stores. Typed arrays and DataView access shared bytes under the backing store's lock. Each agent owns its JavaScript heap and imports a reference to the same backing store. The ordinary typed-array path keeps direct access to heap byte cells. Shared access and detached-buffer checks use a separate path.

Add Atomics operations and per-offset FIFO wait queues. The wait operation checks the value and registers its waiter under the same lock. Test262 worker agents run on separate threads. They exchange backing store references and reports. The test host delivers asynchronous notifications and timeouts on the waiting agent. Same-agent notification queues promise reactions before returning to the caller.

The pinned Test262 checkout at `e4fbf577` covers ArrayBuffer, SharedArrayBuffer, Atomics, typed arrays, DataView, array iterators, and Math.f16round. The previous selection had 2,890 passes and 114 skips. All prior passes and skips now pass. The new SharedArrayBuffer and Atomics groups add 493 passes. The total is 3,497 passes with zero failures and zero skips.

Compare release builds with main at `04c36bdc` on archMachine (AMD Ryzen 9 5900HX). Use the minimum of three alternating runs per binary and allocator. Across the five `interp_load` cases, user instructions change by at most 0.003% under glibc and jemalloc. The largest run spread is below 0.005%. A plain Uint32Array probe with one million indexed writes and reads uses 0.165% fewer instructions under glibc and 0.164% fewer under jemalloc. Its maximum run spread is below 0.001%.

A shared counter probe does 400,000 total Atomics.add operations under glibc. Each of three runs per agent count checks the final counter. One agent uses 828.36 million user instructions. Two agents use 846.81 to 852.62 million. Four agents use 874.25 to 877.93 million. These counts include startup, worker coordination, and report handling.

The focused Rust tests cover cross-agent growth, atomic visibility, waiter shutdown, and same-agent promise ordering. Miri passes all three shared-memory tests. Browser exposure, shared structured clone, and browser Worker integration remain separate work. Coverage excludes the full Test262 suite, browser journeys, and Windows.

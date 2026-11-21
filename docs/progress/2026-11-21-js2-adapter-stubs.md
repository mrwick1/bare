# 2026-11-21: Complete js2 browser adapter operations

- Read a symbol description through a shared realm reference. Preserve the difference between a missing description and an empty string.
- Create the iterator prototype when the browser creates the realm. Use the same object for built-in and host iterators. Check constructibility with the engine's constructor test.
- Use the realm wall clock for `Date()`, `new Date()` and `Date.now()`. A virtual session without replay uses its scheduler time plus an epoch offset. A replay session uses the process clock. The old engine keeps its existing behavior.
- Print compact JSON for objects. Show cycles, thrown getters, functions, dates, nodes, and the entries of maps and sets. Honor the output cap. Ignore `toJSON`.
- Count dead object, string and symbol cells during an explicit collection. This uses the mark bits and the free lists after marking. In-task collections skip the counting pass. External growth can trigger a collection between tasks. The threshold is the greater of 4 MiB or one quarter of the native bytes that survived the previous browser collection.
- Borrow the current host call's arguments in place. Use the installed host for named and indexed property sources.
- Both engines give `[object HTMLElement]` for a div until the first read of `HTMLDivElement`. Both give `[object HTMLDivElement]` after that read.
- js2 has no ArrayBuffer, TypedArray, or DataView cells. There are no backing bytes for `bytes_of` to borrow.

## Checks

- `cargo test --locked -p bare --lib browser_new` passed 15 tests on archMachine. The baseline run on the head of #854 reproduced six adapter failures.
- The browser tests include a loopback session with virtual time and no replay. A 25 ms timer advances `Date.now()` and `performance.now()` by 25 ms.
- `cargo test --locked -p bare-js2 --lib api` passed 38 tests. The tests cover a borrowed argument range and exact collection counts for rooted and dead objects, strings, and symbols.

# Share js2 compiled units across realms

## Design

The loader parses a source and builds its code and lazy stubs. Each code has a table of atom pointers. Bytecode already indexes this table. Child function references point into the realm’s full code table. Large constant trees also store atom pointers. Lazy stubs keep parse facts and closure metadata. The realm owns the mutable execution state.

A portable unit holds codes with unit-relative child indexes and a table of owned string units. Each code’s atom table and constant trees hold indexes into that string table. On a hit, intern each distinct string into the realm’s source atom table and bind the code tables. Add the realm’s code base to each child index. Keep the source once and leave mutable execution state in the realm. The cost of a hit is linear in source bytes for the hash and comparison, plus the size of the compiled tables for the bind. The bind needs one transient vector of realm atoms. Each realm keeps its bound tables, and the cache keeps the portable tables.

The thread keeps up to 32 MiB of accounted entries and skips sources shorter than 512 bytes. A hash map uses source identity and compile mode as the key. A hit also compares the source bytes. An ordered index tracks recency and removes the oldest entry first for size or realm age. Lookup takes expected constant time before the source comparison, and an update takes logarithmic time in the number of entries. This change stores no files.

- An operand remap at runtime would keep more code shared, but a property or string operand can run many times for each bind. Binding fills the existing operand tables, so the hot loop can stay as it is.
- One atom arena per thread would avoid reinterning, but js2 never sweeps shared atoms. Live and dead documents would retain all of their interned names until the thread exits. It would still need per-realm child indexes and a portable disk format.
- A parse-tree cache would avoid repeated reads of the source and retain the tree. Every realm would still compile. The current lazy parser already drops most function bodies after it checks them.

V8 caches compiled code by source within an isolate and separates context-specific feedback. SpiderMonkey emits a GC-independent Stencil and instantiates it into runtime cells. [V8 code caching](https://v8.dev/blog/improved-code-caching), [SpiderMonkey frontend](https://firefox-source-docs.mozilla.org/js/)

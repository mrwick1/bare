# Vex, the JavaScript engine

Vex is the JavaScript engine of `bare`. It lives in `crates/vex` (`bare-vex`). It is the default engine and has no dependency on a browser: the browser reaches it through a host interface. Vex is written from scratch in Rust. The only third-party code in it is ICU4X data and a stack-growth helper (see [third-party.md](third-party.md)).

## Values

A value is 64 bits (NaN-boxing). A double is stored as it is. A 32-bit integer, `undefined`, `null`, a boolean and a hole each have a tag in the unused NaN space. A string, symbol, BigInt or object is a direct pointer to a cell in the heap. There is no slot table and no generation check.

## Heap and collector

- The heap is a set of size-class blocks. A block comes from the OS, is aligned to its size, and holds one mark bit for each cell. The mark bits need no side table.
- A small request pops a cell from the free list of one of 16 size classes. A large request gets a mapping of its own.
- The collector is non-moving and marks from the roots. It walks the roots where they live, with no copy into a list. A cell is marked before it is pushed, so graphs with cycles end.
- The sweep is lazy. A block is swept when its size class next needs cells. A block with no live cell goes to a small pool, and the rest go back to the OS.
- Weak tables and ephemerons (`WeakMap`) are handled in the same collection.
- The trigger counts bytes on the slow path of the allocator only. A collection can run between tasks, or inside a task with the frames as roots. The heap adjusts its limit after each collection from the retained growth.

Strings are heap cells with one-byte (Latin-1) and two-byte layouts, ropes for concatenation, and atoms for names. Source atoms are shared by all realms of a session. A realm keeps its run-time atoms in a weak table.

## Shapes and inline caches

An object holds a 32-bit shape ID in its header. The shape says which keys the object has, in which slot each value is, and which prototype it has. Objects made by the same literal share one shape. Shapes form a transition tree, and chains share descriptor tables. A property read at one site caches the shape and the slot. A read that finds its value on the prototype chain has its own site cache. The cache lives in a per-realm feedback vector, with one record for each site.

## Compiler and interpreter

- A pre-parse reads the whole script and reports every early error at load. The top level is compiled at once. A function inside it is a stub that is parsed and compiled at its first call, so code that never runs costs only its pre-parse.
- The compiler goes from the parse to register bytecode. Locals and arguments live in registers. A variable that an inner function captures lives in a small context object.
- One instruction is one 32-bit word with three 8-bit operands, and sometimes one more word. A verifier checks the code before the interpreter trusts it.
- The interpreter is one loop with one `match`. The hot state is in locals. One `Vec<Value>` holds the frames of all calls. Rare operations sit in cold arms, so the hot code stays small and stable. Vex has no JIT.
- A compile result can be cached in a portable form and bound to a new realm.

## Built-ins

Built-ins are built lazily. A table of methods, statics and constants records itself on the first use of an object, and a property is made at the first lookup that needs it. A realm pays for the properties that a script uses. Some built-ins are written in JavaScript (a prelude) and compile at their first call.

Vex has: `Object`, `Function`, `Array` and typed arrays, `ArrayBuffer`, `DataView`, `String`, `Number`, `Math`, `BigInt`, `Symbol`, `Date` with time zones, `JSON`, `Map`, `Set`, `WeakMap`, `WeakSet`, `Proxy`, `Reflect`, `Promise`, iterators, generators, `async` functions, async generators and `for await`, modules with `import()`, `TextEncoder` and `TextDecoder`, and `Intl` (`NumberFormat`, `Collator`, `PluralRules`, `Segmenter`, `DateTimeFormat`, `RelativeTimeFormat`, `ListFormat`, `DisplayNames`).

**Regular expressions.** A backtracking matcher compiles a pattern to a flat image that sits in one heap cell. It supports classes, groups, named groups, back-references, look-ahead, look-behind, the flags `d g i m s u v y`, and the web-compatibility rules for patterns without `u`. A pattern is validated once and its parse is reused.

**Promises and async.** A promise is a 32-byte cell with two hidden slots. A generator or async function suspends its frame into a heap object and resumes it later. Microtasks run in a queue that the host drains between tasks.

## Host interface

The engine knows nothing about the DOM. The host (`crates/bare`) gives it native functions and accessor properties. A host call gets its `this` and its arguments as handles that name the engine's own stack slots, with no copy. A handle scope maps onto the collector roots. A native that calls script holds the collector until the call returns.

## Conformance

On the test262 suite at pin `e4fbf57745c6aae3d1f12f181ecef78b1665d812`, Vex passes 29,217 tests, fails 1,619 and skips 8,537 (the skips are features the suite flags and Vex does not claim). The per-directory counts are in `tools/fixtures/js`. Source: [full cost matrix](internal/progress/2026-11-22-full-cost-matrix.md).

## Speed and memory

Numbers are in [performance.md](performance.md). The design notes of each part are in [internal](internal/README.md).

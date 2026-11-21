# 2026-11-21: Give js2 load examples distinct names

The four js2 examples now use the `js2_` prefix. Cargo can build both engines into one target directory. The lexer script selects the example by package. The cost matrix includes one row for each renamed binary.

On archMachine, one release build produced all eight binaries. The cost matrix ran one row per binary against the same build under glibc and jemalloc. It took the minimum instruction count of three runs per row and allocator. All eight rows ran. The two columns matched for every row. This check verifies binary lookup.

The `lex::` tests passed on Linux (31 per engine). The lexer script passed at scales 1, 10 and 100 with one run per case on each engine.

Not tested: Windows, the full cost matrix or performance against a separate build.

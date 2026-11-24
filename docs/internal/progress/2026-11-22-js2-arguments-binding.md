# Separate the parameter and body bindings for `arguments`

## Design

When a parameter has a default initializer or a computed key, the compiler puts a body function named `arguments` in an inner scope. It enters that scope after it initializes the parameters. A closure made by a parameter expression keeps access to the parameter scope. With no parameter expressions, the compiler uses the existing shared scope. [ECMAScript FunctionDeclarationInstantiation](https://tc39.es/ecma262/2025/multipage/ordinary-and-exotic-objects-behaviours.html#sec-functiondeclarationinstantiation)

## Tests

Four regression tests failed before the fix. After the fix, Linux passed 71 focused tests and `cargo check --locked -p bare-js2`. The Windows VM passed all six regression tests. The regression tests check runtime results and verify the bytecode of the script and each function.

Test262 compared the same 4,138 files on `74495429` and the branch. The pass count rose from 2,650 to 2,654. Failures fell from 181 to 177, and 1,307 tests were skipped in both runs. No passes were lost. New passes:

- `test/language/expressions/function/arguments-with-arguments-fn.js`
- `test/language/expressions/generators/arguments-with-arguments-fn.js`
- `test/language/statements/function/arguments-with-arguments-fn.js`
- `test/language/statements/generators/arguments-with-arguments-fn.js`

The four existing failures for a body lexical declaration named `arguments` remain.

## Cost

ArchMachine, AMD Ryzen 9 5900HX. The base is `74495429`. Locked release builds used the workspace's `codegen-units=1` for js2. Each row ran three times per build and allocator in a fresh process. Deltas compare minimum user-mode instructions. The table gives glibc ranges.

| `interp_load` row  |           Base instructions |         Branch instructions | glibc change | jemalloc change |
| ------------------ | --------------------------: | --------------------------: | -----------: | --------------: |
| `loopfn 3000000`   |     866,273,737–866,274,475 |     866,274,338–866,275,006 |     +0.0001% |        +0.0000% |
| `calls 1000000`    |     571,311,148–571,312,109 |     571,311,246–571,312,766 |     +0.0000% |        +0.0001% |
| `fib 27`           |     248,628,269–248,628,348 |     248,628,810–248,629,102 |     +0.0002% |        +0.0000% |
| `closures 1000000` | 1,942,608,779–1,942,609,484 | 1,942,609,514–1,942,610,130 |     +0.0000% |        −0.0000% |
| `objects 1000000`  |     744,159,633–744,159,923 |     744,159,687–744,160,845 |     +0.0000% |        +0.0000% |

The four js2 cost matrix rows passed under both allocators. The mean change was −0.006% with glibc and +0.000% with jemalloc. No row exceeded +0.5%.

Not tested: full workspace, full test262, browser journeys, warm-cache timing or peak RSS.

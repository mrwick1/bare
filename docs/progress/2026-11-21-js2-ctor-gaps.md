# js2 constructor gaps

Stacked on [async generators](2026-11-21-js2-async-generators.md) (#845). Three small gaps of the function kinds.

## What is built

- `new` on an arrow function, a method or an accessor that was never called now throws `TypeError`. The stub of a function that waits for its first call (`script.rs`, `stub`) had `noctor` only for generator and async functions. The compiled code had it for all kinds, so the error came only after the first call. `Reflect.construct` with such a function as `newTarget` throws too, because it reads the same flag.
- `GeneratorFunction`, `AsyncFunction` and `AsyncGeneratorFunction` are built. Each is the `constructor` of its prototype object (`%GeneratorFunction.prototype%` and the others), not writable, with `prototype` set to that object, `length` 1, and `Function` as its prototype. They are not globals. The realm makes each one when it makes the prototype object, which is at the first function of that kind (`Realm::dynamic_ctor`). A realm that has no such function pays nothing.
- The code is the code of `Function` (#838): `dynamic_function` in `builtins.rs` takes the keyword (`function`, `function*`, `async function`, `async function*`). The parameters and the body are checked alone as one declaration each, so a brace or a comment cannot reach across. The function has the global scope and the name `anonymous`.

## Not built

- `Function.prototype.toString` with the source text. It is not cheap. The source and the span of a function are in `Lazy` (`script.rs`), behind the `LazyCompile` trait that the realm sees. A read needs a new trait method (a trait change), a record of the function id for each stub (it is dropped at the first call), and the start of a method, an accessor and a class (the span starts at the `(`). A function that is not lazy (it has `super`) has no unit. Left for its own change.
- The prototype of the new function from `newTarget` (`GetPrototypeFromConstructor`). `Function` does not do it either. A subclass of `GeneratorFunction` gets the plain prototype.

## Tests

- `script_tests.rs`: `new` on an arrow function, a method, a getter, a setter and a static method before the first call, and `Reflect.construct` with an arrow function as `newTarget`; the properties of the three constructors; a generator built with arguments; a bad text (`yield` as a parameter of a generator, `await` as a parameter of an async function, `super()`, a brace that closes the function); the global scope.
- `asyncgen_tests.rs`: an async function and an async generator built by their constructors, run to the end.
- `cargo test --locked -p bare-js2 --lib` (724) and `cargo test --locked -p bare-js-api --features suite` pass. Test written first: the `new` test failed (`new (() => 1)()` gave no error) before the fix to the stub.

## test262

archMachine, same rules as the previous change. Base: the tree of #845.

| | base | now |
| --- | ---: | ---: |
| classic (`test262_js2.sh`), js2 pass | 26,047 | 26,068 |
| modules (`test262_modules new`), pass | 1,113 | 1,113 |

0 passes lost in both. 21 gained in classic: `built-ins/GeneratorFunction` (9), `built-ins/AsyncFunction` (6), `language/statements/class/subclass/builtin-objects/GeneratorFunction` (2), and one each in `language/expressions/object/method-definition`, `language/expressions/class`, `language/expressions/arrow-function` and `built-ins/Function/prototype/toString`. `tools/fixtures/js/test262-js2-baseline.txt` is written with `-u`.

## Cost

Same method as the previous change (release builds, `tools/js/instr.sh`, glibc and jemalloc, `LD_PRELOAD`). Base: the tree of #845. Changed code: the stub (every function at load), the realm setup of the three prototype objects (cold).

| Row | glibc change | jemalloc change | cgu=1 glibc | cgu=1 jemalloc |
| --- | ---: | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | -0.000% | +0.003% | +0.000% | +0.004% |
| `interp_load calls 1000000` | +0.000% | -0.000% | +0.000% | +0.000% |
| `js2_gen_load next 100000` | +0.001% | +0.001% | +0.001% | +0.001% |
| `js2_promise_load then 100000` | +0.991% | +0.969% | +0.000% | +0.000% |
| startup (`script_run`, `var x = 1;`) | -0.115% | -0.055% | -0.006% | -0.002% |

`js2_promise_load then` is over +0.5% in the default build (+1,496,590 instructions) and within it at `codegen-units=1` (+661). The rule allows it. The change does not touch the code of this row, so this is a layout effect (hypothesis; the same row moved the same way in A1 and in the previous change).

## Not tested

- The prototype from `newTarget` and the realm of another global (the test262 `proto-from-ctor-realm` tests still fail).
- `BARE_GC_STRESS=1`: js2 has no such switch. The new tests run under a constant collection trigger.
- Windows, wall time and memory.

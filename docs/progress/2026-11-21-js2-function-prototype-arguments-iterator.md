# js2: no `prototype` on some functions, `arguments[Symbol.iterator]`, tag of a host object

Three small spec bugs of the js2 core. Each has a test that failed before the fix.

## What is built

- **No own `prototype` for arrow functions, methods, accessors and async functions.** `typeof (() => 1).prototype` was `object`. `Realm::needs_prototype` (`builtins.rs`) made the `prototype` object at the first read for every script function that was not async. It now skips a function with `noctor` and no generator kind. That is an arrow function, a method, a getter, a setter and (with the old async rule) an async function. A generator method and an async generator method keep `prototype`, because they have a generator kind. Class constructors and native functions were already excluded. The test of the kind is one flag read in a cold path. Making a function costs the same as before. #846 fixed the no-constructor flag of the stub; this is the separate `prototype` path.
- **`arguments[Symbol.iterator]` is `%Array.prototype.values%`.** The shape of an arguments object (`Realm::s_args`, `interp_rt.rs`) now has the own property `Symbol.iterator` (writable, not enumerable, configurable) in slot 0. `Realm::new_args` (`lazy.rs`) makes the object and writes the slot from `Realm::args_iter`. The first call fills `args_iter` with `values` of `Array.prototype`, made from its lazy table. The realm keeps the function for `Array.prototype[Symbol.iterator]` (`Lazy::shared`), so a script that deleted or replaced `Array.prototype.values` before the first arguments object does not change the result. A realm with no compiler cannot make `values` (it is a prelude function): the property is `undefined` there. Both creation sites in `interp.rs` call `new_args`. js2 has no mapped arguments object, so one shape serves both kinds.
- **`Object.prototype.toString` on a host object gave `[object Date]`.** `F_DATE` (`bi_date.rs`) and `F_HOST` (`object.rs`) are the same header bit (1 << 19). `object_to_string` tested only the flag. It now calls `bi_date::is_date`, which is the test that the `Date` methods already use (the object has the time-value slot). A host object gives `[object Object]` unless it has `Symbol.toStringTag`.

## Not changed

- `String(document.createElement('div'))` giving `[object HTMLElement]` is not a js2 bug. js2 reads `Symbol.toStringTag` through the prototype chain like any property, so the tag comes from the host objects of the browser. hypothesis: the browser gives `HTMLElement.prototype` the tag and no tag to `HTMLDivElement.prototype`. Not checked in the browser here.
- `arguments.callee` does not exist in js2 (a sloppy function). Not part of this change.

## Tests

- `script_tests.rs`: `an_arrow_function_a_method_and_an_accessor_have_no_own_prototype` (the kinds, before and after the first call, and that a script can still add `prototype`), `the_arguments_object_has_the_array_values_iterator` (descriptor, strict and sloppy, spread, `for of`, own keys, `delete`). Both failed first.
- `api_host_tests.rs`: `a_host_object_is_not_a_date_for_object_prototype_to_string` (named and indexed host objects, a real `Date`, and `Date.prototype.getTime.call` on a host object). It failed first with `[object Date]`.
- Two old tests changed: `function_prototype_and_plain_function_prototypes` expected `typeof method.prototype` to be `object`; `a_lazy_compile_that_fails_throws_a_plain_error` now reads `[].values` before it breaks the compiler (the arguments object of the failing stub needs `values`).
- `cargo test --locked -p bare-js2 --lib` (745) and `cargo test --locked -p bare-js-api --features suite` pass.

## test262

archMachine, `tools/js/test262_js2.sh -u -j4`, test262 at `/data/claude-tmp/test262`. Base: origin/main 1303414b (the committed baseline file). 0 passes lost. 10 gained:

| Directory | base pass | now pass |
| --- | ---: | ---: |
| `built-ins/ArrayIteratorPrototype/next` | 5 | 13 |
| `language/arguments-object/mapped` | 21 | 22 |
| `language/arguments-object/unmapped` | 4 | 5 |

## Cost

archMachine. Release builds of `bare-js2` examples, base and new from separate target dirs. Count: `tools/js/instr.sh` (minimum of 3 runs of `perf stat -e instructions:u`), glibc, and `LD_PRELOAD` jemalloc. Base: origin/main 1303414b. Changed code: `CreateArgs` and the call path that makes an arguments object (`interp.rs`), the realm setup of one shape. The `interp_load` rows use hand-written bytecode: they do not make arguments objects, so they show the effect on the interpreter loop only. `script_run` of a small script (20,000 turns, one call with `arguments`, one plain call, an arrow function and a method call each) shows the arguments cost.

Default build (`codegen-units` 16):

| Row | base glibc | new glibc | change | change jemalloc |
| --- | ---: | ---: | ---: | ---: |
| `loopfn 1000000` | 292,299,293 | 296,299,069 | +1.368% | +1.353% |
| `calls 300000` | 177,405,829 | 180,405,972 | +1.691% | +1.660% |
| `fib 24` | 62,134,798 | 62,735,918 | +0.967% | +0.917% |
| `closures 300000` | 535,770,145 | 537,571,019 | +0.336% | +0.334% |
| `objects 300000` | 213,754,068 | 214,055,514 | +0.141% | +0.138% |
| `loopfn 0` (setup) | 798,547 | 798,548 | +0.000% | +0.003% |
| `closures 0` (setup) | 991,454 | 992,626 | +0.118% | +0.005% |

`loopfn`, `calls` and `fib` are over +0.5% under both allocators. The cause is code layout of the interpreter function (`loopfn` has no call and no arguments, and gains exactly 4 instructions a turn). With `RUSTFLAGS="-C codegen-units=1"` on both trees they are below the base:

| Row | glibc change | jemalloc change |
| --- | ---: | ---: |
| `loopfn 1000000` | -3.517% | -3.476% |
| `calls 300000` | -2.639% | -2.588% |
| `fib 24` | -1.380% | -1.307% |
| `closures 300000` | -1.322% | -1.314% |
| `objects 300000` | -0.810% | -0.797% |
| `loopfn 0` / `closures 0` (setup) | +0.025% / +0.066% | +0.012% / +0.008% |

So the rows pass under the standing rule (within +0.5% at `codegen-units=1`). The startup and the creation of a function are flat. The cost of the new property is the arguments object: 49 instructions more for each one (script_run, 20,000 and 40,000 turns, slope 49.5 and 49.3; base 47,068,330 and new 48,058,662 at 20,000 at `codegen-units=1`). A new property in the object is the price of the spec behavior. Making `new_args` `#[inline(never)]` or inline gave the same numbers.

## Not tested

- The browser (`BARE_JS_EXEC=new`). The `[object HTMLElement]` report is not checked there.
- Windows.
- The full cost matrix and the rows of `cost_rows.txt` (it has no js2 row; the rows above are the `interp_load` rows named in the task).
- `bare-js` (the old engine) and `crates/bare`: not touched.

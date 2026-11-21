# js2 Function constructor (phase 2 of the fast plan)

`Function(...)` and `new Function(...)` threw a `TypeError` in js2 ("there is no eval"). Some libraries use them, and a Reflect test needed them. They now compile the text with the script path of #821.

## What is built

- `function_ctor` in `crates/js2/src/builtins.rs` (the only code change). It follows CreateDynamicFunction for the normal kind:
  - It converts the arguments to text in order: the parameters first, the body last. A throw in a conversion goes out unchanged.
  - It parses two checks with `parse_script_lazy`: `function anonymous(P\n) {\n}` and `function anonymous(\n) {\nB\n}`. Each must parse as exactly one function declaration. So a comment, a brace or a `}` followed by more code in one part cannot reach into the other part or out of the function. Parse errors are `SyntaxError`.
  - It compiles `(function (P\n) {\nB\n})` with `Realm::script_code` (`ScriptMode { value: true, .. }`, not scoped, not eval), makes the function with `new_function` and gets it with `call_function` on the global object. The function has the global scope.
  - It names the function `anonymous` with `Realm::name_function`. The text has no function name, so the body has no binding for `anonymous`, as in the spec.
- No change to `Realm`, `script.rs`, `api_script.rs` or a trait. No `unsafe`.
- `run_code` (`a_call`) is not used. It crashed under constant collection when called from a native, because `a_call` goes to `call_held` and not to `call_function` when the API reports no nested call (hypothesis: the nest count is 0 in a native that a script called). `call_function` is the path that natives use (`reenter.rs`).

## Not built (listed)

- `GeneratorFunction`, `AsyncFunction` and `AsyncGeneratorFunction`: js2 has no such constructors and no `%GeneratorFunction.prototype%` object to hang them on. Generators and async functions exist, but only through source text.
- `toString` of a function made here: `Function.prototype.toString` returns `function () { [native code] }` for every function in js2, so it does not give `function anonymous(a,b\n) {\nreturn a\n}`. This is not changed here.
- A function made here has no `[[Realm]]` of its own and no `new.target` prototype lookup beyond what a script function has. Cross-realm behavior is not tested.

## Tests

| Command                                                          | Result                      |
| ---------------------------------------------------------------- | --------------------------- |
| `cargo test --offline -p bare-js2 --lib function_constructor`    | 1 test (12 scripts), passes |
| `cargo test --offline -p bare-js2 --lib`                         | 578 pass, 0 fail            |

The test `the_function_constructor_builds_a_global_function` runs each script normally and under constant collection. It covers the forms, parameter lists with a comma, defaults, `name` and `length`, the global scope, no binding for `anonymous`, the order of conversion, a throw in a conversion, the injection cases (`/*` and `*/){`, a body that closes the function and opens another, `}` then `var x = function () {`), a syntax error, a duplicate parameter in strict code, `this`, and `new` on the result.

## test262

`tools/js/test262_js2.sh`, archMachine. Compared with `test262-js2-baseline.txt`.

| Run                                       | Result                                                                          |
| ----------------------------------------- | ------------------------------------------------------------------------------- |
| `-g built-ins/Function`, js2 (pass/fail/skip) | 145 / 30 / 4 (baseline 73 / 101 / 5). The old engine: 145 / 30 / 4.        |
| whole js2 subset, 645 rows joined         | 0 passes lost in any row; 411 more passes than the baseline file (all rows)    |

The 411 count includes gains that came from PRs merged after the baseline file was written. Only the `built-ins/Function` rows are attributed to this change, and only those rows are updated in the baseline file. In `built-ins/Function`, 35 tests pass on the old engine and fail on js2. They are existing js2 gaps and the same before this change (bound functions as constructors, `Reflect.construct` of a bound function, `Date` before #835, `length` and `name` of `Function.prototype`, generator `toString`). They are not caused by this change.

## Cost

`cost_matrix.sh` was not run. The change is in `builtins.rs` only and adds no code to the interpreter loop, so the js2 micro rows were run (release, base = main 05a6acf1, min of 3 `perf stat instructions:u`, archMachine, one build each):

| Row                           |        base |         new |       change |
| ----------------------------- | ----------: | ----------: | -----------: |
| `script_run` (x.js)           |   2,081,929 |   2,084,330 |     +0.1153% |
| `interp_load loopfn 0`        |     999,607 |   1,002,050 |     +0.2444% |
| `interp_load loopfn 3000000`  | 848,500,549 | 848,502,874 |     +0.0003% |
| `interp_load calls 1000000`   | 585,557,525 | 585,559,659 |     +0.0004% |
| `interp_load fib 27`          | 257,769,370 | 257,771,576 |     +0.0009% |
| `interp_load closures 1000000`| 1,959,698,794 | 1,959,701,469 |   +0.0001% |
| `interp_load objects 1000000` | 711,385,891 | 711,387,841 |     +0.0003% |

Every row grew by 1,950 to 2,675 instructions, a flat amount. Hypothesis: it is the layout of the code, not work per run, because no row's loop changed. The two startup rows are under +0.5% as the rule says. Not measured: `codegen-units=1`, the other allocator, memory.

## Not tested

- `TZ` and other hosts, Windows: the change has no platform code.
- A function made by `Function` in a realm other than the one of the constructor.
- A very long body or a body at the parser limits.
- The old-engine differential for this feature (the old engine runs the same declaration path; the test262 counts are equal).

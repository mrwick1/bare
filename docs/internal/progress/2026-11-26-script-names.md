# Stack frames show the script URL (2026-11-26)

Base: main 244ff1cd.

## Problem

- Every stack frame said `<anonymous>:LINE:COL`. Vex had no name for a script.
- WhatsApp Web logs `TypeError: Cannot read properties of undefined (reading 'operation') at <anonymous>:437:4283` in a worker. The location did not match a downloaded bundle, so the cause was not found. With the URL in the frame, one live run gives the bundle and the line.

## Change

- js-api: new `Engine::run_script_named(source, name)` and `Scope::run_script_named`. The default calls `run_script`.
- Vex: each `Unit` (one load of a text) has `name`. The code cache shares the compiled codes of a text, but the unit is made for each load, so two URLs with the same text keep their own names. `LazyCompile` gets `set_name` and `name_of`. `frame_line` writes the name in place of `<anonymous>`. A module unit gets its URL from `module_parse`.
- bare: an external classic script gets its request URL. An inline script gets the document URL without the fragment. `importScripts` gets the URL of each script. A worker main script loads as an external script, so it gets its URL too. A `data:` script gets its URL.
- Not changed: eval code, `new Function` and event handler code stay `<anonymous>`. A blob script stays `<anonymous>`.

## Deviations from Chrome

- The line of an inline script counts from the script text. Chrome counts from the start of the HTML document.
- A redirected script shows the request URL. Chrome shows the final response URL.
- A frame of eval code is `at <anonymous>:L:C`. Chrome writes `at eval (eval at f (URL:L:C), <anonymous>:L:C)`.
- A blob script shows `<anonymous>`. Chrome shows the blob URL.

## Tests

- New file `crates/bare/src/browser_script_names_tests.rs`, 5 tests: an external script with a query, an inline script plus eval, `importScripts` in a worker, the same text from two URLs, and a module script. The first 3 failed before the change (`<anonymous>`). All 5 pass.
- `t`: 11 tests of the changed files pass. The Vex stack tests (`error_stack_tests.rs`, `error_stack_lines_tests.rs`, `api_error_stack_tests.rs`) pass.

## test262

`tools/js/test262_js2.sh -g '^test/(built-ins/(Error|NativeErrors|eval|Function)|language/eval-code)(/|$)'`, release builds of base and this change: 575 passed, 61 failed, 60 skipped on both. The per-test results are the same.

## Cost

archMachine, shared with other agents. `interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3 runs), user instructions.

| Row              | Allocator |          base |           new |   Change |
| ---------------- | --------- | ------------: | ------------: | -------: |
| loopfn 3000000   | glibc     |   784,501,266 |   784,500,732 | -0.0001% |
| loopfn 3000000   | jemalloc  |   784,561,102 |   784,561,542 | +0.0001% |
| calls 1000000    | glibc     |   550,501,482 |   550,501,410 | -0.0000% |
| calls 1000000    | jemalloc  |   550,529,975 |   550,530,673 | +0.0001% |
| fib 27           | glibc     |   240,503,286 |   240,503,521 | +0.0001% |
| fib 27           | jemalloc  |   240,565,699 |   240,565,081 | -0.0003% |
| closures 1000000 | glibc     | 1,750,723,590 | 1,750,723,738 | +0.0000% |
| closures 1000000 | jemalloc  | 1,750,746,497 | 1,750,746,862 | +0.0000% |
| objects 1000000  | glibc     |   697,789,601 |   697,789,212 | -0.0001% |
| objects 1000000  | jemalloc  |   697,855,158 |   697,854,528 | -0.0001% |

No row is over the rule. The name costs one `Rc<str>` for each loaded script. It is read only when a stack text is made.

## Not tested

- A live WhatsApp Web run.
- The Windows VM.
- Miri (no unsafe code changed).
- The full test262 subset and the full cost matrix.
- A shared worker and a service worker script.

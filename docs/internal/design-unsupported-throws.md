# A gate throws, and the page goes on

Owner decision, 2026-11-25: "Throw, page goes on". This note records the design.

## Current flow (before)

- A gate is a host code path for a feature that bare does not have. It returns `Fault::Unsupported(what)` (old form, `Dom::gate`, then `lift_err`) or `Abort::Unsupported(what)` (new form, `unsupp`).
- `eng_new::error` turns a `Fault::Unsupported` into `Error::Abort(Abort::Unsupported)` and saves the fault in `HOST_FAULT`. `Fault::Heap` also becomes `Abort::Unsupported`, with the heap fault saved.
- Vex does not let a script catch an abort. The abort unwinds to the Rust caller. `eng_new::fault` gives back the saved fault.
- `Browser::survive` calls every fault that is not a script budget fatal. The scheduler (`pump`), navigation, fetch and platform queues save it as `halted`. No script, timer or input runs again.
- The loader maps the fault to `Stop::Fatal(Fatal::Unsupported)`. `drive` diagnose prints "the page stopped" and the verdict "bare gap: the page stopped".

## New flow

- `Host::call` of `Dom` (`browser.rs`) is the one place where every host function returns to Vex. When the result is `Abort::Unsupported` and the saved fault is not a heap fault (`Ops::take_gate`), the host:
  1. clears the saved fault,
  2. writes a console error `bare: not supported: <what>`,
  3. throws a `NotSupportedError` DOMException (code 9) whose message is `<what>`.
- The script can catch the error. An uncaught one ends its task like any other uncaught exception: a report, the `error` event of `window`, and the page goes on.
- The console line keeps the gap visible. `drive` diagnose reads `error` lines that contain "not supported" as "missing feature", and its verdict is "bare gap: ...". This also works when the page catches the error.
- The cost on the hot path is one compare of the `Err` variant of the result. The conversion code is `#[cold]`.

## What does not change

- `Fault::Limit` and `Fault::Heap`: still aborts, still fatal (heap) or a task abort (budget).
- A gate that Rust code reaches with no script on the stack: the loader, the driver (`drive_type`, `focus`, `use_virtual_wall_clock_at`), render (`render_failed` waits for a missing sheet). These still return `Fault::Unsupported` to their Rust caller.
- An `Abort::Unsupported` that Vex raises in a script that a host function called also becomes a throw at that host boundary. This is in line with the decision.
- Vex does not change.

## Tests and tools that relied on the old behavior

- Many `*_tests.rs` files read a gate through `run_script` with a `show` helper that prints `unsupported:<what>`. A script gate is now an uncaught throw, so the helper prints `throw:NotSupportedError:<what>`. Rule for the bulk update: in the expected strings, replace `"unsupported:` with `"throw:NotSupportedError:`. A test that reads a gate from Rust (no script) keeps `Unsupported`. A test that matched `Err(Unsupported)` from `run_script` uses `is_gate` of `browser_unsupported_throws_tests.rs`.
- A test that expects a fatal stop from a script gate now expects that the page goes on.
- `browser_computed.rs` said that a gate stays page-level fatal. Its doc comment now points to this note.

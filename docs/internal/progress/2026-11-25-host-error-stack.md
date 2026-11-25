# Stack of errors that the host makes

- `Engine::new_error` (Vex `api.rs`) now records the frames of the script, as `error_ctor` does. A `DOMException` of a DOM call thus has a stack.
- Test: `api_error_stack_tests.rs`. It fails before the change (the stack was only `Error: from the host`) and passes after.
- `t`: pass.
- Use: a probe printed the stack of a DOM call in Teams with this change. That found the `ownerDocument` bug (#1057).
- Cost: the capture runs only when the host makes an error. That is a cold path. No hot path changes, so no cost rows were run.

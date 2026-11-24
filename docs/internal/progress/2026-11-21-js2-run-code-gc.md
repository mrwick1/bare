# 2026-11-21: js2 indirect eval under constant collection

Branch `t3/fix-js2-run-code-gc`. Base: origin/main 24c05b6f.

## Cause

`eval_native` (indirect eval) called `run_code`. `run_code` called `a_call`. `a_call` chose `call_function` only when `a_nested()` held (a host call or `nest > 0`). A native that the interpreter calls has `nest == 0` and no host window, so `a_call` took the other path, `call_held`. That path runs the code on a pooled machine and does not hold the collector. The new `Vm::exec` collected with its own stack as the only frame root. The stack of the script that called `eval` was out of the roots. The next collection of the outer run read a freed cell: `unreachable!` in `cell.rs:59` (`Layout::from_code`), reached from `Marker::cell`, `walk_frames`, `Vm::exec`. `Realm::collect` already has `debug_assert!(self.nest == 0, ...)` for this rule: a nested run holds the collector.

Evidence: the new test `an_indirect_eval_called_by_a_script_survives_constant_collection` panicked on origin/main with that backtrace, and passes with the fix.

## Fix

`run_code(ci, native)` and `eval_code(v, native)` take a flag. `eval_native` passes `true`, and `run_code` then calls `call_function` (nest, hold, saved state). The host entries (`run_script`, `compile_function`, `eval_global`, `run_text`) pass `false` and keep `a_call`, which already picks `call_function` inside a host call.

## Other callers checked

`a_run_script` refuses a nested call. `a_compile_function` and `a_eval_global` come from the host, where `a_call` sees the host window. `script_code` allocates only error objects, and an allocation never collects. `new_function` in `run_code` is a Rust local with no collection before the call roots it.

## Not tested

Miri (the fix touches no unsafe code), test262 (the js2 subset leaves out `eval`; the change does not alter what eval computes), a native other than eval that runs script (the Function constructor of #838 uses `call_function`).

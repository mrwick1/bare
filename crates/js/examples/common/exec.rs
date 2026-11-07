//! The engine of an example: `BARE_JS_EXEC=tree|auto|byte` (`Interp::set_exec`). The library reads no environment variable, so
//! every example that makes an interpreter calls `set` once (or puts `mode` in `EvalLimits::exec`). Include it with
//! `#[path = "common/exec.rs"] mod exec;`.
#![allow(dead_code)]
use bare_js::eval::{Exec, Interp};

/// The mode of `BARE_JS_EXEC` (`tree` when it is not set). An unknown value stops the example.
pub fn mode() -> Exec {
    match std::env::var("BARE_JS_EXEC") {
        Err(_) => Exec::Tree,
        Ok(v) => Exec::from_name(&v).unwrap_or_else(|| {
            eprintln!("BARE_JS_EXEC: expected tree, auto or byte, got {v:?}");
            std::process::exit(2)
        }),
    }
}

/// Set the mode of `BARE_JS_EXEC` on `it`.
pub fn set(it: &mut Interp<'_>) {
    it.set_exec(mode());
}

//! The new JavaScript engine of bare (`docs/design-engine-rewrite.md`). Stage S1 builds values, the heap and objects here; stage S2
//! adds the compiler and the interpreter. Until then it runs no scripts, and `BARE_JS_EXEC=new` stops with [`NOT_READY`].
//!
//! Only the `value`, `heap`, `string` and `atoms` modules may hold `unsafe` code. CI runs this crate under Miri.
#![deny(unsafe_code)]

/// The error of `BARE_JS_EXEC=new` until the new engine runs scripts (plan stage S2).
pub const NOT_READY: &str = "the new JavaScript engine (bare-js2) cannot run scripts yet; use BARE_JS_EXEC=tree, auto or byte";


pub mod value;
#[allow(unsafe_code)]
pub mod cell;
#[allow(unsafe_code)]
pub mod heap;
#[allow(unsafe_code)]
pub mod string;
#[allow(unsafe_code)]
pub mod atoms;

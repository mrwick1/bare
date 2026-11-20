//! The new JavaScript engine of bare (`docs/design-engine-rewrite.md`). Stage S1 builds values, the heap and objects here; stage S2
//! adds the compiler and the interpreter. Until then it runs no scripts, and `BARE_JS_EXEC=new` stops with [`NOT_READY`].
//!
//! Only the `cell`, `heap`, `string`, `atoms`, `shapes`, `object` and `gc` modules may hold `unsafe` code. CI runs this crate under Miri.
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
#[allow(unsafe_code)]
pub mod shapes;
#[allow(unsafe_code)]
pub mod gc;
#[allow(unsafe_code)]
pub mod object;
pub mod roots;
pub mod lex;
pub mod ast;
pub mod facts;
pub mod parse;
pub mod bytecode;
pub mod verify;
pub mod compile;

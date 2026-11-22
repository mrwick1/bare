//! The new JavaScript engine of bare (`docs/design-engine-rewrite.md`). Stage S1 builds values, the heap and objects here; stage S2
//! adds the compiler and the interpreter. [`script::run`] runs a script end to end with the core built-ins (`builtins.rs`, S2.7), and the engine
//! is not wired into the browser: `BARE_JS_EXEC=new` still stops with [`NOT_READY`].
//!
//! Only the `cell`, `heap`, `string`, `atoms`, `shapes`, `object`, `blob`, `weaktab`, `gc`, `codes`, `interp_rt`, `interp`, `api` and `bi_buffer` modules may hold `unsafe` code. CI runs this crate under Miri.
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
pub mod blob;
#[allow(unsafe_code)]
pub mod gc;
#[allow(unsafe_code)]
pub mod object;
#[allow(unsafe_code)]
pub mod codes;
#[allow(unsafe_code)]
pub mod weaktab;
pub mod roots;
pub mod parse_ahead;
pub mod lex;
pub mod regexp;
mod regexp_rt;
pub mod regexp_check;
pub mod ast;
pub mod facts;
pub mod parse;
pub mod bytecode;
pub mod verify;
pub mod compile;
mod data_lit;
#[allow(unsafe_code)]
pub mod interp_rt;
#[allow(unsafe_code)]
pub mod interp;
#[allow(unsafe_code)]
pub mod api;
#[allow(unsafe_code)]
pub mod api_host;
pub mod api_raw;
pub mod interp_ops;
#[cfg(feature = "op-count")]
pub mod opcount;
pub mod builtins;
pub mod reenter;
pub mod generator;
mod asyncgen;
mod iter;
mod bi_array;
mod bi_bigint;
mod bigint;
#[allow(unsafe_code)]
mod bi_buffer;
mod bi_typed;
mod bi_json;
mod bi_date;
mod bi_date_calc;
mod bi_date_tz;
mod bi_object;
mod bi_number;
mod bi_wrap;
mod bi_string;
pub mod promise;
mod module;
mod promise_statics;
mod bi_symbol;
mod bi_collections;
mod bi_reflect;
mod bi_string_more;
mod bi_string_regexp;
mod bi_uri;
mod bi_string_iter;
mod lazy;
mod prelude_facts {
    include!(concat!(env!("OUT_DIR"), "/prelude_facts.rs"));
}
mod prelude_pre;
pub mod script;
#[cfg(test)]
#[path = "prelude_facts_tests.rs"]
mod prelude_facts_tests;
#[cfg(test)]
#[path = "builtins_more_tests.rs"]
mod builtins_more_tests;

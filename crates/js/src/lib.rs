//! JavaScript syntax, managed values, JSON, regexp matching and a bounded evaluator subset.

pub mod ast;
pub mod lex;
pub mod parse;
pub mod value;
pub mod date;
pub mod tz;
pub mod intl;
mod intl_data;
mod intl_num_data;
mod icu;
pub mod heap;
pub mod json;
pub mod regexp;
pub mod text;

pub mod eval;

mod number;
mod bigint;
mod ieee754;

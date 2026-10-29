//! CSS: turn style sheets into rules, and rules into the style of each element.

pub mod at_rules;
pub mod calc;
pub mod cascade;
pub mod color;
mod colors;
pub mod matching;
pub mod parser;
pub mod selector;
pub mod tokenizer;
pub mod values;
pub mod var;
// `pub mod` makes the module visible to other crates: `bare_css::tokenizer`.

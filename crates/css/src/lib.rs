//! CSS: turn style sheets into rules, and rules into the style of each element.

pub mod at_rules;
pub mod calc;
pub mod cascade;
pub mod color;
mod colors;
pub mod modern_color;
pub mod form;
pub mod grid;
pub mod hints;
pub mod matching;
pub mod motion;
pub mod parser;
pub mod rule_cache;
mod scan;
pub mod selector;
#[cfg(test)]
mod shadow_tests;
pub mod tokenizer;
pub mod urls;
pub mod values;
pub mod var;
// `pub mod` makes the module visible to other crates: `bare_css::tokenizer`.

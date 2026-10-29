//! HTML: turn the bytes of a page into a tree of elements (the DOM).

pub mod charref;
// `pub mod` makes the module visible to other crates: `bare_html::charref`.
pub mod dom;
pub mod encoding;
mod entities;
// No `pub`: only this crate uses the table. Other crates use `charref`.
pub mod tokenizer;

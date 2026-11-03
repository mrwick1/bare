//! Layout: find the place and the size of every box on the page, from the DOM and the computed styles.

pub mod boxes;
// `pub mod` makes the module visible to other crates: `bare_layout::boxes`.

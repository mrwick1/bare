//! Networking: turn a URL into the bytes of a page.

pub mod dns;
// `pub mod` makes the module visible to other crates: `bare_net::dns`.
pub mod http;
pub mod tcp;

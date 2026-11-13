//! Networking: turn a URL into the bytes of a page.

pub mod cause;
pub mod cookie;
pub mod dns;
mod encoding;
// `pub mod` makes the module visible to other crates: `bare_net::dns`.
mod h2;
mod hpack;
pub mod http;
pub mod parallel;
pub mod replay;
pub mod tcp;
pub mod trace;
pub mod vclock;

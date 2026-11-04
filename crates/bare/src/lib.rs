//! The browser app as a library, so that its parts can be tested. `main.rs` is the command line on top.
//!
//! `browser` joins the DOM (`bare_html::dom::Document`) to the JavaScript evaluator (`bare_js`): `window`,
//! `document`, nodes, events. Read its header for what works and what is gated.

pub mod browser;
mod browser_dom;
mod browser_events;
#[cfg(test)]
mod browser_tests;

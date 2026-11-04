//! The browser app as a library, so that its parts can be tested. `main.rs` is the command line on top.
//!
//! `browser` joins the DOM (`bare_html::dom::Document`) to the JavaScript evaluator (`bare_js`): `window`,
//! `document`, nodes, events. Read its header for what works and what is gated.

pub mod browser;
mod browser_dom;
mod browser_events;
pub mod browser_fetch;
#[cfg(test)]
mod browser_fetch_tests;
#[cfg(test)]
mod browser_tests;
mod browser_scheduler;
#[cfg(test)]
mod browser_scheduler_tests;

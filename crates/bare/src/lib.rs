//! The browser app as a library, so that its parts can be tested. `main.rs` is the command line on top.
//!
//! `browser` joins the DOM (`bare_html::dom::Document`) to the JavaScript evaluator (`bare_js`): `window`,
//! `document`, nodes, events. Read its header for what works and what is gated.

pub mod browser;
mod browser_dom;
mod browser_events;
mod browser_focus;
#[cfg(test)]
mod browser_focus_tests;
mod browser_namespace;
mod browser_attr;
#[cfg(test)]
mod browser_attr_tests;
pub mod browser_fetch;
pub mod browser_console;
#[cfg(test)]
mod browser_console_tests;
pub mod browser_streams;
#[cfg(test)]
mod browser_streams_tests;
#[cfg(test)]
mod browser_fetch_tests;
pub mod browser_navigation;
mod browser_navigation_params;
mod browser_navigation_state;
mod browser_navigation_storage;
mod browser_navigation_url;
#[cfg(test)]
mod browser_navigation_tests;
#[cfg(test)]
mod browser_namespace_tests;
#[cfg(test)]
mod browser_iframe_tests;
#[cfg(test)]
mod browser_link_tests;
#[cfg(test)]
mod browser_root_node_tests;
#[cfg(test)]
mod browser_tests;
pub mod browser_platform;
mod browser_platform_class;
mod browser_platform_query;
mod browser_scheduler;
mod browser_selection;
#[cfg(test)]
mod browser_selection_tests;
#[cfg(test)]
mod browser_scheduler_tests;
#[cfg(test)]
mod browser_date_tests;
pub mod resources;
mod browser_loader;
pub use browser_loader::{Fatal, Input, NavStop, PageConfig, PageLimits, PageResult, Stop};
pub mod browser_session;
#[cfg(test)]
mod browser_session_tests;
#[cfg(test)]
mod browser_loader_tests;
#[cfg(test)]
mod browser_platform_tests;
mod browser_style;
mod browser_style_block;
#[cfg(test)]
mod browser_style_tests;
#[cfg(test)]
mod browser_resume_tests;

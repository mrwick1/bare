//! The browser app as a library, so that its parts can be tested. `main.rs` is the command line on top.
//!
//! `browser` joins the DOM (`bare_html::dom::Document`) to the JavaScript evaluator (`bare_js`): `window`,
//! `document`, nodes, events. Read its header for what works and what is gated.

pub mod browser;
mod browser_dom;
mod browser_fragment;
mod browser_shadow;
mod browser_events;
mod browser_error_event;
mod browser_handlers;
#[cfg(test)]
mod browser_handler_tests;
mod browser_focus;
mod browser_form;
mod browser_submit;
#[cfg(test)]
mod browser_submit_tests;
pub mod browser_ui;
mod browser_drive;
mod browser_point;
mod browser_uievent;
pub mod page;
mod images;
pub mod control;
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod browser_ui_tests;
mod browser_scroll;
#[cfg(test)]
mod browser_scroll_tests;
#[cfg(test)]
mod browser_form_tests;
#[cfg(test)]
mod browser_focus_tests;
mod browser_namespace;
mod browser_attr;
mod browser_nodecmp;
mod browser_nodelist;
mod browser_select;
#[cfg(test)]
mod browser_select_tests;
mod browser_controls;
mod browser_textarea;
#[cfg(test)]
mod browser_controls_tests;
#[cfg(test)]
mod browser_idtab_tests;
mod browser_dataset;
mod browser_abort;
mod browser_xhr;
mod browser_encoding;
mod browser_web;
#[cfg(test)]
mod browser_web_tests;
#[cfg(test)]
mod browser_attr_tests;
#[cfg(test)]
mod browser_nodelist_tests;
#[cfg(test)]
mod browser_dataset_tests;
#[cfg(test)]
mod browser_abort_tests;
pub mod browser_fetch;
pub mod browser_console;
#[cfg(test)]
mod browser_console_tests;
pub mod browser_streams;
#[cfg(test)]
mod browser_streams_tests;
#[cfg(test)]
mod browser_fetch_tests;
#[cfg(test)]
mod browser_xhr_tests;
#[cfg(test)]
mod browser_cors_tests;
pub mod browser_navigation;
mod browser_navigation_params;
mod browser_navigation_state;
mod browser_cookie;
mod browser_navigation_storage;
mod browser_navigation_url;
#[cfg(test)]
mod browser_navigation_tests;
#[cfg(test)]
mod browser_navigation_subclass_tests;
#[cfg(test)]
mod browser_namespace_tests;
#[cfg(test)]
mod browser_iframe_tests;
#[cfg(test)]
mod browser_link_tests;
#[cfg(test)]
mod browser_root_node_tests;
#[cfg(test)]
mod browser_shadow_tests;
#[cfg(test)]
mod browser_fragment_tests;
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
pub use browser_loader::{CssStats, Fatal, Input, Intercept, NavStop, PageConfig, PageLimits, PageResult, Stop};
pub mod browser_session;
#[cfg(test)]
mod browser_session_tests;
#[cfg(test)]
mod browser_loader_tests;
#[cfg(test)]
mod browser_platform_tests;
mod browser_style;
mod browser_style_block;
pub mod browser_computed;
mod browser_used;
pub mod browser_geometry;
pub mod browser_resize;
mod browser_element;
mod browser_frame;
mod browser_idle;
mod browser_intersection;
mod browser_walker;
mod browser_range;
mod browser_rects;
mod browser_chardata;
mod browser_lazy;
mod browser_implementation;
mod browser_perf_observer;
mod browser_mutation;
mod browser_custom;
mod browser_face;
mod browser_geom;
mod browser_matrix;
#[cfg(test)]
mod browser_custom_tests;
#[cfg(test)]
mod browser_topic_tests;
#[cfg(test)]
mod browser_drive_tests;
#[cfg(test)]
mod browser_point_tests;
#[cfg(test)]
mod browser_uievent_tests;
#[cfg(test)]
mod browser_raf_tests;
#[cfg(test)]
mod browser_idle_tests;
#[cfg(test)]
mod browser_element_tests;
mod browser_elements;
mod browser_slot;
mod browser_media;
mod browser_img;
#[cfg(test)]
mod browser_img_tests;
#[cfg(test)]
mod browser_media_tests;
#[cfg(test)]
mod browser_elements_tests;
#[cfg(test)]
mod browser_mutation_tests;
#[cfg(test)]
mod browser_intersection_tests;
#[cfg(test)]
mod browser_sticky_tests;
#[cfg(test)]
mod browser_baseline_tests;
#[cfg(test)]
mod browser_valign_tests;
#[cfg(test)]
mod browser_aspect_tests;
#[cfg(test)]
mod browser_cssfix_tests;
#[cfg(test)]
mod browser_storage_tests;
#[cfg(test)]
mod browser_cookie_tests;
#[cfg(test)]
mod browser_cookie_http_tests;
#[cfg(test)]
mod browser_svg_tests;
#[cfg(test)]
mod browser_walker_tests;
#[cfg(test)]
mod browser_range_tests;
#[cfg(test)]
mod browser_resize_tests;
#[cfg(test)]
mod browser_geometry_tests;
#[cfg(test)]
mod browser_geom_tests;
#[cfg(test)]
mod browser_computed_tests;
#[cfg(test)]
mod browser_style_tests;
#[cfg(test)]
mod browser_resume_tests;
#[cfg(test)]
mod browser_survive_tests;
#[cfg(test)]
mod browser_settle_tests;

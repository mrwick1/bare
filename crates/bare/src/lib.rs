//! The browser app as a library, so that its parts can be tested. `main.rs` is the command line on top.
//!
//! `browser` joins the DOM (`bare_html::dom::Document`) to the JavaScript engine (`bare_vex`): `window`,
//! `document`, nodes, events. Read its header for what works and what is gated.

#![allow(type_alias_bounds)]

pub mod browser;
pub mod eng;
pub mod browser_any;
pub mod eng_new;
pub mod js_limits;
pub mod js_num;
mod browser_dom;
mod browser_dom_old;
mod browser_fragment;
mod browser_idb;
pub mod profile;
#[cfg(test)]
mod profile_tests;
mod profile_record;
mod browser_shadow;
mod browser_events;
mod browser_error_event;
mod browser_rejection;
#[cfg(test)]
mod browser_rejection_tests;
mod browser_handlers;
#[cfg(test)]
mod browser_handler_tests;
#[cfg(test)]
mod browser_listener_per_target_tests;
mod browser_focus;
mod browser_form;
mod browser_details;
mod browser_submit;
#[cfg(test)]
mod browser_submit_tests;
pub mod browser_ui;
mod browser_drive;
mod browser_point;
mod browser_uievent;
mod browser_mouse_legacy;
pub mod agent;
pub mod page;
mod images;
pub mod control;
pub mod force;
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod page_live_events_tests;
#[cfg(test)]
mod page_live_iframe_tests;
#[cfg(test)]
mod paint_trace_tests;
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
mod browser_sources;
mod browser_abort;
mod browser_editing;
mod browser_xhr;
mod browser_encoding;
mod browser_web;
mod browser_subtle;
#[cfg(test)]
mod browser_subtle_tests;
#[cfg(test)]
mod browser_subtle_hmac_hash_tests;
#[cfg(test)]
mod browser_subtle_ctr_hmac_tests;
#[cfg(test)]
mod browser_web_tests;
#[cfg(test)]
mod browser_web_filelist_tests;
#[cfg(test)]
mod browser_attr_tests;
#[cfg(test)]
mod browser_nodelist_tests;
#[cfg(test)]
mod browser_dataset_tests;
#[cfg(test)]
mod browser_abort_tests;
#[cfg(test)]
mod browser_editing_tests;
pub mod browser_fetch;
pub mod browser_console;
#[cfg(test)]
mod browser_console_tests;
mod browser_css_ns;
#[cfg(test)]
mod browser_css_ns_tests;
pub mod browser_streams;
#[cfg(test)]
mod browser_streams_tests;
#[cfg(test)]
mod browser_streams_shim_tests;
#[cfg(test)]
mod browser_compression_tests;
#[cfg(test)]
mod browser_fetch_tests;
#[cfg(test)]
mod browser_xhr_tests;
#[cfg(test)]
mod browser_eventsource_tests;
#[cfg(test)]
mod browser_websocket_tests;
#[cfg(test)]
mod browser_websocket_net_tests;
#[cfg(test)]
mod browser_websocket_deflate_tests;
#[cfg(test)]
mod browser_speech_tests;
#[cfg(test)]
mod browser_filereader_tests;
#[cfg(test)]
mod browser_cors_tests;
#[cfg(test)]
mod browser_fetch_origin_tests;
#[cfg(test)]
mod browser_referer_tests;
#[cfg(test)]
mod browser_fetch_integrity_tests;
pub mod browser_navigation;
mod browser_navigation_params;
mod browser_navigation_state;
mod browser_cookie;
mod browser_refresh;
mod browser_navigation_storage;
mod browser_navigation_url;
#[cfg(test)]
mod browser_navigation_url_colon_tests;
#[cfg(test)]
mod browser_navigation_tests;
#[cfg(test)]
mod browser_clone_tests;
#[cfg(test)]
mod browser_history_state_cap_tests;
#[cfg(test)]
mod browser_navigation_subclass_tests;
#[cfg(test)]
mod browser_namespace_tests;
#[cfg(test)]
mod browser_iframe_tests;
#[cfg(test)]
mod browser_named_tests;
#[cfg(test)]
mod browser_heap_stop_tests;
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
#[cfg(test)]
mod browser_new_tests;
#[cfg(test)]
mod browser_events_gc_tests;
pub mod browser_platform;
mod browser_platform_class;
mod browser_platform_query;
mod browser_scheduler;
mod browser_worker;
mod browser_wasm;
#[cfg(test)]
mod browser_wasm_tests;
#[cfg(test)]
mod browser_wasm_memory_tests;
#[cfg(test)]
mod browser_wasm_clone_tests;
#[cfg(test)]
mod browser_web_navigator_tests;
#[cfg(test)]
mod resources_fetch_metadata_tests;
mod browser_broadcast;
mod browser_perf;
mod browser_html;
mod browser_html_members;
mod browser_reflect;
mod browser_table;
mod browser_selection;
#[cfg(test)]
mod browser_selection_tests;
#[cfg(test)]
mod browser_scheduler_tests;
#[cfg(test)]
mod browser_worker_tests;
#[cfg(test)]
mod browser_date_tests;
#[cfg(test)]
mod browser_web_api_tests;
#[cfg(test)]
mod browser_gaps_tests;
#[cfg(test)]
mod browser_query_item_tests;
#[cfg(test)]
mod browser_style_media_tests;
#[cfg(test)]
mod browser_owner_document_tests;
#[cfg(test)]
mod browser_caches_tests;
#[cfg(test)]
mod browser_sw_tests;
#[cfg(test)]
mod browser_loader_queue_tests;
#[cfg(test)]
mod browser_loader_data_tests;
#[cfg(test)]
mod browser_link_sheet_tests;
#[cfg(test)]
mod browser_perf_detail_tests;
#[cfg(test)]
mod browser_clone_host_tests;
#[cfg(test)]
mod browser_idb_tx_tests;
#[cfg(test)]
mod browser_idb_binary_key_tests;
#[cfg(test)]
mod browser_events_count_tests;
#[cfg(test)]
mod browser_idb_tx_order_tests;
#[cfg(test)]
mod browser_idb_cursor_tests;
#[cfg(test)]
mod browser_idb_store_tests;
#[cfg(test)]
mod profile_record_tests;
#[cfg(test)]
mod browser_idb_many_requests_tests;
#[cfg(test)]
mod browser_idb_reread_tests;
#[cfg(test)]
mod browser_idb_log_tests;
#[cfg(test)]
mod browser_idb_worker_tests;
pub mod resources;
mod browser_loader;
mod browser_shims;
mod browser_websocket;
mod browser_gaps;
pub use browser_loader::{CssStats, Fatal, Input, Intercept, NavStop, PageConfig, PageLimits, PageResult, Stop};
pub mod browser_session;
pub mod downloads;
pub use downloads::{Download, DownloadState, Downloads};
#[cfg(test)]
mod browser_any_stats_tests;
#[cfg(test)]
mod browser_session_tests;
#[cfg(test)]
mod browser_stream_tests;
#[cfg(test)]
mod browser_loader_tests;
#[cfg(test)]
mod browser_loader_trackers_tests;
#[cfg(test)]
mod browser_current_script_tests;
#[cfg(test)]
mod browser_canvas_tests;
#[cfg(test)]
mod browser_canvas_size_tests;
#[cfg(test)]
mod browser_canvas_raster_tests;
#[cfg(test)]
mod browser_platform_tests;
mod browser_style;
mod browser_style_block;
pub mod browser_computed;
mod browser_used;
pub mod browser_geometry;
pub mod browser_motion;
pub mod browser_resize;
mod browser_element;
mod browser_frame;
mod browser_idle;
mod browser_intersection;
mod browser_walker;
mod browser_range;
mod browser_reclaim;
mod browser_rects;
mod browser_chardata;
mod browser_lazy;
mod browser_implementation;
mod browser_perf_observer;
mod browser_mutation;
mod browser_custom;
mod browser_scoped;
#[cfg(test)]
mod browser_scoped_tests;
mod browser_face;
mod browser_geom;
mod browser_basics;
mod browser_fontface; mod browser_fonts;
#[cfg(test)] mod browser_fonts_tests;
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
mod browser_mouse_legacy_tests;
#[cfg(test)]
mod browser_raf_tests;
#[cfg(test)]
mod browser_idle_tests;
#[cfg(test)]
mod browser_element_tests;
#[cfg(test)]
mod browser_element_nonce_tests;
mod browser_elements;
mod browser_canvas;
mod browser_svg;
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
mod browser_owners_tests;
#[cfg(test)]
#[cfg(test)]
mod browser_session_vex_default_tests;
#[cfg(test)]
mod browser_reclaim_tests;
#[cfg(test)]
mod browser_reclaim_custom_tests;
#[cfg(test)]
mod browser_dom_text_bound_tests;
#[cfg(test)]
mod browser_resize_tests;
#[cfg(test)]
mod browser_geometry_tests;
#[cfg(test)]
mod browser_geom_tests;
#[cfg(test)]
mod browser_computed_tests;
#[cfg(test)]
mod browser_computed_chain_tests;
#[cfg(test)]
mod browser_computed_pseudo_tests;
#[cfg(test)]
mod browser_generated_tests;
#[cfg(test)]
mod browser_rule_share_tests;
#[cfg(test)]
mod browser_style_tests;
#[cfg(test)]
mod browser_style_svg_tests;
#[cfg(test)]
mod browser_style_effects_tests;
#[cfg(test)]
mod browser_resume_tests;
#[cfg(test)]
mod browser_survive_tests;
#[cfg(test)]
mod browser_survive_vex_tests;
#[cfg(test)]
mod browser_settle_tests;

mod browser_cssom;
#[cfg(test)]
mod browser_cssom_tests;
#[cfg(test)]
mod browser_cssom_owned_tests;
#[cfg(test)]
mod browser_domparser_move_tests;
#[cfg(test)]
mod browser_domparser_xhtml_tests;
#[cfg(test)]
mod browser_create_document_tests;

mod browser_frames;
mod browser_child;
#[cfg(test)]
mod browser_child_tests;
#[cfg(test)]
mod browser_broadcast_pages_tests;
#[cfg(test)]
mod browser_message_queue_gc_tests;
#[cfg(test)]
mod browser_worker_run_tests;
#[cfg(test)]
mod browser_worker_scope_tests;
#[cfg(test)]
mod browser_worker_import_tests;
#[cfg(test)]
mod browser_worker_gc_tests;
mod browser_remote_window;
mod browser_port_hub;

#[cfg(test)]
mod browser_steps_tests;

#[cfg(test)]
mod browser_basics_tests;
#[cfg(test)]
mod browser_window_close_tests;
#[cfg(test)]
mod browser_broadcast_tests;

#[cfg(test)] mod browser_domexception_tests;

#[cfg(test)]
mod browser_rellist_tests;
#[cfg(test)]
mod browser_sandbox_tests;
#[cfg(test)]
mod browser_transition_tests;

#[cfg(test)]
mod browser_shared_wait_tests;

#[cfg(test)]
mod browser_threaded_worker_tests;

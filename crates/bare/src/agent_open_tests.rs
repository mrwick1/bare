//! Tests of the tabs that `window.open` asks a driver for (`agent.rs`).

use crate::agent::{Event, set_policy, take};
use crate::browser::*;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn opened(js: &str) -> Vec<Event> {
    std::thread::Builder::new().stack_size(64 << 20).spawn({
        let js = js.to_string();
        move || {
            let mut b = Browser::new(tree::parse("<p>opener</p>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
            b.set_document_url("https://example.com/page").unwrap();
            set_policy(true, None);
            b.run_script(&js).expect("script");
            take()
        }
    }).unwrap().join().unwrap()
}

#[test]
fn window_open_with_no_url_or_an_empty_url_opens_no_tab_at_the_opener_url() {
    assert_eq!(opened("window.open(''); window.open()"), vec![]);
}

#[test]
fn window_open_with_a_relative_url_opens_a_tab_at_the_resolved_url() {
    assert_eq!(opened("window.open('/next')"), vec![Event::Tab("https://example.com/next".into())]);
}

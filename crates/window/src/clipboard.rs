//! The plain rules of the clipboard: which MIME type to read, which to offer, and the state of a read. No Wayland here, so a
//! test can check them with no compositor (`clipboard_tests.rs`).

use std::collections::VecDeque;
use std::sync::Arc;

/// The text types, best first. The first one that the source offers is read.
pub const TEXT: &[&str] = &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"];
/// The image types that a page can use, best first.
pub const IMAGE: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// The first type of `want` that `offered` has.
pub fn pick(offered: &[String], want: &[&str]) -> Option<String> {
    want.iter().find(|w| offered.iter().any(|o| o == *w)).map(|w| w.to_string())
}

/// What we offer for the data of the app: `text/plain` is offered under all the text names, the other types as they are.
pub fn offer_for(data: &[(String, Arc<[u8]>)]) -> Vec<(String, Arc<[u8]>)> {
    let mut out = Vec::new();
    for (mime, bytes) in data {
        if mime == "text/plain" {
            out.extend(TEXT.iter().map(|m| (m.to_string(), bytes.clone())));
        } else {
            out.push((mime.clone(), bytes.clone()));
        }
    }
    out
}

/// A read of the clipboard: one type for each group, read one after the other.
pub struct ReadJob {
    queue: VecDeque<String>,
    items: Vec<(String, Vec<u8>)>,
}

impl ReadJob {
    pub fn new(offered: &[String], groups: &[&[&str]]) -> ReadJob {
        ReadJob { queue: groups.iter().filter_map(|g| pick(offered, g)).collect(), items: Vec::new() }
    }

    /// The next type to read, or `None` when all are read.
    pub fn next(&mut self) -> Option<String> {
        self.queue.pop_front()
    }

    /// Keep the data of a type. A source that gave nothing leaves no item.
    pub fn push(&mut self, mime: &str, data: Vec<u8>) {
        if !data.is_empty() {
            self.items.push((mime.to_string(), data));
        }
    }

    pub fn items(self) -> Vec<(String, Vec<u8>)> {
        self.items
    }
}

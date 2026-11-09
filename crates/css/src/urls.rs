//! The addresses that `url()` names in a style sheet.
//!
//! A value is 8 bytes and `Copy`, so it cannot hold text. `background-image` and `mask-image` hold a number that stands for the
//! address (`Value::Number`). `intern` gives the number and `get` gives the text back. One address has one number for the whole
//! process, so two sheets that name the same file share it, and the number is the same in every style. The first number is 1.
//! A sheet that names too many different addresses, or one that is very long, gets 0 from `intern`: no image.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Different addresses at most. A page rarely has 1,000.
const MAX_URLS: usize = 1 << 16;
/// The longest address that is kept, in bytes. A `data:` URL of an icon is a few KB.
const MAX_LEN: usize = 1 << 20;

#[derive(Default)]
struct Table {
    list: Vec<Arc<str>>,
    by_text: HashMap<Arc<str>, u32>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// The number of `url`: the same text gives the same number. 0 when the text is empty, too long, or the table is full.
pub fn intern(url: &str) -> u32 {
    if url.is_empty() || url.len() > MAX_LEN {
        return 0;
    }
    let mut guard = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let t = guard.get_or_insert_with(Table::default);
    if let Some(&id) = t.by_text.get(url) {
        return id;
    }
    if t.list.len() >= MAX_URLS {
        return 0;
    }
    let text: Arc<str> = url.into();
    t.list.push(text.clone());
    let id = t.list.len() as u32;
    t.by_text.insert(text, id);
    id
}

/// The address of number `id`, if there is one.
pub fn get(id: u32) -> Option<Arc<str>> {
    let guard = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref()?.list.get((id as usize).checked_sub(1)?).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_address_has_one_number() {
        let (a, b) = (intern("https://x.test/a.svg"), intern("data:image/svg+xml,<svg/>"));
        assert!(a > 0 && b > 0 && a != b);
        assert_eq!(intern("https://x.test/a.svg"), a);
        assert_eq!(get(a).as_deref(), Some("https://x.test/a.svg"));
        assert_eq!((intern(""), get(0)), (0, None));
    }
}

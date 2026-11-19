//! Host-filled properties by host key (`docs/design-lazy-properties.md`, option A).
//!
//! An indexed or named object (`NodeList`, `DOMTokenList`, `CSSStyleDeclaration`, `NamedNodeMap`, `TextTrackList`, `CSSRuleList`,
//! `DOMStringMap`, `Storage`, the named properties of the window and the document) gets a host key when it is made (`Dom::new_source`).
//! The key is its kind, and the engine gives the object with the key, as V8 gives the receiver to an interceptor. So a read goes
//! to its subsystem with one jump, and the subsystem finds its state by the object as before: no probe of each subsystem's map
//! and no table read. The object is not an owner for the collector yet: its references still go through the old per-object hook.
//! When it becomes an owner (a later PR), its key gets an owner slot in the bits above the kind.

use crate::browser::{Dom, R};
use bare_js::heap::{HeapError, Slot};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_js_api::{Item, Named, Pending};

/// The kind of a host-filled object: its host key. 0 is no kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Source {
    NodeList = 1,
    TokenList,
    Style,
    AttrMap,
    Captions,
    RuleList,
    Dataset,
    Storage,
    Frames,
}

impl Source {
    /// The kind of host key `key`.
    #[inline]
    fn of(key: u32) -> Option<Source> {
        Some(match key {
            1 => Source::NodeList,
            2 => Source::TokenList,
            3 => Source::Style,
            4 => Source::AttrMap,
            5 => Source::Captions,
            6 => Source::RuleList,
            7 => Source::Dataset,
            8 => Source::Storage,
            9 => Source::Frames,
            _ => return None,
        })
    }
}

impl Dom {
    /// A new object of kind `k` with prototype `proto`: indexed, or named for `Dataset`, `Storage` and `Frames` (the last two are
    /// open: a property of the chain wins over a name).
    pub(crate) fn new_source(&self, it: &mut Interp<'_>, k: Source, proto: Option<ObjId>) -> R<ObjId> {
        let key = k as u32;
        match k {
            Source::Dataset => it.heap.new_named(proto, key),
            Source::Storage | Source::Frames => it.heap.new_named_open(proto, key),
            _ => it.heap.new_indexed(proto, key),
        }
        .map_err(|e| it.heap_error(e))
    }
}

/// The object and kind of a call. `None`: not a host-filled object of this browser.
#[inline]
fn of(object: &Value, key: u32) -> Option<(Source, ObjId)> {
    match (Source::of(key), object) {
        (Some(k), Value::Object(o)) => Some((k, *o)),
        _ => None,
    }
}

#[inline]
fn pending(e: HeapError) -> Pending {
    match e {
        HeapError::NeedsHost { index, .. } => Pending::Fill(index),
        // The read callbacks give no other error.
        _ => Pending::Busy,
    }
}

#[inline]
fn item(s: Slot) -> Item<Value> {
    match s {
        Slot::Absent => Item::Absent,
        Slot::Missing => Item::Missing,
        Slot::Ready(o) => Item::Value(Value::Object(o)),
        Slot::Str(s) => Item::Value(Value::String(s)),
    }
}

#[inline]
pub(crate) fn indexed_len(d: &Dom, object: &Value, key: u32) -> Result<usize, Pending> {
    let Some((k, o)) = of(object, key) else { return Ok(0) };
    let r = match k {
        Source::NodeList => crate::browser_nodelist::len(d, o),
        Source::TokenList => crate::browser_platform_class::indexed_len(d, o),
        Source::Style => crate::browser_style::indexed_len(d, o),
        Source::AttrMap => Some(crate::browser_attr::len(d, o)),
        Source::Captions => crate::browser_media::caption_len(d, o),
        Source::RuleList => crate::browser_cssom::len(d, o),
        Source::Dataset | Source::Storage | Source::Frames => None,
    };
    r.unwrap_or(Ok(0)).map_err(pending)
}

#[inline]
pub(crate) fn indexed_item(d: &Dom, object: &Value, key: u32, index: usize) -> Result<Item<Value>, Pending> {
    let Some((k, o)) = of(object, key) else { return Ok(Item::Absent) };
    let r = match k {
        Source::NodeList => crate::browser_nodelist::item(d, o, index),
        Source::TokenList => crate::browser_platform_class::indexed_item(d, o, index),
        Source::Style => crate::browser_style::indexed_item(d, o, index),
        Source::AttrMap => Some(crate::browser_attr::item(d, o, index)),
        Source::Captions => crate::browser_media::caption_item(d, o, index),
        Source::RuleList => crate::browser_cssom::item(d, o, index),
        Source::Dataset | Source::Storage | Source::Frames => None,
    };
    r.unwrap_or(Ok(Slot::Absent)).map(item).map_err(pending)
}

#[inline]
pub(crate) fn indexed_name(d: &Dom, object: &Value, key: u32, name: &[u16]) -> Result<Option<u32>, Pending> {
    match of(object, key) {
        Some((Source::AttrMap, o)) => crate::browser_attr::name(d, o, name).map_err(pending),
        _ => Ok(None),
    }
}

#[inline]
pub(crate) fn indexed_names(d: &Dom, object: &Value, key: u32) -> Result<Vec<Vec<u16>>, Pending> {
    match of(object, key) {
        Some((Source::AttrMap, o)) => crate::browser_attr::names(d, o).map_err(pending),
        _ => Ok(Vec::new()),
    }
}

#[inline]
pub(crate) fn named_has(d: &Dom, object: &Value, key: u32, name: &[u16]) -> Result<bool, Pending> {
    let r = match of(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(false), |s| d.storage_get(s, name).map(|v| v.is_some())),
        Some((Source::Dataset, o)) => crate::browser_dataset::has(d, o, name),
        Some((Source::Frames, o)) => crate::browser_frames::has(d, o, name).map(|v| v.unwrap_or(false)),
        _ => Ok(false),
    };
    r.map_err(pending)
}

#[inline]
pub(crate) fn named_get(d: &Dom, object: &Value, key: u32, name: &[u16]) -> Result<Option<Named<Value>>, Pending> {
    let r = match of(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(None), |s| d.storage_get(s, name).map(|v| v.map(Named::Text))),
        Some((Source::Dataset, o)) => crate::browser_dataset::get(d, o, name),
        Some((Source::Frames, o)) => Ok(crate::browser_frames::value(d, o, name).map(Named::Value)),
        _ => Ok(None),
    };
    r.map_err(pending)
}

#[inline]
pub(crate) fn named_len(d: &Dom, object: &Value, key: u32) -> Result<usize, Pending> {
    let r = match of(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(0), |s| d.storage_len(s)),
        Some((Source::Dataset, o)) => crate::browser_dataset::len(d, o),
        Some((Source::Frames, o)) => crate::browser_frames::len(d, o).map(|v| v.unwrap_or(0)),
        _ => Ok(0),
    };
    r.map_err(pending)
}

#[inline]
pub(crate) fn named_keys(d: &Dom, object: &Value, key: u32) -> Result<Vec<Vec<u16>>, Pending> {
    let r = match of(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(Vec::new()), |s| d.storage_keys(s)),
        Some((Source::Dataset, o)) => crate::browser_dataset::keys(d, o),
        Some((Source::Frames, o)) => Ok(crate::browser_frames::keys(d, o).unwrap_or_default()),
        _ => Ok(Vec::new()),
    };
    r.map_err(pending)
}

/// `Host::fill_indexed` for a host-filled object: the fill step of its subsystem. `Ok(false)`: `owner` is not one.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<bool> {
    match it.heap.host_key(owner).and_then(Source::of) {
        Some(Source::NodeList) => crate::browser_nodelist::fill(d, it, owner, index),
        Some(Source::TokenList) => crate::browser_platform_class::indexed_fill(d, it, owner),
        Some(Source::Style) => crate::browser_style::fill(d, it, owner),
        Some(Source::Dataset) => crate::browser_dataset::fill(d, it, owner),
        Some(Source::RuleList) => crate::browser_cssom::fill(d, it, owner, index),
        Some(Source::Captions) => crate::browser_media::caption_fill(d, it, owner, index),
        Some(Source::AttrMap | Source::Storage | Source::Frames) | None => Ok(false),
    }
}

//! Host-filled properties by host key (`docs/design-lazy-properties.md`, option A).
//!
//! An indexed or named object (`NodeList`, `DOMTokenList`, `CSSStyleDeclaration`, `NamedNodeMap`, `TextTrackList`, `CSSRuleList`,
//! `DOMStringMap`, `Storage`, the named properties of the window and the document) gets a host key when it is made (`Dom::new_source`).
//! The key is its kind in the low `KIND_BITS` bits, and the engine gives the object with the key, as V8 gives the receiver to an
//! interceptor. So a read goes to its subsystem with one jump, and the subsystem finds its state by the object.
//!
//! The objects that hold references or table entries are owners for the collector (`docs/design-host-tracing.md`). Their key has an
//! owner slot (`Owners`) in the bits above the kind. The collector calls `Host::trace(key)` for a reached owner and `Host::prune`
//! with the keys of dead ones. The kind in the key picks the subsystem, so no other subsystem is asked. `Captions`, `Storage` and
//! `Frames` are not owners: they hold no reference and no entry that a collection frees (the page keeps them for its whole life).

use crate::eng::{Eng, Ops, Val};
use crate::browser::{Dom, KIND_BITS, Owner, R};
use crate::eng::Slot;
use bare_js_api::{Item, Named, Pending, Tracer};

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
    Rule,
    Sheet,
    Media,
    /// A `ValidityState` (`browser_controls.rs`): a plain object that is an owner for its table entries only.
    Validity,
}

impl Source {
    /// The kind of host key `key`.
    #[inline]
    fn of(key: u32) -> Option<Source> {
        Some(match key & ((1 << KIND_BITS) - 1) {
            1 => Source::NodeList,
            2 => Source::TokenList,
            3 => Source::Style,
            4 => Source::AttrMap,
            5 => Source::Captions,
            6 => Source::RuleList,
            7 => Source::Dataset,
            8 => Source::Storage,
            9 => Source::Frames,
            10 => Source::Rule,
            11 => Source::Sheet,
            12 => Source::Media,
            13 => Source::Validity,
            _ => return None,
        })
    }

    /// Is an object of this kind an owner: does it hold references, or entries that a collection frees?
    #[inline]
    fn owns(self) -> bool {
        !matches!(self, Source::Captions | Source::Storage | Source::Frames)
    }
}

impl<E: Eng> Dom<E> {
    /// A new object of kind `k` with prototype `proto`: indexed, or named for `Dataset`, `Storage` and `Frames` (the last two are
    /// open: a property of the chain wins over a name). An object that is an owner gets its key slot here, and it is registered
    /// with the engine.
    pub(crate) fn new_source(&self, it: &mut E::Vm, k: Source, proto: Option<E::Obj>) -> R<E, E::Obj> {
        if k.owns() {
            self.wrapper_edges.set(true);
        }
        let key = if k.owns() { self.st.borrow_mut().owners.key(Owner::Reserved) | k as u32 } else { k as u32 };
        let made = match k {
            Source::Dataset => it.h_new_named(proto, key),
            Source::Storage | Source::Frames => it.h_new_named_open(proto, key),
            _ => it.h_new_indexed(proto, key),
        };
        match made {
            Ok(o) => {
                if k.owns() {
                    self.st.borrow_mut().owners.set(key, Owner::Source(o));
                    it.set_host_owner_obj(o, key);
                }
                Ok(o)
            }
            Err(e) => {
                if k.owns() {
                    self.st.borrow_mut().owners.release(key);
                }
                Err(e)
            }
        }
    }
}

impl<E: Eng> Dom<E> {
    /// A new plain object of kind `k` (`Rule`, `Sheet` or `Media`) with prototype `proto`. It is an owner, as the objects of `new_source`.
    pub(crate) fn new_owned(&self, it: &mut E::Vm, k: Source, proto: E::Obj) -> R<E, E::Obj> {
        let key = self.st.borrow_mut().owners.key(Owner::Reserved) | k as u32;
        match self.obj(it, proto) {
            Ok(o) => {
                self.st.borrow_mut().owners.set(key, Owner::Source(o));
                it.set_host_owner_obj(o, key);
                Ok(o)
            }
            Err(e) => {
                self.st.borrow_mut().owners.release(key);
                Err(e)
            }
        }
    }
}

/// Keep the raw value `v` alive. For the tables of the subsystems, which still hold raw `ObjId`s and strings.
#[inline]
pub(crate) fn visit<E: Eng>(t: &mut Tracer<'_, E::Raw>, v: E::Raw) {
    t.visit(&bare_js_api::Traced::from_raw(v));
}

/// The roots of the page, as the trace of the window owner (`Dom::trace_roots`): a list that takes raw values. A subsystem keeps
/// its prototypes, constructors and queued callbacks in raw tables and adds them here, as it added them to the roots list of the old host.
pub(crate) struct Roots<'a, 'b, E: Eng>(pub &'a mut Tracer<'b, E::Raw>);

impl<E: Eng> Roots<'_, '_, E> {
    #[inline]
    pub(crate) fn push(&mut self, v: E::Raw) {
        visit::<E>(self.0, v);
    }

    #[inline]
    pub(crate) fn extend<I: IntoIterator<Item = E::Raw>>(&mut self, it: I) {
        for v in it {
            self.push(v);
        }
    }

    #[inline]
    pub(crate) fn extend_from_slice(&mut self, s: &[E::Raw]) {
        self.extend(s.iter().copied());
    }
}

/// `Host::trace` for the host-filled object `o` with host key `key`.
pub(crate) fn trace<E: Eng>(d: &Dom<E>, key: u32, o: E::Obj, t: &mut Tracer<'_, E::Raw>) {
    match Source::of(key) {
        Some(Source::TokenList) => crate::browser_platform::trace_list(d, o, t),
        Some(Source::Style) => {
            crate::browser_style::trace_obj(d, o, t);
            crate::browser_cssom::trace_decl(d, o, t);
        }
        Some(Source::AttrMap) => crate::browser_attr::trace_map(d, o, t),
        Some(Source::RuleList) => crate::browser_cssom::trace_list(d, o, t),
        Some(Source::Rule) => crate::browser_cssom::trace_rule(d, o, t),
        Some(Source::Sheet) => crate::browser_cssom::trace_sheet(d, o, t),
        Some(Source::Media) => crate::browser_cssom::trace_media(d, o, t),
        Some(Source::Dataset) => crate::browser_dataset::trace_obj(d, o, t),
        // A node list holds no reference. Captions, storage and frames are not owners.
        _ => {}
    }
}

/// The references that the wrapper of the node `n` keeps to its host-filled objects.
#[inline]
pub(crate) fn trace_node<E: Eng>(d: &Dom<E>, n: bare_html::dom::NodeId, t: &mut Tracer<'_, E::Raw>) {
    if d.wrapper_edges.get() {
        trace_node_edges(d, n, t);
    }
}

#[inline(never)]
fn trace_node_edges<E: Eng>(d: &Dom<E>, n: bare_html::dom::NodeId, t: &mut Tracer<'_, E::Raw>) {
    crate::browser_dataset::trace_node(d, n, t);
    crate::browser_platform::trace_node(d, n, t);
    crate::browser_style::trace_node(d, n, t);
    crate::browser_attr::trace_node(d, n, t);
    crate::browser_cssom::trace_node(d, n, t);
    if d.frames.active() {
        crate::browser_frames::trace_node(d, n, t);
    }
}

/// `Host::prune` for the host-filled object `o` with host key `key`, which the collector freed.
pub(crate) fn prune<E: Eng>(d: &Dom<E>, key: u32, o: E::Obj) {
    match Source::of(key) {
        Some(Source::NodeList) => crate::browser_nodelist::prune_list(d, o),
        Some(Source::TokenList) => crate::browser_platform::prune_list(d, o),
        Some(Source::Style) => {
            crate::browser_style::prune_obj(d, o);
            crate::browser_cssom::prune_decl(d, o);
        }
        Some(Source::AttrMap) => crate::browser_attr::prune_map(d, o),
        Some(Source::RuleList) => crate::browser_cssom::prune_list(d, o),
        Some(Source::Rule) => crate::browser_cssom::prune_rule(d, o),
        Some(Source::Sheet) => crate::browser_cssom::prune_sheet(d, o),
        Some(Source::Media) => crate::browser_cssom::prune_media(d, o),
        Some(Source::Dataset) => crate::browser_dataset::prune_obj(d, o),
        Some(Source::Validity) => crate::browser_controls::prune_validity(d, o),
        _ => {}
    }
}

/// The object and kind of a call. `None`: not a host-filled object of this browser.
#[inline]
fn of<E: Eng>(object: &E::Raw, key: u32) -> Option<(Source, E::Obj)> {
    match (Source::of(key), E::kind(*object)) {
        (Some(k), Val::Object(o)) => Some((k, o)),
        _ => None,
    }
}

#[inline]
fn item<E: Eng>(s: Slot<E>) -> Item<E::Raw> {
    match s {
        Slot::Absent => Item::Absent,
        Slot::Missing => Item::Missing,
        Slot::Ready(o) => Item::Value(E::obj_raw(o)),
        Slot::Str(s) => Item::Value(E::str_raw(s)),
    }
}

#[inline]
pub(crate) fn indexed_len<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32) -> Result<usize, Pending> {
    let Some((k, o)) = of::<E>(object, key) else { return Ok(0) };
    let r = match k {
        Source::NodeList => crate::browser_nodelist::len(d, o),
        Source::TokenList => crate::browser_platform_class::indexed_len(d, o),
        Source::Style => crate::browser_style::indexed_len(d, o),
        Source::AttrMap => Some(crate::browser_attr::len(d, o)),
        Source::Captions => crate::browser_media::caption_len(d, o),
        Source::RuleList => crate::browser_cssom::len(d, o),
        Source::Dataset | Source::Storage | Source::Frames | Source::Rule | Source::Sheet | Source::Media | Source::Validity => None,
    };
    r.unwrap_or(Ok(0))
}

#[inline]
pub(crate) fn indexed_item<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32, index: usize) -> Result<Item<E::Raw>, Pending> {
    let Some((k, o)) = of::<E>(object, key) else { return Ok(Item::Absent) };
    let r = match k {
        Source::NodeList => crate::browser_nodelist::item(d, o, index),
        Source::TokenList => crate::browser_platform_class::indexed_item(d, o, index),
        Source::Style => crate::browser_style::indexed_item(d, o, index),
        Source::AttrMap => Some(crate::browser_attr::item(d, o, index)),
        Source::Captions => crate::browser_media::caption_item(d, o, index),
        Source::RuleList => crate::browser_cssom::item(d, o, index),
        Source::Dataset | Source::Storage | Source::Frames | Source::Rule | Source::Sheet | Source::Media | Source::Validity => None,
    };
    r.unwrap_or(Ok(Slot::Absent)).map(item::<E>)
}

#[inline]
pub(crate) fn indexed_name<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32, name: &[u16]) -> Result<Option<u32>, Pending> {
    match of::<E>(object, key) {
        Some((Source::AttrMap, o)) => crate::browser_attr::name(d, o, name),
        _ => Ok(None),
    }
}

#[inline]
pub(crate) fn indexed_names<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32) -> Result<Vec<Vec<u16>>, Pending> {
    match of::<E>(object, key) {
        Some((Source::AttrMap, o)) => crate::browser_attr::names(d, o),
        _ => Ok(Vec::new()),
    }
}

#[inline]
pub(crate) fn named_has<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32, name: &[u16]) -> Result<bool, Pending> {
    let r = match of::<E>(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(false), |s| d.storage_get(s, name).map(|v| v.is_some())),
        Some((Source::Dataset, o)) => crate::browser_dataset::has(d, o, name),
        Some((Source::Frames, o)) => crate::browser_frames::has(d, o, name).map(|v| v.unwrap_or(false)),
        _ => Ok(false),
    };
    r
}

#[inline]
pub(crate) fn named_get<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32, name: &[u16]) -> Result<Option<Named<E::Raw>>, Pending> {
    let r = match of::<E>(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(None), |s| d.storage_get(s, name).map(|v| v.map(Named::Text))),
        Some((Source::Dataset, o)) => crate::browser_dataset::get(d, o, name),
        Some((Source::Frames, o)) => Ok(crate::browser_frames::value(d, o, name).map(Named::Value)),
        _ => Ok(None),
    };
    r
}

#[inline]
pub(crate) fn named_len<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32) -> Result<usize, Pending> {
    let r = match of::<E>(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(0), |s| d.storage_len(s)),
        Some((Source::Dataset, o)) => crate::browser_dataset::len(d, o),
        Some((Source::Frames, o)) => crate::browser_frames::len(d, o).map(|v| v.unwrap_or(0)),
        _ => Ok(0),
    };
    r
}

#[inline]
pub(crate) fn named_keys<E: Eng>(d: &Dom<E>, object: &E::Raw, key: u32) -> Result<Vec<Vec<u16>>, Pending> {
    let r = match of::<E>(object, key) {
        Some((Source::Storage, o)) => d.storage_area(o).map_or(Ok(Vec::new()), |s| d.storage_keys(s)),
        Some((Source::Dataset, o)) => crate::browser_dataset::keys(d, o),
        Some((Source::Frames, o)) => Ok(crate::browser_frames::keys(d, o).unwrap_or_default()),
        _ => Ok(Vec::new()),
    };
    r
}

/// `Host::fill` for a host-filled object: the fill step of its subsystem. `Ok(false)`: `owner` is not one.
pub(crate) fn fill<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, index: u32) -> R<E, bool> {
    match it.h_host_key(owner).and_then(Source::of) {
        Some(Source::NodeList) => crate::browser_nodelist::fill(d, it, owner, index),
        Some(Source::TokenList) => crate::browser_platform_class::indexed_fill(d, it, owner),
        Some(Source::Style) => crate::browser_style::fill(d, it, owner),
        Some(Source::Dataset) => crate::browser_dataset::fill(d, it, owner),
        Some(Source::RuleList) => crate::browser_cssom::fill(d, it, owner, index),
        Some(Source::Captions) => crate::browser_media::caption_fill(d, it, owner, index),
        Some(Source::AttrMap | Source::Storage | Source::Frames | Source::Rule | Source::Sheet | Source::Media | Source::Validity) | None => Ok(false),
    }
}

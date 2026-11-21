//! `HTMLMediaElement`, `HTMLAudioElement`, `HTMLVideoElement`, the `Audio` constructor, `TimeRanges` and `MediaError`.
//!
//! This engine has no decoder and no audio output. A media element is honest about it: it acts as a browser that supports no media format.
//! Every source fails with `MEDIA_ERR_SRC_NOT_SUPPORTED` (code 4), `canPlayType()` gives `""`, `readyState` stays 0, `duration` stays `NaN`, and
//! `play()` rejects with `NotSupportedError`. Nothing plays and no time passes. Chrome 155 is the model for the rest: the events, their order, the
//! error texts and the member list were recorded (`tools/fixtures/ui/media-cases.txt`, 77 cases, `BARE_MEDIA=1 node tools/ui/chrome_topic.mjs media`).
//!
//! Lazy: the six globals are accessors until the first read (`browser_lazy.rs`). The interfaces are made once, by the first read of a global or by
//! the first wrapper of an `<audio>` or `<video>` (`proto`, called by `Dom::wrap`). A page with no media pays for six accessor functions.
//!
//! State of an element (`M`) is kept in a table of `Dom`, made when the wrapper is made. `muted` is read from the `muted` attribute at that time and
//! is then separate from `defaultMuted` (Chrome: only the parser and the constructor copy the attribute).
//!
//! Load algorithm (HTML Standard, "media element load algorithm") on `src` set (the IDL setter, `setAttribute`), `load()`, `srcObject = null` and
//! `new Audio(src)`. Synchronously: the queued events of the element are dropped (`epoch` changes); when `networkState` is not `EMPTY`, `abort` (when it
//! was `IDLE` or `LOADING`), `emptied`, and `timeupdate` (when `currentTime` is not 0) are queued, and the pending `play()` promises reject with
//! `AbortError` if the element was playing; `playbackRate` goes back to `defaultPlaybackRate` (`ratechange` if it changed); `error` becomes null and
//! `networkState` becomes `NO_SOURCE`. Then a task due at once queues the resource selection. It runs after the tasks that the script queued, in
//! the place where Chrome runs the step at the end of the script, so `play` and `waiting` see the new `networkState`. `pause()` on an element with
//! `networkState` `EMPTY` runs the same step (Chrome does):
//! - no `src` attribute: `networkState` is `EMPTY`, no event, a pending `play()` stays pending (as in Chrome);
//! - a `src`: `currentSrc` is set and `loadstart` fires in a task due a hair later (after `play`, `waiting` and a `setTimeout(0)`). Then the load
//!   fails in a later task: `error`, then the pending promises reject.
//!   `networkState` is `LOADING` at `loadstart`; `NO_SOURCE` at once for an empty value, a URL that does not parse, a `file:` URL (all with a
//!   `currentSrc` of `""`, except `data:` which keeps its text) and `data:` (no type is supported). `preload="none"` (read here, as Chrome reads it) and a
//!   paused element: `networkState` is `IDLE`, `loadstart` and `suspend` fire, and nothing else happens until `play()` or a change of `preload` to
//!   another value (`ns` 2 at once, then `error` in a task). `load()` is never deferred (Chrome goes on to `LOADING` and `error`).
//!
//! Deviations from Chrome:
//! - No request is made. Chrome asks the server for the media (a range request); when the answer is a success it fires `progress` and `suspend`
//!   before `error`, and the error text depends on what came back. Here the text is `MEDIA_ELEMENT_ERROR: Format error` and those two events never
//!   fire. This is the case of a 404, of an unreachable host and of a scheme that Chrome has no loader for.
//! - `canPlayType()` gives `""` where Chrome says `probably` or `maybe` for mp3, wav, ogg, mp4 and webm.
//! - `play()` acts as if the page had a user gesture (no `NotAllowedError`). This engine has no user activation.
//! - An element that the parser makes with a `src` does not load until `load()` or `play()` runs (no hook in the tree builder). `<source>` children
//!   are not read at all: an element with only `<source>` children is an element with no source. `setAttribute('preload', ...)` does not resume an
//!   idle load (the IDL setter does).
//! - A MediaError that the page kept after a second failure reads `message` as `""`. `currentSrc` is set when the selection task runs, not at the
//!   end of the script.
//! - The handlers `onerror`, `onplay`... are on `HTMLMediaElement.prototype` (11 of them: the events that this file fires), not on `HTMLElement`.
//!   No `fastSeek` (Chrome 155 has none either), `audioTracks`, `captureStream`, `sinkId`, `mediaKeys`, `preservesPitch`,
//!   `controlsList`, `disableRemotePlayback`, `webkit*`, `getVideoPlaybackQuality`, `requestVideoFrameCallback`, picture in picture.
//!   `srcObject` takes `null` and `undefined` only (there is no `MediaStream`).
//!
//! GC: pending promises are persistent handles (the engine roots them); `MediaError` objects are roots. A queued event holds a node id, a generation and a step, nothing else. The number of
//! queued media events is capped (`max_media_tasks`, `media-tasks`).

use crate::eng::{Eng, Ops, Slot};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_dom::attr_set_s;
use crate::browser_lazy::lazy_global;
use crate::browser_navigation_url::{UErr, clean, parse, scheme_of};
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js_api::{Args, Error as AErr, HostFn, Local, Pending, Type};
use std::cell::{Cell, RefCell};

/// The events that this file fires. The index is the step of a queued task (and the `N` of a handler).
const EVENTS: [&str; 11] = ["abort", "emptied", "error", "loadstart", "pause", "play", "ratechange", "suspend", "timeupdate", "volumechange", "waiting"];
const ABORT: u8 = 0;
const EMPTIED: u8 = 1;
const LOADSTART: u8 = 3;
const PAUSE: u8 = 4;
const PLAY: u8 = 5;
const RATECHANGE: u8 = 6;
const SUSPEND: u8 = 7;
const TIMEUPDATE: u8 = 8;
const VOLUMECHANGE: u8 = 9;
const WAITING: u8 = 10;
/// Steps that are not an event.
const SELECT: u8 = 100;
const FAIL: u8 = 101;
const REJECT: u8 = 102;
const START: u8 = 103;

/// `networkState`.
const EMPTY_NS: u8 = 0;
const IDLE_NS: u8 = 1;
const LOADING_NS: u8 = 2;
const NO_SOURCE_NS: u8 = 3;

/// The resource selection task is due this long (ms) after the tasks that its script queued, and after a `setTimeout(0)`.
const SELECT_DELAY: f64 = 1e-3;

const FORMAT: &str = "MEDIA_ELEMENT_ERROR: Format error";
const SAFETY: &str = "MEDIA_ELEMENT_ERROR: Media load rejected by URL safety check";
const NO_SUPPORTED_SOURCE: &str = "Failed to load because no supported source was found.";
const NO_SUPPORTED_SOURCES: &str = "The element has no supported sources.";

#[derive(Clone, Copy)]
struct Made<E: Eng> {
    media: E::Obj,
    audio: E::Obj,
    video: E::Obj,
    ranges: E::Obj,
    error: E::Obj,
    /// `HTMLMediaElement`, `HTMLAudioElement`, `HTMLVideoElement`, `TimeRanges`, `MediaError`, `Audio`.
    ctors: [E::Obj; 6],
}

struct M<E: Eng> {
    ns: u8,
    paused: bool,
    /// `networkState` is `IDLE` because of `preload="none"`: `play()` or `preload` goes on.
    suspended: bool,
    /// `load()` ran: `preload="none"` does not defer this load (Chrome).
    eager: bool,
    muted: bool,
    volume: f64,
    rate: f64,
    default_rate: f64,
    /// The default playback start position: what `currentTime` reads while nothing is loaded.
    time: f64,
    /// Changes at each load algorithm. A queued task of an older generation does nothing.
    epoch: u32,
    current_src: String,
    /// The text of `error.message`.
    message: &'static str,
    error: Option<E::Obj>,
    /// Promises of `play()` that nothing settled.
    pending: Vec<bare_js_api::Persistent>,
    /// Promises that a `pause()` took and a task rejects.
    aborting: Vec<bare_js_api::Persistent>,
}

impl<E: Eng> M<E> {
    fn new(muted: bool) -> M<E> {
        M { ns: EMPTY_NS, paused: true, suspended: false, eager: false, muted, volume: 1.0, rate: 1.0, default_rate: 1.0, time: 0.0, epoch: 0, current_src: String::new(), message: FORMAT, error: None, pending: Vec::new(), aborting: Vec::new() }
    }
}

#[derive(Default)]
pub(crate) struct Media<E: Eng> {
    made: Cell<Option<Made<E>>>,
    st: RefCell<IdMap<NodeId, M<E>>>,
    tracks: RefCell<IdMap<NodeId, CaptionTrack<E>>>,
    lists: RefCell<IdMap<<E as Eng>::Obj, CaptionList>>,
    track_proto: Cell<Option<<E as Eng>::Obj>>,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    out.extend(d.media.track_proto.get().map(E::obj_raw));
    out.extend(d.media.lists.borrow().keys().copied().map(E::obj_raw));
    for t in d.media.tracks.borrow().values() {
        out.extend([E::obj_raw(t.object)]);
        out.extend(t.cues.map(E::obj_raw));
        out.extend(t.active.map(E::obj_raw));
        out.extend(t.entries.iter().map(|c| E::obj_raw(c.object)));
    }

    if let Some(m) = d.media.made.get() {
        out.extend([m.media, m.audio, m.video, m.ranges, m.error].into_iter().chain(m.ctors).map(E::obj_raw));
        for s in d.media.st.borrow().values() {
            out.extend(s.error.map(E::obj_raw));
        }
    }
}

/// The prototype of the wrapper of an HTML `audio` or `video` element `n`. Makes the state of the element.
pub(crate) fn proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId, tag: &str) -> R<E, E::Obj> {
    if tag == "track" {
        return caption_proto(d, it);
    }
    let m = ensure(d, it)?;
    let muted = d.doc.borrow().find_atom("muted").is_some_and(|a| d.doc.borrow().attr(n, a).is_some());
    d.media.st.borrow_mut().entry(n).or_insert_with(|| M::new(muted));
    Ok(if tag == "audio" { m.audio } else { m.video })
}

fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, Made<E>> {
    match d.media.made.get() {
        Some(m) => Ok(m),
        None => make(d, it),
    }
}

fn ens<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    ensure(d, it).map(|_| ())
}

fn pick<E: Eng>(d: &Dom<E>, i: usize) -> E::Obj {
    d.media.made.get().expect("made").ctors[i]
}

lazy_global!(lazy_media, "HTMLMediaElement", ens, |d| pick(d, 0));
lazy_global!(lazy_audio_el, "HTMLAudioElement", ens, |d| pick(d, 1));
lazy_global!(lazy_video_el, "HTMLVideoElement", ens, |d| pick(d, 2));
lazy_global!(lazy_ranges, "TimeRanges", ens, |d| pick(d, 3));
lazy_global!(lazy_error, "MediaError", ens, |d| pick(d, 4));
lazy_global!(lazy_audio, "Audio", ens, |d| pick(d, 5));

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "HTMLMediaElement", lazy_media),
        Row::both(1, "HTMLAudioElement", lazy_audio_el),
        Row::both(1, "HTMLVideoElement", lazy_video_el),
        Row::both(1, "TimeRanges", lazy_ranges),
        Row::both(1, "MediaError", lazy_error),
        Row::both(1, "Audio", lazy_audio),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// `new HTMLMediaElement()` and the like: `Failed to construct 'X': Illegal constructor`.
fn illegal_ctor<'s, const I: usize, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error(&format!("Failed to construct '{}': Illegal constructor", ["HTMLMediaElement", "HTMLAudioElement", "HTMLVideoElement", "TimeRanges", "MediaError"][I])))
}

/// The handler `on<EVENTS[N]>` of a media element.
fn on_get<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    old_v::<E>(s, |it| Ok(crate::browser_handlers::handler_get(d, it, Target::Node(n), EVENTS[N])))
}

fn on_set<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| crate::browser_handlers::handler_set(d, it, Target::Node(n), EVENTS[N], v))?;
    Ok(s.undefined())
}

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, Made<E>> {
    let op = it.object_prototype();
    let media = d.obj(it, d.p().html)?;
    let (audio, video, ranges, error) = (d.obj(it, media)?, d.obj(it, media)?, d.obj(it, op)?, d.obj(it, op)?);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut ctors = [media; 6];
    let ill: [HS<E>; 5] = [illegal_ctor::<0, _>, illegal_ctor::<1, _>, illegal_ctor::<2, _>, illegal_ctor::<3, _>, illegal_ctor::<4, _>];
    for (i, (name, proto)) in [("HTMLMediaElement", media), ("HTMLAudioElement", audio), ("HTMLVideoElement", video), ("TimeRanges", ranges), ("MediaError", error)].into_iter().enumerate() {
        let f = d.add_fn_s_obj(it, ill[i], name, 0, HostFn::Ctor)?;
        it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        ctors[i] = f;
    }
    // `Audio` has the prototype of `HTMLAudioElement`.
    let f = d.add_fn_s_obj(it, audio_ctor, "Audio", 0, HostFn::Ctor)?;
    it.define_data(f, kp, E::obj_raw(audio), false, false, false)?;
    ctors[5] = f;
    let m = Made { media, audio, video, ranges, error, ctors };
    d.def_get_s(it, error, "code", error_code, None)?;
    d.def_get_s(it, error, "message", error_message, None)?;
    d.def_get_s(it, ranges, "length", ranges_length, None)?;
    method(d, it, ranges, "end", 1, range_at::<true, _>)?;
    method(d, it, ranges, "start", 1, range_at::<false, _>)?;
    // Set at once: the members below make no wrapper, but a failure half way must not leave a made interface with no state.
    for (i, consts) in [
        &[("NETWORK_EMPTY", 0.0), ("NETWORK_IDLE", 1.0), ("NETWORK_LOADING", 2.0), ("NETWORK_NO_SOURCE", 3.0), ("HAVE_NOTHING", 0.0), ("HAVE_METADATA", 1.0), ("HAVE_CURRENT_DATA", 2.0), ("HAVE_FUTURE_DATA", 3.0), ("HAVE_ENOUGH_DATA", 4.0)][..],
        &[("MEDIA_ERR_ABORTED", 1.0), ("MEDIA_ERR_NETWORK", 2.0), ("MEDIA_ERR_DECODE", 3.0), ("MEDIA_ERR_SRC_NOT_SUPPORTED", 4.0)][..],
    ]
    .into_iter()
    .enumerate()
    {
        let (f, proto) = if i == 0 { (ctors[0], media) } else { (ctors[4], error) };
        for &(name, v) in consts {
            let k = d.key(it, name)?;
            it.define_data(f, k, E::num(v), false, true, false)?;
            it.define_data(proto, k, E::num(v), false, true, false)?;
        }
    }

    for (name, g, s) in [
        ("error", error_of as HS<E>, None),
        ("src", src_get, Some(src_set as HS<E>)),
        ("currentSrc", current_src, None),
        ("crossOrigin", cross_origin, Some(set_cross_origin)),
        ("networkState", network_state, None),
        ("preload", preload, Some(set_preload)),
        ("buffered", empty_ranges, None),
        ("readyState", zero, None),
        ("seeking", no, None),
        ("currentTime", current_time, Some(set_current_time)),
        ("duration", duration, None),
        ("paused", paused, None),
        ("defaultPlaybackRate", default_rate, Some(set_default_rate)),
        ("playbackRate", playback_rate, Some(set_playback_rate)),
        ("played", empty_ranges, None),
        ("seekable", empty_ranges, None),
        ("ended", no, None),
        ("autoplay", flag_get::<0, _>, Some(flag_set::<0, _> as HS<E>)),
        ("loop", flag_get::<1, _>, Some(flag_set::<1, _>)),
        ("controls", flag_get::<2, _>, Some(flag_set::<2, _>)),
        ("volume", volume, Some(set_volume)),
        ("muted", muted, Some(set_muted)),
        ("defaultMuted", flag_get::<3, _>, Some(flag_set::<3, _>)),
        ("srcObject", src_object, Some(set_src_object)),
    ] {
        d.def_get_s(it, media, name, g, s)?;
    }
    d.def_get_s(it, media, "textTracks", text_tracks, None)?;
    method(d, it, media, "load", 0, load)?;
    method(d, it, media, "play", 0, play)?;
    method(d, it, media, "pause", 0, pause)?;
    method(d, it, media, "canPlayType", 1, can_play_type)?;
    let on: [(HS<E>, HS<E>); 11] = [
        (on_get::<0, _>, on_set::<0, _>),
        (on_get::<1, _>, on_set::<1, _>),
        (on_get::<2, _>, on_set::<2, _>),
        (on_get::<3, _>, on_set::<3, _>),
        (on_get::<4, _>, on_set::<4, _>),
        (on_get::<5, _>, on_set::<5, _>),
        (on_get::<6, _>, on_set::<6, _>),
        (on_get::<7, _>, on_set::<7, _>),
        (on_get::<8, _>, on_set::<8, _>),
        (on_get::<9, _>, on_set::<9, _>),
        (on_get::<10, _>, on_set::<10, _>),
    ];
    for (i, (g, s)) in on.into_iter().enumerate() {
        let name = format!("on{}", EVENTS[i]);
        d.def_get_s(it, media, &name, g, Some(s))?;
    }
    for (name, g, s) in [
        ("width", video_size::<0, _> as HS<E>, Some(set_video_size::<0, _> as HS<E>)),
        ("height", video_size::<1, _>, Some(set_video_size::<1, _>)),
        ("videoWidth", zero_video, None),
        ("videoHeight", zero_video, None),
        ("poster", poster, Some(set_poster)),
        ("playsInline", flag_get::<4, _>, Some(flag_set::<4, _>)),
    ] {
        d.def_get_s(it, video, name, g, s)?;
    }
    // `constructor` comes last, as in Chrome.
    for (proto, f) in [media, audio, video, ranges, error].into_iter().zip(ctors) {
        it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    }
    d.media.made.set(Some(m));
    Ok(m)
}

/// A method of an interface: enumerable, as WebIDL makes it.
fn method<E: Eng>(d: &Dom<E>, it: &mut E::Vm, target: E::Obj, name: &str, len: u32, h: HS<E>) -> R<E, ()> {
    let (key, f) = (d.key(it, name)?, d.add_fn_s_obj(it, h, name, len, HostFn::Method)?);
    it.define_data(target, key, E::obj_raw(f), true, true, true)
}

#[cfg(test)]
impl<E: Eng> Dom<E> {
    /// Have the media interfaces been made? (Tests: a page without media pays nothing.)
    pub(crate) fn media_made(&self) -> bool {
        self.media.made.get().is_some()
    }
}

// ------------------------------------------------------------------------------------------ this

/// The native `audio` or `video` (`tag`, or either) of `this`, else `Illegal invocation`. Makes the state of an element that has none.
fn media_el<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, tag: Option<&str>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    let muted = {
        let doc = d.doc.borrow();
        let ok = match tag {
            Some(t) => doc.is_html(n, t),
            None => doc.is_html(n, "audio") || doc.is_html(n, "video"),
        };
        if !ok {
            drop(doc);
            return Err(s.type_error("Illegal invocation"));
        }
        doc.find_atom("muted").is_some_and(|a| doc.attr(n, a).is_some())
    };
    d.media.st.borrow_mut().entry(n).or_insert_with(|| M::new(muted));
    Ok(n)
}

fn with<T, E: Eng>(d: &Dom<E>, n: NodeId, f: impl FnOnce(&mut M<E>) -> T) -> T {
    f(d.media.st.borrow_mut().get_mut(&n).expect("state made with the wrapper"))
}

fn zero<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    Ok(s.number(0.0))
}

fn no<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    Ok(s.boolean(false))
}

fn zero_video<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, Some("video"))?;
    Ok(s.number(0.0))
}

fn duration<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    Ok(s.number(f64::NAN))
}

fn network_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.number(with(d, n, |m| m.ns) as f64))
}

fn paused<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.boolean(with(d, n, |m| m.paused)))
}

fn can_play_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'canPlayType' on 'HTMLMediaElement': 1 argument required, but only 0 present."));
    };
    d.string_arg_s(s, a0)?;
    d.js_str_s(s, "")
}

// ------------------------------------------------------------------------------------------ attributes

fn attr_of<E: Eng>(d: &Dom<E>, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
}

/// The content attributes that a boolean member reflects, in the order of `flag_get::<N, _>`.
const FLAGS: [&str; 5] = ["autoplay", "loop", "controls", "muted", "playsinline"];

fn flag_get<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, if N == 4 { Some("video") } else { None })?;
    Ok(s.boolean(attr_of(d, n, FLAGS[N]).is_some()))
}

fn flag_set<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, if N == 4 { Some("video") } else { None })?;
    let v = arg_s::<E>(s, a, 0);
    if s.to_boolean(v) {
        attr_set_s(d, s, n, FLAGS[N], "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, FLAGS[N]);
    }
    Ok(s.undefined())
}

fn muted<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.boolean(with(d, n, |m| m.muted)))
}

fn set_muted<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let v = arg_s::<E>(s, a, 0);
    let v = s.to_boolean(v);
    if with(d, n, |m| std::mem::replace(&mut m.muted, v)) != v {
        queue_event_s(d, s, n, VOLUMECHANGE)?;
    }
    Ok(s.undefined())
}

/// `preload` as the reflected enumerated attribute: none, metadata (missing or invalid), auto (empty).
fn preload<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let t = match attr_of(d, n, "preload").map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("none") => "none",
        Some("auto" | "") => "auto",
        _ => "metadata",
    };
    d.js_str_s(s, t)
}

fn set_preload<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let t = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, n, "preload", &t)?;
    if !t.eq_ignore_ascii_case("none") && with(d, n, |m| std::mem::take(&mut m.suspended)) {
        with(d, n, |m| m.ns = LOADING_NS);
        queue_s(d, s, n, FAIL, 0.0)?;
    }
    Ok(s.undefined())
}

fn cross_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    match attr_of(d, n, "crossorigin") {
        None => Ok(s.null()),
        Some(t) => d.js_str_s(s, if t.eq_ignore_ascii_case("use-credentials") { "use-credentials" } else { "anonymous" }),
    }
}

fn set_cross_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    if is_nullish::<E>(s, a0) {
        d.doc.borrow_mut().remove_attr(n, "crossorigin");
    } else {
        let t = d.string_arg_s(s, a0)?;
        attr_set_s(d, s, n, "crossorigin", &t)?;
    }
    Ok(s.undefined())
}

/// An `unsigned long` content attribute reads and writes 0 above this.
const MAX_UNSIGNED: u32 = i32::MAX as u32;

/// `width` (`I` 0) and `height` of a `video`: an unsigned integer, 0 when missing or not a number.
fn video_size<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, Some("video"))?;
    let t = attr_of(d, n, ["width", "height"][I]).unwrap_or_default();
    let t = t.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    Ok(s.number(t[..digits].parse::<u32>().ok().filter(|&v| v <= MAX_UNSIGNED).unwrap_or(0) as f64))
}

fn set_video_size<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, Some("video"))?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = bare_js::value::to_uint32(s.to_number(a0)?);
    attr_set_s(d, s, n, ["width", "height"][I], &(if v > MAX_UNSIGNED { 0 } else { v }).to_string())?;
    Ok(s.undefined())
}

fn poster<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, Some("video"))?;
    let t = attr_of(d, n, "poster").map_or_else(String::new, |r| resolved(d, &r));
    d.js_str_s(s, &t)
}

fn set_poster<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, Some("video"))?;
    let t = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, n, "poster", &t)?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ the URL

/// The URL of an attribute value: parsed against the base; the text as it is when it does not parse; a scheme in lower case for an opaque one.
pub(crate) fn resolved<E: Eng>(d: &Dom<E>, raw: &str) -> String {
    match parse(raw, crate::browser_elements::base(d).as_ref()) {
        Ok(r) => r.href(),
        Err(UErr::Invalid) => raw.to_string(),
        Err(_) => {
            let t = clean(raw);
            let t = t.trim_matches(|c: char| c <= ' ');
            scheme_of(t).map_or_else(|| t.to_string(), |(s, rest)| format!("{}:{rest}", s.to_ascii_lowercase()))
        }
    }
}

/// What the resource selection makes of a `src` value, with no decoder: `ok` means that a request would be made (and is not).
struct Source {
    ok: bool,
    current: String,
    message: &'static str,
}

fn classify<E: Eng>(d: &Dom<E>, raw: &str) -> Source {
    let bad = |current: String, message| Source { ok: false, current, message };
    if raw.is_empty() {
        return bad(String::new(), "MEDIA_ELEMENT_ERROR: Empty src attribute");
    }
    let good = |current| Source { ok: true, current, message: FORMAT };
    match parse(raw, crate::browser_elements::base(d).as_ref()) {
        Ok(r) => good(r.href()),
        Err(UErr::Invalid) => bad(String::new(), SAFETY),
        Err(UErr::Gate(_)) => good(resolved(d, raw)),
        Err(UErr::Scheme) => {
            let t = clean(raw);
            match scheme_of(t.trim_matches(|c: char| c <= ' ')).map(|s| s.0.to_ascii_lowercase()).as_deref() {
                Some("file") => bad(String::new(), SAFETY),
                Some("data") => bad(resolved(d, raw), "MEDIA_ELEMENT_ERROR: Unable to load URL due to content type"),
                _ => good(resolved(d, raw)),
            }
        }
    }
}

fn src_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let t = attr_of(d, n, "src").map_or_else(String::new, |r| resolved(d, &r));
    d.js_str_s(s, &t)
}

fn src_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let t = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, n, "src", &t)?;
    load_algorithm(d, s, n)?;
    Ok(s.undefined())
}

fn current_src<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let t = with(d, n, |m| m.current_src.clone());
    d.js_str_s(s, &t)
}

fn src_object<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    Ok(s.null())
}

/// `srcObject = null` runs the load algorithm. A value that is not `null` or `undefined` would be a `MediaStream` or a `MediaSourceHandle`: this
/// engine has neither, so it is the `TypeError` of a value of another type.
fn set_src_object<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    if !is_nullish::<E>(s, a0) {
        return Err(s.type_error("Failed to set the 'srcObject' property on 'HTMLMediaElement': The provided value is not of type '(MediaSourceHandle or MediaStream)'."));
    }
    load_algorithm(d, s, n)?;
    Ok(s.undefined())
}

/// `setAttribute('src', ...)` on any element: an `audio` or `video` runs the load algorithm.
pub(crate) fn src_changed<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, ()> {
    if d.media.tracks.borrow().contains_key(&n) {
        {
            let mut ts = d.media.tracks.borrow_mut();
            let t = ts.get_mut(&n).expect("checked");
            t.epoch = t.epoch.wrapping_add(1);
            t.ready = 0;
            t.cues = None;
            t.active = None;
        }
        return lift::<E, _>(s, caption_start(d, n));
    }
    if d.media.made.get().is_some() && d.media.st.borrow().contains_key(&n) {
        load_algorithm(d, s, n)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ numbers

/// The finite number of a setter, or the `TypeError` of a `double`.
fn finite<'s, E: Eng>(s: &mut Sc<'s, E>, name: &str, v: Local<'s>) -> Res<'s, f64> {
    let x = s.to_number(v)?;
    if x.is_finite() { Ok(x) } else { Err(s.type_error(&format!("Failed to set the '{name}' property on 'HTMLMediaElement': The provided double value is non-finite."))) }
}

fn volume<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.number(with(d, n, |m| m.volume)))
}

fn set_volume<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = finite::<E>(s, "volume", a0)?;
    if !(0.0..=1.0).contains(&v) {
        return Err(d.dom_err_s(s, "IndexSizeError", &format!("Failed to set the 'volume' property on 'HTMLMediaElement': The volume provided ({v}) is outside the range [0, 1].")));
    }
    if with(d, n, |m| std::mem::replace(&mut m.volume, v)) != v {
        queue_event_s(d, s, n, VOLUMECHANGE)?;
    }
    Ok(s.undefined())
}

fn current_time<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.number(with(d, n, |m| m.time)))
}

fn set_current_time<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = finite::<E>(s, "currentTime", a0)?;
    with(d, n, |m| m.time = v);
    Ok(s.undefined())
}

/// A rate that a browser supports: 0, or 1/16 to 16.
fn rate_ok(v: f64) -> bool {
    v == 0.0 || (0.0625..=16.0).contains(&v)
}

fn playback_rate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.number(with(d, n, |m| m.rate)))
}

fn set_playback_rate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = finite::<E>(s, "playbackRate", a0)?;
    if !rate_ok(v) {
        return Err(d.dom_err_s(s, "NotSupportedError", &format!("Failed to set the 'playbackRate' property on 'HTMLMediaElement': The provided playback rate ({v}) is not in the supported playback range.")));
    }
    if with(d, n, |m| std::mem::replace(&mut m.rate, v)) != v {
        queue_event_s(d, s, n, RATECHANGE)?;
    }
    Ok(s.undefined())
}

fn default_rate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(s.number(with(d, n, |m| m.default_rate)))
}

/// A value that is out of range is ignored, not an error (Chrome 155).
fn set_default_rate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = finite::<E>(s, "defaultPlaybackRate", a0)?;
    if rate_ok(v) && with(d, n, |m| std::mem::replace(&mut m.default_rate, v)) != v {
        queue_event_s(d, s, n, RATECHANGE)?;
    }
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ TimeRanges and MediaError

/// Every `TimeRanges` is empty. The brand is the prototype.
fn ranges_this<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, ()> {
    let want = d.media.made.get().map(|m| m.ranges);
    let ok = is_object::<E>(s, this)
        && match s.get_prototype_of(this) {
            Ok(Some(p)) => obj_id::<E>(s, p) == want,
            _ => false,
        };
    if ok { Ok(()) } else { Err(s.type_error("Illegal invocation")) }
}

fn empty_ranges<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    media_el(d, s, this, None)?;
    d.obj_with_s(s, d.media.made.get().expect("made").ranges)
}

fn ranges_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    ranges_this(d, s, this)?;
    Ok(s.number(0.0))
}

/// `end(i)` (`END`) and `start(i)`.
fn range_at<'s, const END: bool, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let name = if END { "end" } else { "start" };
    ranges_this(d, s, this)?;
    let Some(a0) = a.get(0) else {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'TimeRanges': 1 argument required, but only 0 present.")));
    };
    let i = bare_js::value::to_uint32(s.to_number(a0)?);
    Err(d.dom_err_s(s, "IndexSizeError", &format!("Failed to execute '{name}' on 'TimeRanges': The index provided ({i}) is greater than {}the maximum bound (0).", if i == 0 { "or equal to " } else { "" })))
}

fn error_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    Ok(match with(d, n, |m| m.error) {
        Some(o) => d.obj_local(s, o),
        None => s.null(),
    })
}

/// The element that holds the `MediaError` `this`, as its message.
fn error_this<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, &'static str> {
    let found = obj_id::<E>(s, this).and_then(|o| d.media.st.borrow().values().find(|m| m.error == Some(o)).map(|m| m.message));
    match found {
        Some(m) => Ok(m),
        None => Err(s.type_error("Illegal invocation")),
    }
}

fn error_code<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    error_this(d, s, this)?;
    Ok(s.number(4.0))
}

fn error_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let m = error_this(d, s, this)?;
    d.js_str_s(s, m)
}

// ------------------------------------------------------------------------------------------ the algorithms

/// Queue the step `step` of element `n`, `delay` ms after the tasks that are due now.
fn queue<E: Eng>(d: &Dom<E>, n: NodeId, step: u8, delay: f64) -> R<E, ()> {
    let epoch = with(d, n, |m| m.epoch);
    d.queue_media(n, epoch, step, delay)
}

/// `queue` for a handler: the failure as an error of the interface.
fn queue_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, step: u8, delay: f64) -> Res<'s, ()> {
    lift::<E, _>(s, queue(d, n, step, delay))
}

fn queue_event_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, ev: u8) -> Res<'s, ()> {
    queue_s(d, s, n, ev, 0.0)
}

/// Reject each promise of `list` with a `DOMException`.
fn reject_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, list: Vec<bare_js_api::Persistent>, name: &str, msg: &str) -> Res<'s, ()> {
    for p in list {
        let p = s.local(&p);
        match d.dom_err_s(s, name, msg) {
            AErr::Throw(v) => s.reject_promise(p, v)?,
            e => return Err(e),
        }
    }
    Ok(())
}

/// `reject_s` for a task.
fn reject<E: Eng>(d: &Dom<E>, it: &mut E::Vm, list: Vec<bare_js_api::Persistent>, name: &str, msg: &str) -> R<E, ()> {
    run::<E, _>(it, |s| reject_s(d, s, list, name, msg))
}

/// The media element load algorithm (see the header).
fn load_algorithm<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, ()> {
    let aborted = {
        let mut aborted = Vec::new();
        with(d, n, |m| m.epoch = m.epoch.wrapping_add(1));
        let (ns, was_paused, time) = with(d, n, |m| (m.ns, m.paused, m.time));
        if ns != EMPTY_NS {
            if ns == IDLE_NS || ns == LOADING_NS {
                queue_event_s(d, s, n, ABORT)?;
            }
            queue_event_s(d, s, n, EMPTIED)?;
            if !was_paused {
                aborted = with(d, n, |m| {
                    m.paused = true;
                    let mut all = std::mem::take(&mut m.pending);
                    all.append(&mut m.aborting);
                    all
                });
            }
            if time != 0.0 {
                queue_event_s(d, s, n, TIMEUPDATE)?;
            }
        }
        if with(d, n, |m| m.rate != m.default_rate) {
            with(d, n, |m| m.rate = m.default_rate);
            queue_event_s(d, s, n, RATECHANGE)?;
        }
        with(d, n, |m| {
            m.error = None;
            m.ns = NO_SOURCE_NS;
            m.suspended = false;
            m.eager = false;
        });
        queue_s(d, s, n, SELECT, 0.0)?;
        aborted
    };
    reject_s(d, s, aborted, "AbortError", "The play() request was interrupted by a new load request. https://goo.gl/LdLk22")
}

fn load<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    load_algorithm(d, s, n)?;
    with(d, n, |m| m.eager = true);
    Ok(s.undefined())
}

fn pause<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    if with(d, n, |m| m.ns == EMPTY_NS) {
        with(d, n, |m| m.ns = NO_SOURCE_NS);
        queue_s(d, s, n, SELECT, 0.0)?;
    }
    if with(d, n, |m| std::mem::replace(&mut m.paused, true)) {
        return Ok(s.undefined());
    }
    queue_event_s(d, s, n, TIMEUPDATE)?;
    queue_event_s(d, s, n, PAUSE)?;
    if with(d, n, |m| {
        let mut p = std::mem::take(&mut m.pending);
        m.aborting.append(&mut p);
        !m.aborting.is_empty()
    }) {
        queue_s(d, s, n, REJECT, 0.0)?;
    }
    Ok(s.undefined())
}

/// `play()` always gives a promise: a bad `this` is a rejected one (WebIDL). The promise of a source that failed rejects at once, and the one of a
/// source that is loading rejects with the failure.
fn play<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let r = (|| {
        let n = match media_el(d, s, this, None) {
            Ok(n) => n,
            Err(AErr::Throw(_)) => return Err(s.type_error("Failed to execute 'play' on 'HTMLMediaElement': Illegal invocation")),
            Err(e) => return Err(e),
        };
        if with(d, n, |m| m.error.is_some()) {
            return Err(d.dom_err_s(s, "NotSupportedError", NO_SUPPORTED_SOURCES));
        }
        let p = s.new_promise()?;
        let (select, resume, was_paused) = with(d, n, |m| {
            let select = m.ns == EMPTY_NS;
            let resume = std::mem::take(&mut m.suspended);
            if select {
                m.ns = NO_SOURCE_NS;
            } else if resume {
                m.ns = LOADING_NS;
            }
            (select, resume, std::mem::replace(&mut m.paused, false))
        });
        if select {
            queue_s(d, s, n, SELECT, 0.0)?;
        }
        if was_paused {
            queue_event_s(d, s, n, PLAY)?;
            queue_event_s(d, s, n, WAITING)?;
        }
        if resume {
            queue_s(d, s, n, FAIL, 0.0)?;
        }
        let held = s.persist(p);
        with(d, n, |m| m.pending.push(held));
        Ok(p)
    })();
    match r {
        Err(AErr::Throw(v)) => {
            let p = s.new_promise()?;
            s.reject_promise(p, v)?;
            Ok(p)
        }
        r => r,
    }
}

/// `new Audio(src)`: an `audio` with `preload="auto"`, and `src` when the argument is not `undefined`.
fn audio_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    let src = if s.type_of(a0) == Type::Undefined { None } else { Some(d.string_arg_s(s, a0)?) };
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, 0)?;
        if doc.find_atom("audio").is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim_s(s, "dom-names"));
        }
        doc.create_element("audio")
    };
    let w = d.wrap_s(s, n)?;
    attr_set_s(d, s, n, "preload", "auto")?;
    if let Some(t) = src {
        attr_set_s(d, s, n, "src", &t)?;
        load_algorithm(d, s, n)?;
    }
    Ok(w)
}

// ------------------------------------------------------------------------------------------ tasks

impl<E: Eng> Dom<E> {
    /// Does anything listen for `ty` at `n`, on the way up, or at the window?
    fn media_heard(&self, it: &mut E::Vm, n: NodeId, ty: &str) -> R<E, bool> {
        let (doc, st) = (self.doc.borrow(), self.st.borrow());
        let on = |t: Target<E>| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
        let mut cur = n;
        let mut found = on(Target::Node(cur));
        while let Some(p) = doc.parent(cur) {
            it.charge_o(1)?;
            cur = p;
            found |= on(Target::Node(cur));
        }
        Ok(found || on(Target::Window))
    }

    /// Fire the event `ev` at `n`: trusted, no bubbling, cancelable (Chrome 155 makes them so).
    fn media_fire(&self, it: &mut E::Vm, n: NodeId, ev: u8) -> R<E, ()> {
        let ty = EVENTS[ev as usize];
        if !self.media_heard(it, n, ty)? {
            return Ok(());
        }
        let e = self.new_event(it, ty, false, true, false, None)?;
        self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), e).map(|_| ())
    }

    /// Run a queued step. A step of an older generation does nothing.
    pub(crate) fn media_task(&self, it: &mut E::Vm, n: NodeId, epoch: u32, step: u8) -> R<E, ()> {
        if step == 104 { return caption_load(self, it, n, epoch); }
        if step >= crate::browser_img::LOADED {
            return self.img_task(it, n, step);
        }
        if self.media.st.borrow().get(&n).map(|m| m.epoch) != Some(epoch) {
            return Ok(());
        }
        match step {
            SELECT => select(self, n),
            START => start(self, it, n, epoch),
            FAIL => fail(self, it, n),
            REJECT => {
                let list = with(self, n, |m| std::mem::take(&mut m.aborting));
                reject(self, it, list, "AbortError", "The play() request was interrupted by a call to pause(). https://goo.gl/LdLk22")
            }
            ev => self.media_fire(it, n, ev),
        }
    }
}

/// The resource selection, at the end of the script that asked for it: the state of the element. `loadstart` follows in a task (`start`).
fn select<E: Eng>(d: &Dom<E>, n: NodeId) -> R<E, ()> {
    let Some(raw) = attr_of(d, n, "src") else {
        with(d, n, |m| m.ns = EMPTY_NS);
        return Ok(());
    };
    let none = attr_of(d, n, "preload").is_some_and(|p| p.eq_ignore_ascii_case("none"));
    let c = classify(d, &raw);
    let idle = c.ok && none && with(d, n, |m| m.paused && !std::mem::take(&mut m.eager));
    with(d, n, |m| {
        m.ns = if !c.ok { NO_SOURCE_NS } else if idle { IDLE_NS } else { LOADING_NS };
        m.suspended = idle;
        m.current_src = c.current;
        m.message = c.message;
    });
    queue(d, n, START, SELECT_DELAY)
}

/// `loadstart`, then the failure (or `suspend` for `preload="none"`).
fn start<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId, epoch: u32) -> R<E, ()> {
    d.media_fire(it, n, LOADSTART)?;
    let idle = with(d, n, |m| m.suspended);
    // A handler of `loadstart` may have run the load algorithm: the failure belongs to the old load.
    if with(d, n, |m| m.epoch) == epoch {
        queue(d, n, if idle { SUSPEND } else { FAIL }, 0.0)?;
    }
    Ok(())
}

/// The load failed: `error`, then the promises of `play()` reject. The list is taken before the event, so a `pause()` in a handler finds none.
fn fail<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId) -> R<E, ()> {
    let e = d.obj(it, d.media.made.get().expect("made").error)?;
    let list = with(d, n, |m| {
        m.ns = NO_SOURCE_NS;
        m.suspended = false;
        m.error = Some(e);
        let mut all = std::mem::take(&mut m.pending);
        all.append(&mut m.aborting);
        all
    });
    d.media_fire(it, n, 2)?;
    reject(d, it, list, "NotSupportedError", NO_SUPPORTED_SOURCE)
}

// Text tracks use the native element as their identity. They have no media decoder.
struct CaptionList {
    node: NodeId,
    revision: Option<u64>,
    ids: Vec<NodeId>,
}

struct CaptionTrack<E: Eng> {
    object: E::Obj,
    mode: String,
    epoch: u32,
    ready: u8,
    cues: Option<E::Obj>,
    active: Option<E::Obj>,
    entries: Vec<CaptionCue<E>>,
}

struct CaptionCue<E: Eng> {
    object: E::Obj,
    line: Option<f64>,
    size: f64,
}

fn caption_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    if let Some(p) = d.media.track_proto.get() { return Ok(p); }
    let p = d.obj(it, d.p().html)?;
    crate::browser_lazy::tag(d, it, p, "HTMLTrackElement")?;
    d.def_get_s(it, p, "track", caption_track, None)?;
    d.def_get_s(it, p, "readyState", caption_ready, None)?;
    d.def_get_s(it, p, "src", caption_src, Some(caption_set_src))?;
    d.media.track_proto.set(Some(p));
    Ok(p)
}

fn caption_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if !d.doc.borrow().is_html(n, "track") { return Err(s.type_error("Illegal invocation")); }
    Ok(n)
}

fn caption_track<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_node(d, s, this)?;
    caption_object(d, s, n)
}

fn caption_object<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, Local<'s>> {
    let made = d.media.tracks.borrow().get(&n).map(|t| t.object);
    if let Some(o) = made { return Ok(d.obj_local(s, o)); }
    if d.media.tracks.borrow().len() >= d.lim.max_wrappers { return Err(d.lim_s(s, "dom-wrappers")); }
    let o = s.new_object()?;
    let id = obj_id::<E>(s, o).expect("an object");
    old::<E, _>(s, |it| crate::browser_lazy::tag(d, it, id, "TextTrack"))?;
    table!(rows1 = [
        Row::get(1, "mode", caption_mode, Some(caption_set_mode)),
        Row::get(1, "cues", caption_cues, None),
        Row::get(1, "activeCues", caption_active, None),
    ]);
    let targets = [o];
    d.install_rows(s, &targets, &rows1::T::<E>::V)?;
    d.media.tracks.borrow_mut().insert(n, CaptionTrack { object: id, mode: "disabled".into(), epoch: 0, ready: 0, cues: None, active: None, entries: Vec::new() });
    lift::<E, _>(s, caption_start(d, n))?;
    Ok(o)
}

fn caption_receiver<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    s.charge(d.media.tracks.borrow().len() as u64 + 1)?;
    if let Some(o) = obj_id::<E>(s, this)
        && let Some((&n, _)) = d.media.tracks.borrow().iter().find(|(_, t)| t.object == o) { return Ok(n); }
    Err(s.type_error("Illegal invocation"))
}

fn caption_mode<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_receiver(d, s, this)?;
    let mode = d.media.tracks.borrow()[&n].mode.clone();
    d.js_str_s(s, &mode)
}

fn caption_set_mode<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_receiver(d, s, this)?;
    let mode = d.arg_str_s(s, a, 0)?;
    if !["disabled", "hidden", "showing"].contains(&mode.as_str()) { return Ok(s.undefined()); }
    d.media.tracks.borrow_mut().get_mut(&n).expect("receiver checked").mode = mode;
    lift::<E, _>(s, caption_start(d, n))?;
    Ok(s.undefined())
}

fn caption_cues<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_receiver(d, s, this)?;
    let cues = {
        let ts = d.media.tracks.borrow();
        let t = &ts[&n];
        if t.mode == "disabled" { None } else { t.cues }
    };
    Ok(match cues { Some(o) => d.obj_local(s, o), None => s.null() })
}

fn text_tracks<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = media_el(d, s, this, None)?;
    s.charge(d.media.lists.borrow().len() as u64 + 1)?;
    let found = d.media.lists.borrow().iter().find(|(_, l)| l.node == n).map(|(&o, _)| o);
    if let Some(o) = found { return Ok(d.obj_local(s, o)); }
    if d.media.lists.borrow().len() >= d.lim.max_wrappers { return Err(d.lim_s(s, "dom-wrappers")); }
    let p = s.new_object()?;
    let pid = obj_id::<E>(s, p).expect("an object");
    old::<E, _>(s, |it| crate::browser_lazy::tag(d, it, pid, "TextTrackList"))?;
    d.def_get_l(s, p, "length", caption_list_length, None)?;
    let o = d.new_source_s(s, crate::browser_sources::Source::Captions, Some(pid))?;
    d.media.lists.borrow_mut().insert(o, CaptionList { node: n, revision: None, ids: Vec::new() });
    Ok(d.obj_local(s, o))
}

fn caption_prepare<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, owner: E::Obj) -> Res<'s, ()> {
    let (node, old) = { let ls = d.media.lists.borrow(); let l = &ls[&owner]; (l.node, l.revision) };
    let revision = d.doc.borrow().revision();
    if old == Some(revision) { return Ok(()); }
    let mut ids = Vec::new();
    {
        let doc = d.doc.borrow();
        for c in doc.children(node) {
            s.charge(1)?;
            if doc.is_html(c, "track") {
                if ids.len() >= d.lim.max_results { return Err(d.lim_s(s, "dom-results")); }
                ids.push(c);
            }
        }
    }
    let mut ls = d.media.lists.borrow_mut();
    let l = ls.get_mut(&owner).expect("list");
    l.ids = ids;
    l.revision = Some(revision);
    Ok(())
}

fn caption_list_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id::<E>(s, this).filter(|o| d.media.lists.borrow().contains_key(o)) else { return Err(s.type_error("Illegal invocation")); };
    caption_prepare(d, s, o)?;
    let len = d.media.lists.borrow()[&o].ids.len();
    Ok(s.number(len as f64))
}

pub(crate) fn caption_len<E: Eng>(d: &Dom<E>, owner: E::Obj) -> Option<Result<usize, Pending>> {
    let ls = d.media.lists.borrow();
    let l = ls.get(&owner)?;
    Some(if l.revision == Some(d.doc.borrow().revision()) { Ok(l.ids.len()) } else { Err(Pending::Fill(0)) })
}

pub(crate) fn caption_item<E: Eng>(d: &Dom<E>, owner: E::Obj, index: usize) -> Option<Result<Slot<E>, Pending>> {
    let ls = d.media.lists.borrow();
    let l = ls.get(&owner)?;
    if l.revision != Some(d.doc.borrow().revision()) { return Some(Err(Pending::Fill(index.min(u32::MAX as usize) as u32))); }
    Some(Ok(match l.ids.get(index) {
        None => Slot::Absent,
        Some(n) => match d.media.tracks.borrow().get(n) { Some(t) => Slot::Ready(t.object), None => Slot::Missing },
    }))
}

pub(crate) fn caption_fill<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, index: u32) -> R<E, bool> {
    if !d.media.lists.borrow().contains_key(&owner) { return Ok(false); }
    run::<E, _>(it, |s| {
        caption_prepare(d, s, owner)?;
        let n = d.media.lists.borrow()[&owner].ids.get(index as usize).copied();
        if let Some(n) = n { caption_object(d, s, n)?; }
        Ok(true)
    })
}

fn caption_ready<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_node(d, s, this)?;
    let ready = d.media.tracks.borrow().get(&n).map_or(0, |t| t.ready);
    Ok(s.number(ready as f64))
}

fn caption_src<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_node(d, s, this)?;
    d.js_str_s(s, &attr_of(d, n, "src").unwrap_or_default())
}

fn caption_set_src<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_node(d, s, this)?;
    let src = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, n, "src", &src)?;
    src_changed(d, s, n)?;
    Ok(s.undefined())
}

fn caption_start<E: Eng>(d: &Dom<E>, n: NodeId) -> R<E, ()> {
    let mut ts = d.media.tracks.borrow_mut();
    let t = ts.get_mut(&n).expect("track made");
    if t.mode == "disabled" || t.ready != 0 || attr_of(d, n, "src").is_none_or(|s| s.is_empty()) { return Ok(()); }
    d.queue_media(n, t.epoch, 104, 0.0)?;
    t.ready = 1;
    Ok(())
}

fn caption_load<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId, epoch: u32) -> R<E, ()> {
    if d.media.tracks.borrow().get(&n).is_none_or(|t| t.epoch != epoch || t.ready != 1) { return Ok(()); }
    let src = attr_of(d, n, "src").unwrap_or_default();
    if src.len() > d.lim.max_string_bytes { return Err(d.lim("dom-string")); }
    it.charge_o(src.len() as u64 + 1)?;
    // Only in-memory WebVTT is supported. Other resources fail rather than claim loaded captions.
    let payload = crate::images::data_url(&src);
    let parsed = payload.as_deref().and_then(|b| std::str::from_utf8(b).ok()).and_then(parse_captions);
    let ty = if let Some(cues) = parsed {
        if cues.len() + d.media.tracks.borrow().values().map(|t| t.entries.len()).sum::<usize>() > d.lim.max_results { return Err(d.lim("dom-results")); }
        let list = d.obj(it, it.object_prototype())?;
        let mut entries = Vec::new();
        for (i, (start, end, text)) in cues.into_iter().enumerate() {
            let o = d.obj(it, it.object_prototype())?;
            for (key, value) in [("startTime", E::num(start)), ("endTime", E::num(end))] {
                let k = d.key(it, key)?;
                it.define_data(o, k, value, true, true, true)?;
            }
            let k = d.key(it, "text")?;
            let v = d.js_str(it, &text)?;
            it.define_data(o, k, v, true, true, true)?;
            d.def_get_s(it, o, "line", cue_line, Some(set_cue_line))?;
            d.def_get_s(it, o, "size", cue_size, Some(set_cue_size))?;
            let k = d.key(it, &i.to_string())?;
            it.define_data(list, k, E::obj_raw(o), false, true, false)?;
            entries.push(CaptionCue { object: o, line: None, size: 100.0 });
        }
        let k = d.key(it, "length")?;
        it.define_data(list, k, E::num(entries.len() as f64), false, false, false)?;
        let active = d.obj(it, it.object_prototype())?;
        let k = d.key(it, "length")?;
        it.define_data(active, k, E::num(0.0), false, false, false)?;
        let mut ts = d.media.tracks.borrow_mut();
        let t = ts.get_mut(&n).expect("track");
        t.active = Some(active);
        t.cues = Some(list);
        t.entries.extend(entries);
        t.ready = 2;
        "load"
    } else {
        d.media.tracks.borrow_mut().get_mut(&n).expect("track").ready = 3;
        "error"
    };
    let e = d.new_event(it, ty, false, false, false, None)?;
    d.st.borrow_mut().events.get_mut(&e).expect("event").trusted = true;
    d.dispatch(it, Target::Node(n), e).map(|_| ())
}

fn parse_captions(text: &str) -> Option<Vec<(f64, f64, String)>> {
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = normalized.split("\n\n");
    let header = blocks.next()?.lines().next()?;
    if header != "WEBVTT" && !header.starts_with("WEBVTT ") && !header.starts_with("WEBVTT\t") { return None; }
    let mut cues = Vec::new();
    for block in blocks {
        let mut lines = block.lines();
        let Some(first) = lines.next() else { continue };
        if first == "NOTE" || first.starts_with("NOTE ") || first.starts_with("NOTE\t") { continue; }
        let timing = if first.contains("-->") { first } else { match lines.next() { Some(l) => l, None => continue } };
        let Some((start, end)) = timing.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (caption_timestamp(start.trim()), end.split_whitespace().next().and_then(caption_timestamp)) else { continue };
        if end <= start { continue; }
        cues.push((start, end, lines.collect::<Vec<_>>().join("\n")));
    }
    Some(cues)
}

fn caption_timestamp(s: &str) -> Option<f64> {
    let fields: Vec<_> = s.split(':').collect();
    if !(2..=3).contains(&fields.len()) { return None; }
    let (seconds, fraction) = fields.last()?.split_once('.')?;
    if seconds.len() != 2 || fraction.len() != 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) { return None; }
    let seconds: u32 = seconds.parse().ok()?;
    let minutes: u32 = fields[fields.len() - 2].parse().ok()?;
    if seconds >= 60 || minutes >= 60 { return None; }
    let hours: u32 = if fields.len() == 3 { fields[0].parse().ok()? } else { 0 };
    Some(hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds as f64 + fraction.parse::<u32>().ok()? as f64 / 1000.0)
}

fn with_cue<'s, T, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&mut CaptionCue<E>) -> T) -> Res<'s, T> {
    s.charge(d.media.tracks.borrow().len() as u64 + 1)?;
    s.charge(d.media.tracks.borrow().values().map(|t| t.entries.len() as u64 + 1).sum::<u64>())?;
    if let Some(o) = obj_id::<E>(s, this) {
        for t in d.media.tracks.borrow_mut().values_mut() {
            if let Some(c) = t.entries.iter_mut().find(|c| c.object == o) { return Ok(f(c)); }
        }
    }
    Err(s.type_error("Illegal invocation"))
}

fn cue_line<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    match with_cue(d, s, this, |c| c.line)? { Some(n) => Ok(s.number(n)), None => d.js_str_s(s, "auto") }
}
fn set_cue_line<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    with_cue(d, s, this, |_| ())?;
    let v = arg_s::<E>(s, a, 0);
    let line = if s.type_of(v) == Type::String && d.string_arg_s(s, v)? == "auto" { None } else { Some(finite::<E>(s, "line", v)?) };
    with_cue(d, s, this, |c| c.line = line)?;
    Ok(s.undefined())
}
fn cue_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let size = with_cue(d, s, this, |c| c.size)?;
    Ok(s.number(size))
}
fn set_cue_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    with_cue(d, s, this, |_| ())?;
    let a0 = arg_s::<E>(s, a, 0);
    let size = finite::<E>(s, "size", a0)?;
    if !(0.0..=100.0).contains(&size) { return Err(d.dom_err_s(s, "IndexSizeError", "Size must be between 0 and 100")); }
    with_cue(d, s, this, |c| c.size = size)?;
    Ok(s.undefined())
}

fn caption_active<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = caption_receiver(d, s, this)?;
    let active = {
        let ts = d.media.tracks.borrow();
        let t = &ts[&n];
        if t.mode == "disabled" { None } else { t.active }
    };
    Ok(match active { Some(o) => d.obj_local(s, o), None => s.null() })
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The media elements, their caption tracks and caption lists.
pub(crate) fn held<E: Eng>(d: &Dom<E>, out: &mut Vec<NodeId>) {
    out.extend(d.media.st.borrow().keys().copied());
    out.extend(d.media.tracks.borrow().keys().copied());
    for l in d.media.lists.borrow().values() {
        out.push(l.node);
        out.extend(l.ids.iter().copied());
    }
}

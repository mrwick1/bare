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
//!   No `fastSeek` (Chrome 155 has none either), `textTracks`, `audioTracks`, `captureStream`, `sinkId`, `mediaKeys`, `preservesPitch`,
//!   `controlsList`, `disableRemotePlayback`, `webkit*`, `getVideoPlaybackQuality`, `requestVideoFrameCallback`, picture in picture.
//!   `srcObject` takes `null` and `undefined` only (there is no `MediaStream`).
//!
//! GC: pending promises and `MediaError` objects are roots. A queued event holds a node id, a generation and a step, nothing else. The number of
//! queued media events is capped (`max_media_tasks`, `media-tasks`).

use crate::browser::*;
use crate::browser_dom::attr_set;
use crate::browser_lazy::lazy_global;
use crate::browser_navigation_url::{UErr, clean, parse, scheme_of};
use crate::browser_platform::arg;
use bare_html::dom::NodeId;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, RefCell};

type V = R<Value>;

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
struct Made {
    media: ObjId,
    audio: ObjId,
    video: ObjId,
    ranges: ObjId,
    error: ObjId,
    /// `HTMLMediaElement`, `HTMLAudioElement`, `HTMLVideoElement`, `TimeRanges`, `MediaError`, `Audio`.
    ctors: [ObjId; 6],
}

struct M {
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
    error: Option<ObjId>,
    /// Promises of `play()` that nothing settled.
    pending: Vec<Value>,
    /// Promises that a `pause()` took and a task rejects.
    aborting: Vec<Value>,
}

impl M {
    fn new(muted: bool) -> M {
        M { ns: EMPTY_NS, paused: true, suspended: false, eager: false, muted, volume: 1.0, rate: 1.0, default_rate: 1.0, time: 0.0, epoch: 0, current_src: String::new(), message: FORMAT, error: None, pending: Vec::new(), aborting: Vec::new() }
    }
}

#[derive(Default)]
pub(crate) struct Media {
    made: Cell<Option<Made>>,
    st: RefCell<IdMap<NodeId, M>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if let Some(m) = d.media.made.get() {
        out.extend([m.media, m.audio, m.video, m.ranges, m.error].into_iter().chain(m.ctors).map(Value::Object));
        for s in d.media.st.borrow().values() {
            out.extend(s.pending.iter().chain(&s.aborting));
            out.extend(s.error.map(Value::Object));
        }
    }
}

/// The prototype of the wrapper of an HTML `audio` or `video` element `n`. Makes the state of the element.
pub(crate) fn proto(d: &Dom, it: &mut Interp<'_>, n: NodeId, tag: &str) -> R<ObjId> {
    let m = ensure(d, it)?;
    let muted = d.doc.borrow().find_atom("muted").is_some_and(|a| d.doc.borrow().attr(n, a).is_some());
    d.media.st.borrow_mut().entry(n).or_insert_with(|| M::new(muted));
    Ok(if tag == "audio" { m.audio } else { m.video })
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    match d.media.made.get() {
        Some(m) => Ok(m),
        None => make(d, it),
    }
}

fn ens(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

fn pick(d: &Dom, i: usize) -> ObjId {
    d.media.made.get().expect("made").ctors[i]
}

lazy_global!(lazy_media, "HTMLMediaElement", ens, |d| pick(d, 0));
lazy_global!(lazy_audio_el, "HTMLAudioElement", ens, |d| pick(d, 1));
lazy_global!(lazy_video_el, "HTMLVideoElement", ens, |d| pick(d, 2));
lazy_global!(lazy_ranges, "TimeRanges", ens, |d| pick(d, 3));
lazy_global!(lazy_error, "MediaError", ens, |d| pick(d, 4));
lazy_global!(lazy_audio, "Audio", ens, |d| pick(d, 5));

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("HTMLMediaElement", lazy_media as H), ("HTMLAudioElement", lazy_audio_el), ("HTMLVideoElement", lazy_video_el), ("TimeRanges", lazy_ranges), ("MediaError", lazy_error), ("Audio", lazy_audio)])
}

/// `new HTMLMediaElement()` and the like: `Failed to construct 'X': Illegal constructor`.
fn illegal_ctor<const I: usize>(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error(&format!("Failed to construct '{}': Illegal constructor", ["HTMLMediaElement", "HTMLAudioElement", "HTMLVideoElement", "TimeRanges", "MediaError"][I])))
}

/// The handler `on<EVENTS[N]>` of a media element.
fn on_get<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    Ok(crate::browser_handlers::handler_get(d, Target::Node(n), EVENTS[N]))
}

fn on_set<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    crate::browser_handlers::handler_set(d, Target::Node(n), EVENTS[N], a.first().copied())?;
    Ok(Value::Undefined)
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let op = it.object_prototype();
    let media = d.obj(it, d.p().html)?;
    let (audio, video, ranges, error) = (d.obj(it, media)?, d.obj(it, media)?, d.obj(it, op)?, d.obj(it, op)?);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut ctors = [media; 6];
    let ill: [H; 5] = [illegal_ctor::<0>, illegal_ctor::<1>, illegal_ctor::<2>, illegal_ctor::<3>, illegal_ctor::<4>];
    for (i, (name, proto)) in [("HTMLMediaElement", media), ("HTMLAudioElement", audio), ("HTMLVideoElement", video), ("TimeRanges", ranges), ("MediaError", error)].into_iter().enumerate() {
        let f = d.add_fn(it, ill[i], name, 0, true)?;
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        ctors[i] = f;
    }
    // `Audio` has the prototype of `HTMLAudioElement`.
    let f = d.add_fn(it, audio_ctor, "Audio", 0, true)?;
    it.define_data(f, kp, Value::Object(audio), false, false, false)?;
    ctors[5] = f;
    let m = Made { media, audio, video, ranges, error, ctors };
    d.def_get(it, error, "code", error_code, None)?;
    d.def_get(it, error, "message", error_message, None)?;
    d.def_get(it, ranges, "length", ranges_length, None)?;
    method(d, it, ranges, "end", 1, |d, it, t, a| range_at(d, it, t, a, "end"))?;
    method(d, it, ranges, "start", 1, |d, it, t, a| range_at(d, it, t, a, "start"))?;
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
            it.define_data(f, k, Value::Number(v), false, true, false)?;
            it.define_data(proto, k, Value::Number(v), false, true, false)?;
        }
    }

    for (name, g, s) in [
        ("error", error_of as H, None),
        ("src", src_get, Some(src_set as H)),
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
        ("autoplay", flag_get::<0>, Some(flag_set::<0> as H)),
        ("loop", flag_get::<1>, Some(flag_set::<1>)),
        ("controls", flag_get::<2>, Some(flag_set::<2>)),
        ("volume", volume, Some(set_volume)),
        ("muted", muted, Some(set_muted)),
        ("defaultMuted", flag_get::<3>, Some(flag_set::<3>)),
        ("srcObject", src_object, Some(set_src_object)),
    ] {
        d.def_get(it, media, name, g, s)?;
    }
    method(d, it, media, "load", 0, load)?;
    method(d, it, media, "play", 0, play)?;
    method(d, it, media, "pause", 0, pause)?;
    method(d, it, media, "canPlayType", 1, can_play_type)?;
    let on: [(H, H); 11] = [
        (on_get::<0>, on_set::<0>),
        (on_get::<1>, on_set::<1>),
        (on_get::<2>, on_set::<2>),
        (on_get::<3>, on_set::<3>),
        (on_get::<4>, on_set::<4>),
        (on_get::<5>, on_set::<5>),
        (on_get::<6>, on_set::<6>),
        (on_get::<7>, on_set::<7>),
        (on_get::<8>, on_set::<8>),
        (on_get::<9>, on_set::<9>),
        (on_get::<10>, on_set::<10>),
    ];
    for (i, (g, s)) in on.into_iter().enumerate() {
        let name = format!("on{}", EVENTS[i]);
        d.def_get(it, media, &name, g, Some(s))?;
    }
    for (name, g, s) in [
        ("width", video_size::<0> as H, Some(set_video_size::<0> as H)),
        ("height", video_size::<1>, Some(set_video_size::<1>)),
        ("videoWidth", zero_video, None),
        ("videoHeight", zero_video, None),
        ("poster", poster, Some(set_poster)),
        ("playsInline", flag_get::<4>, Some(flag_set::<4>)),
    ] {
        d.def_get(it, video, name, g, s)?;
    }
    // `constructor` comes last, as in Chrome.
    for (proto, f) in [media, audio, video, ranges, error].into_iter().zip(ctors) {
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    }
    d.media.made.set(Some(m));
    Ok(m)
}

/// A method of an interface: enumerable, as WebIDL makes it.
fn method(d: &Dom, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: H) -> R<()> {
    let (key, f) = (d.key(it, name)?, d.add_fn(it, h, name, len, false)?);
    it.define_data(target, key, Value::Object(f), true, true, true)
}

#[cfg(test)]
impl Dom {
    /// Have the media interfaces been made? (Tests: a page without media pays nothing.)
    pub(crate) fn media_made(&self) -> bool {
        self.media.made.get().is_some()
    }
}

// ------------------------------------------------------------------------------------------ this

/// The native `audio` or `video` (`tag`, or either) of `this`, else `Illegal invocation`. Makes the state of an element that has none.
fn media_el(d: &Dom, it: &mut Interp<'_>, this: Value, tag: Option<&str>) -> R<NodeId> {
    let n = d.el(it, this)?;
    let muted = {
        let doc = d.doc.borrow();
        let ok = match tag {
            Some(t) => doc.is_html(n, t),
            None => doc.is_html(n, "audio") || doc.is_html(n, "video"),
        };
        if !ok {
            drop(doc);
            return Err(it.type_error("Illegal invocation"));
        }
        doc.find_atom("muted").is_some_and(|a| doc.attr(n, a).is_some())
    };
    d.media.st.borrow_mut().entry(n).or_insert_with(|| M::new(muted));
    Ok(n)
}

fn with<T>(d: &Dom, n: NodeId, f: impl FnOnce(&mut M) -> T) -> T {
    f(d.media.st.borrow_mut().get_mut(&n).expect("state made with the wrapper"))
}

fn num(v: f64) -> V {
    Ok(Value::Number(v))
}

fn zero(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, None)?;
    num(0.0)
}

fn no(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, None)?;
    Ok(Value::Bool(false))
}

fn zero_video(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, Some("video"))?;
    num(0.0)
}

fn duration(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, None)?;
    num(f64::NAN)
}

fn network_state(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    num(with(d, n, |m| m.ns) as f64)
}

fn paused(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    Ok(Value::Bool(with(d, n, |m| m.paused)))
}

fn can_play_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    media_el(d, it, this, None)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'canPlayType' on 'HTMLMediaElement': 1 argument required, but only 0 present."));
    }
    d.string_arg(it, a[0])?;
    d.js_str(it, "")
}

// ------------------------------------------------------------------------------------------ attributes

fn attr_of(d: &Dom, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
}

/// The content attributes that a boolean member reflects, in the order of `flag_get::<N>`.
const FLAGS: [&str; 5] = ["autoplay", "loop", "controls", "muted", "playsinline"];

fn flag_get<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, if N == 4 { Some("video") } else { None })?;
    Ok(Value::Bool(attr_of(d, n, FLAGS[N]).is_some()))
}

fn flag_set<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, if N == 4 { Some("video") } else { None })?;
    if it.to_js_bool(arg(a, 0))? {
        attr_set(d, it, n, FLAGS[N], "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, FLAGS[N]);
    }
    Ok(Value::Undefined)
}

fn muted(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    Ok(Value::Bool(with(d, n, |m| m.muted)))
}

fn set_muted(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let v = it.to_js_bool(arg(a, 0))?;
    if with(d, n, |m| std::mem::replace(&mut m.muted, v)) != v {
        queue_event(d, n, VOLUMECHANGE)?;
    }
    Ok(Value::Undefined)
}

/// `preload` as the reflected enumerated attribute: none, metadata (missing or invalid), auto (empty).
fn preload(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let s = match attr_of(d, n, "preload").map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("none") => "none",
        Some("auto" | "") => "auto",
        _ => "metadata",
    };
    d.js_str(it, s)
}

fn set_preload(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, "preload", &s)?;
    if !s.eq_ignore_ascii_case("none") && with(d, n, |m| std::mem::take(&mut m.suspended)) {
        with(d, n, |m| m.ns = LOADING_NS);
        queue(d, n, FAIL, 0.0)?;
    }
    Ok(Value::Undefined)
}

fn cross_origin(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    match attr_of(d, n, "crossorigin") {
        None => Ok(Value::Null),
        Some(s) => d.js_str(it, if s.eq_ignore_ascii_case("use-credentials") { "use-credentials" } else { "anonymous" }),
    }
}

fn set_cross_origin(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    if matches!(arg(a, 0), Value::Null | Value::Undefined) {
        d.doc.borrow_mut().remove_attr(n, "crossorigin");
    } else {
        let s = d.string_arg(it, a[0])?;
        attr_set(d, it, n, "crossorigin", &s)?;
    }
    Ok(Value::Undefined)
}

/// An `unsigned long` content attribute reads and writes 0 above this.
const MAX_UNSIGNED: u32 = i32::MAX as u32;

/// `width` (`I` 0) and `height` of a `video`: an unsigned integer, 0 when missing or not a number.
fn video_size<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, Some("video"))?;
    let s = attr_of(d, n, ["width", "height"][I]).unwrap_or_default();
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    num(t[..digits].parse::<u32>().ok().filter(|&v| v <= MAX_UNSIGNED).unwrap_or(0) as f64)
}

fn set_video_size<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, Some("video"))?;
    let v = bare_js::value::to_uint32(it.to_js_number(arg(a, 0))?);
    attr_set(d, it, n, ["width", "height"][I], &(if v > MAX_UNSIGNED { 0 } else { v }).to_string())?;
    Ok(Value::Undefined)
}

fn poster(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, Some("video"))?;
    let s = attr_of(d, n, "poster").map_or_else(String::new, |r| resolved(d, &r));
    d.js_str(it, &s)
}

fn set_poster(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, Some("video"))?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, "poster", &s)?;
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ the URL

/// The URL of an attribute value: parsed against the base; the text as it is when it does not parse; a scheme in lower case for an opaque one.
pub(crate) fn resolved(d: &Dom, raw: &str) -> String {
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

fn classify(d: &Dom, raw: &str) -> Source {
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

fn src_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let s = attr_of(d, n, "src").map_or_else(String::new, |r| resolved(d, &r));
    d.js_str(it, &s)
}

fn src_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, "src", &s)?;
    load_algorithm(d, it, n)?;
    Ok(Value::Undefined)
}

fn current_src(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let s = with(d, n, |m| m.current_src.clone());
    d.js_str(it, &s)
}

fn src_object(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, None)?;
    Ok(Value::Null)
}

/// `srcObject = null` runs the load algorithm. A value that is not `null` or `undefined` would be a `MediaStream` or a `MediaSourceHandle`: this
/// engine has neither, so it is the `TypeError` of a value of another type.
fn set_src_object(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    if !matches!(arg(a, 0), Value::Null | Value::Undefined) {
        return Err(it.type_error("Failed to set the 'srcObject' property on 'HTMLMediaElement': The provided value is not of type '(MediaSourceHandle or MediaStream)'."));
    }
    load_algorithm(d, it, n)?;
    Ok(Value::Undefined)
}

/// `setAttribute('src', ...)` on any element: an `audio` or `video` runs the load algorithm.
pub(crate) fn src_changed(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<()> {
    if d.media.made.get().is_some() && d.media.st.borrow().contains_key(&n) {
        load_algorithm(d, it, n)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ numbers

/// The finite number of a setter, or the `TypeError` of a `double`.
fn finite(it: &mut Interp<'_>, name: &str, v: Value) -> R<f64> {
    let x = it.to_js_number(v)?;
    if x.is_finite() { Ok(x) } else { Err(it.type_error(&format!("Failed to set the '{name}' property on 'HTMLMediaElement': The provided double value is non-finite."))) }
}

fn volume(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    num(with(d, n, |m| m.volume))
}

fn set_volume(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let v = finite(it, "volume", arg(a, 0))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(d.dom_err(it, "IndexSizeError", &format!("Failed to set the 'volume' property on 'HTMLMediaElement': The volume provided ({v}) is outside the range [0, 1].")));
    }
    if with(d, n, |m| std::mem::replace(&mut m.volume, v)) != v {
        queue_event(d, n, VOLUMECHANGE)?;
    }
    Ok(Value::Undefined)
}

fn current_time(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    num(with(d, n, |m| m.time))
}

fn set_current_time(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let v = finite(it, "currentTime", arg(a, 0))?;
    with(d, n, |m| m.time = v);
    Ok(Value::Undefined)
}

/// A rate that a browser supports: 0, or 1/16 to 16.
fn rate_ok(v: f64) -> bool {
    v == 0.0 || (0.0625..=16.0).contains(&v)
}

fn playback_rate(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    num(with(d, n, |m| m.rate))
}

fn set_playback_rate(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let v = finite(it, "playbackRate", arg(a, 0))?;
    if !rate_ok(v) {
        return Err(d.dom_err(it, "NotSupportedError", &format!("Failed to set the 'playbackRate' property on 'HTMLMediaElement': The provided playback rate ({v}) is not in the supported playback range.")));
    }
    if with(d, n, |m| std::mem::replace(&mut m.rate, v)) != v {
        queue_event(d, n, RATECHANGE)?;
    }
    Ok(Value::Undefined)
}

fn default_rate(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    num(with(d, n, |m| m.default_rate))
}

/// A value that is out of range is ignored, not an error (Chrome 155).
fn set_default_rate(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    let v = finite(it, "defaultPlaybackRate", arg(a, 0))?;
    if rate_ok(v) && with(d, n, |m| std::mem::replace(&mut m.default_rate, v)) != v {
        queue_event(d, n, RATECHANGE)?;
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ TimeRanges and MediaError

/// Every `TimeRanges` is empty. The brand is the prototype.
fn ranges_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    let ok = matches!(this, Value::Object(o) if it.heap.get_prototype_of(o).ok().flatten() == d.media.made.get().map(|m| m.ranges));
    if ok { Ok(()) } else { Err(it.type_error("Illegal invocation")) }
}

fn empty_ranges(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    media_el(d, it, this, None)?;
    Ok(Value::Object(d.obj(it, d.media.made.get().expect("made").ranges)?))
}

fn ranges_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    ranges_this(d, it, this)?;
    num(0.0)
}

fn range_at(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], name: &str) -> V {
    ranges_this(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'TimeRanges': 1 argument required, but only 0 present.")));
    }
    let i = bare_js::value::to_uint32(it.to_js_number(a[0])?);
    Err(d.dom_err(it, "IndexSizeError", &format!("Failed to execute '{name}' on 'TimeRanges': The index provided ({i}) is greater than {}the maximum bound (0).", if i == 0 { "or equal to " } else { "" })))
}

fn error_of(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    Ok(with(d, n, |m| m.error).map_or(Value::Null, Value::Object))
}

/// The element that holds the `MediaError` `this`, as its message.
fn error_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<&'static str> {
    let found = match this {
        Value::Object(o) => d.media.st.borrow().values().find(|m| m.error == Some(o)).map(|m| m.message),
        _ => None,
    };
    found.ok_or_else(|| it.type_error("Illegal invocation"))
}

fn error_code(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    error_this(d, it, this)?;
    num(4.0)
}

fn error_message(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let m = error_this(d, it, this)?;
    d.js_str(it, m)
}

// ------------------------------------------------------------------------------------------ the algorithms

/// Queue the step `step` of element `n`, `delay` ms after the tasks that are due now.
fn queue(d: &Dom, n: NodeId, step: u8, delay: f64) -> R<()> {
    let epoch = with(d, n, |m| m.epoch);
    d.queue_media(n, epoch, step, delay)
}

fn queue_event(d: &Dom, n: NodeId, ev: u8) -> R<()> {
    queue(d, n, ev, 0.0)
}

/// Reject each promise of `list` with a `DOMException`.
fn reject(d: &Dom, it: &mut Interp<'_>, list: Vec<Value>, name: &str, msg: &str) -> R<()> {
    for p in list {
        match d.dom_err(it, name, msg) {
            Error::Throw(v) => it.host_reject_promise(p, v)?,
            e => return Err(e),
        }
    }
    Ok(())
}

/// The media element load algorithm (see the header).
fn load_algorithm(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<()> {
    let aborted = {
        let mut aborted = Vec::new();
        with(d, n, |m| m.epoch = m.epoch.wrapping_add(1));
        let (ns, was_paused, time) = with(d, n, |m| (m.ns, m.paused, m.time));
        if ns != EMPTY_NS {
            if ns == IDLE_NS || ns == LOADING_NS {
                queue_event(d, n, ABORT)?;
            }
            queue_event(d, n, EMPTIED)?;
            if !was_paused {
                aborted = with(d, n, |m| {
                    m.paused = true;
                    let mut all = std::mem::take(&mut m.pending);
                    all.append(&mut m.aborting);
                    all
                });
            }
            if time != 0.0 {
                queue_event(d, n, TIMEUPDATE)?;
            }
        }
        if with(d, n, |m| m.rate != m.default_rate) {
            with(d, n, |m| m.rate = m.default_rate);
            queue_event(d, n, RATECHANGE)?;
        }
        with(d, n, |m| {
            m.error = None;
            m.ns = NO_SOURCE_NS;
            m.suspended = false;
            m.eager = false;
        });
        queue(d, n, SELECT, 0.0)?;
        aborted
    };
    reject(d, it, aborted, "AbortError", "The play() request was interrupted by a new load request. https://goo.gl/LdLk22")
}

fn load(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    load_algorithm(d, it, n)?;
    with(d, n, |m| m.eager = true);
    Ok(Value::Undefined)
}

fn pause(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = media_el(d, it, this, None)?;
    if with(d, n, |m| m.ns == EMPTY_NS) {
        with(d, n, |m| m.ns = NO_SOURCE_NS);
        queue(d, n, SELECT, 0.0)?;
    }
    if with(d, n, |m| std::mem::replace(&mut m.paused, true)) {
        return Ok(Value::Undefined);
    }
    queue_event(d, n, TIMEUPDATE)?;
    queue_event(d, n, PAUSE)?;
    if with(d, n, |m| {
        let mut p = std::mem::take(&mut m.pending);
        m.aborting.append(&mut p);
        !m.aborting.is_empty()
    }) {
        queue(d, n, REJECT, 0.0)?;
    }
    Ok(Value::Undefined)
}

/// `play()` always gives a promise: a bad `this` is a rejected one (WebIDL). The promise of a source that failed rejects at once, and the one of a
/// source that is loading rejects with the failure.
fn play(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let r = (|| {
        let n = media_el(d, it, this, None).map_err(|e| match e {
            Error::Throw(_) => it.type_error("Failed to execute 'play' on 'HTMLMediaElement': Illegal invocation"),
            e => e,
        })?;
        if with(d, n, |m| m.error.is_some()) {
            return Err(d.dom_err(it, "NotSupportedError", NO_SUPPORTED_SOURCES));
        }
        let p = it.host_new_promise()?;
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
            queue(d, n, SELECT, 0.0)?;
        }
        if was_paused {
            queue_event(d, n, PLAY)?;
            queue_event(d, n, WAITING)?;
        }
        if resume {
            queue(d, n, FAIL, 0.0)?;
        }
        with(d, n, |m| m.pending.push(p));
        Ok(p)
    })();
    match r {
        Err(Error::Throw(v)) => {
            let p = it.host_new_promise()?;
            it.host_reject_promise(p, v)?;
            Ok(p)
        }
        r => r,
    }
}

/// `new Audio(src)`: an `audio` with `preload="auto"`, and `src` when the argument is not `undefined`.
fn audio_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let src = match arg(a, 0) {
        Value::Undefined => None,
        v => Some(d.string_arg(it, v)?),
    };
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, 0)?;
        if doc.find_atom("audio").is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim("dom-names"));
        }
        doc.create_element("audio")
    };
    let w = d.wrap(it, n)?;
    attr_set(d, it, n, "preload", "auto")?;
    if let Some(s) = src {
        attr_set(d, it, n, "src", &s)?;
        load_algorithm(d, it, n)?;
    }
    Ok(w)
}

// ------------------------------------------------------------------------------------------ tasks

impl Dom {
    /// Does anything listen for `ty` at `n`, on the way up, or at the window?
    fn media_heard(&self, it: &mut Interp<'_>, n: NodeId, ty: &str) -> R<bool> {
        let (doc, st) = (self.doc.borrow(), self.st.borrow());
        let on = |t: Target| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
        let mut cur = n;
        let mut found = on(Target::Node(cur));
        while let Some(p) = doc.parent(cur) {
            it.charge(1)?;
            cur = p;
            found |= on(Target::Node(cur));
        }
        Ok(found || on(Target::Window))
    }

    /// Fire the event `ev` at `n`: trusted, no bubbling, cancelable (Chrome 155 makes them so).
    fn media_fire(&self, it: &mut Interp<'_>, n: NodeId, ev: u8) -> R<()> {
        let ty = EVENTS[ev as usize];
        if !self.media_heard(it, n, ty)? {
            return Ok(());
        }
        let e = self.new_event(it, ty, false, true, false, None)?;
        self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), e).map(|_| ())
    }

    /// Run a queued step. A step of an older generation does nothing.
    pub(crate) fn media_task(&self, it: &mut Interp<'_>, n: NodeId, epoch: u32, step: u8) -> R<()> {
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
fn select(d: &Dom, n: NodeId) -> R<()> {
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
fn start(d: &Dom, it: &mut Interp<'_>, n: NodeId, epoch: u32) -> R<()> {
    d.media_fire(it, n, LOADSTART)?;
    let idle = with(d, n, |m| m.suspended);
    // A handler of `loadstart` may have run the load algorithm: the failure belongs to the old load.
    if with(d, n, |m| m.epoch) == epoch {
        queue(d, n, if idle { SUSPEND } else { FAIL }, 0.0)?;
    }
    Ok(())
}

/// The load failed: `error`, then the promises of `play()` reject. The list is taken before the event, so a `pause()` in a handler finds none.
fn fail(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<()> {
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

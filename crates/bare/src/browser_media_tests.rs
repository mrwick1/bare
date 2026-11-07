//! Tests of `browser_media.rs` (`HTMLMediaElement`, `HTMLAudioElement`, `HTMLVideoElement`, `Audio`, `TimeRanges`, `MediaError`). The Chrome
//! cases come first (`tools/fixtures/ui/media-cases.txt`, answers in `chrome-media.txt`, made with `BARE_MEDIA=1 node tools/ui/chrome_topic.mjs media`).
//! The tests below cover what Chrome cannot show: the deliberate differences (`canPlayType`, no download), the lazy globals, the parser, the collector
//! and the limits.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(media_cases, "media", 77);

fn browser(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><html><body>{body}</body></html>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn can_play_type_says_no_for_everything() {
    // Chrome says "probably" or "maybe" for mp3, wav and ogg. This engine has no decoder, so it never says so.
    let mut b = browser("");
    let r = js(&mut b, "var a = new Audio(); ['audio/mpeg', 'audio/wav', 'audio/ogg; codecs=vorbis', 'video/mp4', 'video/webm; codecs=\"vp8\"', '', 'x/y'].map(function (t) { return JSON.stringify(a.canPlayType(t)) }).join() + document.createElement('video').canPlayType('video/mp4')");
    assert_eq!(r, "\"\",\"\",\"\",\"\",\"\",\"\",\"\"");
}

#[test]
fn a_source_is_never_downloaded() {
    // Chrome requests the media URL (a range request). This engine does not: the load fails at once, as for a format with no decoder.
    let mut b = browser("");
    js(&mut b, "var a = new Audio('http://example.com/a.mp3'); var p = a.play(); p.catch(function () {}); a.src = 'http://example.com/b.mp3'; document.createElement('video').src = 'http://example.com/c.mp4'; 1");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(b.dom.fetch_queued(), 0);
    assert_eq!(js(&mut b, "[a.error.code, a.networkState].join()"), "4,3");
}

#[test]
fn a_page_without_media_pays_nothing() {
    let mut b = browser("<div id=d></div>");
    assert_eq!(js(&mut b, "document.getElementById('d').tagName"), "DIV");
    assert!(!b.dom.media_made());
    // The accessors stay until the first read of a global of the group.
    assert_eq!(js(&mut b, "typeof HTMLMediaElement"), "function");
    assert!(b.dom.media_made());
}

#[test]
fn the_first_media_element_makes_the_interfaces() {
    let mut b = browser("<audio id=a></audio>");
    assert!(!b.dom.media_made());
    assert_eq!(js(&mut b, "Object.getPrototypeOf(document.getElementById('a')) === HTMLAudioElement.prototype"), "true");
    let mut b = browser("<audio id=a></audio>");
    assert_eq!(js(&mut b, "document.getElementById('a').volume"), "1");
    assert!(b.dom.media_made());
}

#[test]
fn the_parser_makes_media_elements_with_their_attributes() {
    let mut b = browser("<video id=v muted autoplay loop controls poster=http://e.com/p.png width=20 height=10 playsinline></video><audio id=a></audio>");
    assert_eq!(js(&mut b, "var v = document.getElementById('v'); [v.muted, v.defaultMuted, v.autoplay, v.loop, v.controls, v.poster, v.width, v.height, v.playsInline, v.videoWidth, v.paused].join()"), "true,true,true,true,true,http://e.com/p.png,20,10,true,0,true");
    assert_eq!(js(&mut b, "Object.getPrototypeOf(document.getElementById('a')) === HTMLAudioElement.prototype"), "true");
}

#[test]
fn a_parsed_source_loads_when_the_page_calls_load_or_play() {
    // Chrome loads a parsed `<audio src>` at once. This engine has no hook for the parser: the element stays empty until `load()` or `play()`.
    let mut b = browser("<audio id=a src='http://127.0.0.1:1/x.mp3'></audio>");
    assert_eq!(js(&mut b, "var a = document.getElementById('a'), log = []; ['loadstart', 'error', 'emptied'].forEach(function (t) { a.addEventListener(t, function () { log.push(t) }) }); [a.networkState, a.error, a.currentSrc].join()"), "0,,");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "log.join()"), "");
    js(&mut b, "a.load(); 1");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "[log.join(), a.networkState, a.error.code, a.currentSrc].join()"), "loadstart,error,3,4,http://127.0.0.1:1/x.mp3");
}

#[test]
fn an_element_that_a_page_never_touches_does_nothing() {
    let mut b = browser("<audio src='http://127.0.0.1:1/x.mp3'></audio><video src='http://127.0.0.1:1/y.mp4'></video>");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(b.dom.fetch_queued(), 0);
}

#[test]
fn chrome_has_no_fast_seek_and_no_audio_tracks() {
    let mut b = browser("");
    assert_eq!(js(&mut b, "var a = new Audio(); [typeof a.fastSeek, 'audioTracks' in a, 'videoTracks' in a, typeof HTMLMediaElement.prototype.getStartDate].join()"), "undefined,false,false,undefined");
}

#[test]
fn a_pending_play_promise_survives_a_collection() {
    let mut b = browser("");
    js(&mut b, "var out = []; (function () { var a = new Audio(); a.play().then(function () { out.push('ok') }, function (e) { out.push(e.name) }); })(); 1");
    b.collect(&[]).expect("collect");
    b.advance_time(400.0, 10_000).unwrap();
    // No source: the promise stays pending, as in Chrome, and the element is still alive.
    assert_eq!(js(&mut b, "out.join()"), "");
    js(&mut b, "var keep = new Audio('http://127.0.0.1:1/x.mp3'); keep.play().catch(function (e) { out.push(e.name) }); keep = null; 1");
    b.collect(&[]).expect("collect");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "NotSupportedError");
}

#[test]
fn a_collected_error_object_is_not_lost() {
    let mut b = browser("");
    js(&mut b, "var a = new Audio('http://127.0.0.1:1/x.mp3'); 1");
    b.advance_time(400.0, 10_000).unwrap();
    js(&mut b, "var e = a.error; 1");
    b.collect(&[]).expect("collect");
    assert_eq!(js(&mut b, "[e === a.error, e.code, e.message].join()"), "true,4,MEDIA_ELEMENT_ERROR: Format error");
}

#[test]
fn many_queued_events_hit_a_limit_not_the_heap() {
    let mut b = browser("");
    let r = js(&mut b, "var a = new Audio(); for (var i = 0; i < 100000; i++) { a.volume = (i % 2) / 2 } 1");
    assert!(r.starts_with("limit:") && r.contains("media-tasks"), "{r}");
}

#[test]
fn a_media_event_after_the_element_is_gone_from_the_page_is_harmless() {
    let mut b = browser("<div id=d></div>");
    js(&mut b, "var d = document.getElementById('d'), a = new Audio(); d.appendChild(a); a.src = 'http://127.0.0.1:1/x.mp3'; d.removeChild(a); a.volume = 0.5; 1");
    b.advance_time(400.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "a.error.code"), "4");
}

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

#[test]
fn caption_action_uses_the_track_element_receiver() {
    let mut b = browser("<video id=v><track id=t kind=captions srclang=en default></video>");
    assert_eq!(js(&mut b, r#"var e=document.getElementById('t'),v=document.getElementById('v');
function w(e){let t=e.track.cues;return e.track.cues?r(t):e.addEventListener('load',n,{once:true}),{destroy:()=>e.removeEventListener('load',n)};function n(e){let t=e.target;r(t.track.cues)}function r(e){for(let t=0;t<e.length;t++)e[t].line=-2,e[t].size=80}}
w(e);v.textTracks[0].mode='showing';'continued'"#), "continued");
}

#[test]
fn caption_resource_runs_the_retained_load_callback() {
    let mut b = browser("<video id=v><track id=t kind=captions srclang=en default></video>");
    assert_eq!(js(&mut b, r#"var e=document.getElementById('t'),v=document.getElementById('v');
function w(e){let t=e.track.cues;return e.track.cues?r(t):e.addEventListener('load',n,{once:true}),{destroy:()=>e.removeEventListener('load',n)};function n(e){let t=e.target;r(t.track.cues)}function r(e){for(let t=0;t<e.length;t++)e[t].line=-2,e[t].size=80}}
w(e);v.textTracks[0].mode='showing';'continued'"#), "continued");
    const CAPTIONS: &str = r#"WEBVTT

00:00:00.000 --> 00:00:01.920
 [Scott Tolinski] I wanted to share it immediately.

00:00:01.920 --> 00:00:04.240
 I wanted to build more things in it.

00:00:04.240 --> 00:00:05.640
 [James Q Quick] I love it.

00:00:05.640 --> 00:00:07.080
 [Fireship] It's the only JavaScript framework

00:00:07.080 --> 00:00:08.640
 that's actually enjoyable to use.

00:00:08.640 --> 00:00:10.540
 [Ben McCann] Svelte introduced a new ergonomics

00:00:10.540 --> 00:00:13.160
 to building a website that's made it more fun

00:00:13.160 --> 00:00:15.440
 and easier than it's been in a long time.

00:00:15.440 --> 00:00:18.720
 And the way that it's done that is by introducing

00:00:18.720 --> 00:00:22.360
 this concept of generating reactive code at compile time.

00:00:22.360 --> 00:00:25.560
 [Scott Tolinski] What Svelte allows us to do is to write code

00:00:25.560 --> 00:00:29.400
 that's not only small and simple, but plain and readable.

00:00:29.400 --> 00:00:32.760
 [Amelia Wattenberger] You're mostly just looking at basic HTML,

00:00:32.760 --> 00:00:34.760
 JavaScript, and CSS.

00:00:34.760 --> 00:00:37.120
 [Aliza Aufrichtig] The thing I like most about using Svelte

00:00:37.120 --> 00:00:39.200
 is how fast you can move.

00:00:39.200 --> 00:00:42.400
 [Moritz Stefaner] And when I discovered Svelte, it came back to this,

00:00:42.400 --> 00:00:44.800
 oh, here's a really simple way to do things.

00:00:44.800 --> 00:00:47.480
 [Shawn Wang] You don't actually need all this machinery

00:00:47.480 --> 00:00:49.120
 in order to do components right.

00:00:49.120 --> 00:00:51.640
 I looked at my React experience so far —

00:00:51.640 --> 00:00:55.120
 I could not be anywhere as productive as I could be in Svelte.

00:00:55.120 --> 00:00:57.520
 And that's when I started really taking it seriously.

00:00:57.520 --> 00:00:59.800
 [Amelia Wattenberger] There's quite a bit of usage these days

00:00:59.800 --> 00:01:02.680
 and satisfaction is still, I think, the highest

00:01:02.680 --> 00:01:05.920
 or one of the highest for any of the larger

00:01:05.920 --> 00:01:08.120
 or more used JavaScript frameworks.

00:01:08.120 --> 00:01:10.040
 [Guillermo Rauch] When you write your application,

00:01:10.040 --> 00:01:13.160
 you're making it fast by construction.

00:01:13.160 --> 00:01:14.880
 It's almost impossible to create a slow app

00:01:14.880 --> 00:01:17.200
 and I think Svelte has an incredible advantage here.

00:01:17.200 --> 00:01:18.360
 People love Svelte.

"#;
    assert_eq!(js(&mut b, &format!("e.src='data:text/vtt,'+encodeURIComponent({CAPTIONS:?});[e.track.cues,e.readyState].join('|')")), "|1");
    b.collect(&[]).unwrap();
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "[e.readyState,e.track===v.textTracks[0],e.track.cues.length,e.track.cues[0].endTime,e.track.cues[0].line,e.track.cues[31].size].join('|')"), "2|true|32|1.92|-2|80");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "e.track.cues[0].line"), "-2");
    assert_eq!(js(&mut b, "try{e.track.cues[0].size=101}catch(x){x.name}"), "IndexSizeError");
    assert_eq!(js(&mut b, "e.track.cues[0].size"), "80");
    assert_eq!(js(&mut b, "try{Object.getOwnPropertyDescriptor(e.track,'mode').get.call({})}catch(x){x.name}"), "TypeError");
    js(&mut b, "var old=e.track.cues[0];e.src='data:text/vtt,WEBVTT';1");
    b.advance_time(100.0, 10_000).unwrap();
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "[old.line,old.size,e.track.cues.length].join('|')"), "-2|80|0");
}

#[test]
fn caption_list_is_live_and_preserves_track_identity() {
    let mut b = browser("<video id=v><track id=t></video>");
    assert_eq!(js(&mut b, "var v=document.getElementById('v'),e=document.getElementById('t'),l=v.textTracks,t=e.track;[l===v.textTracks,l.length,l[0]===t,t.cues===null].join('|')"), "true|1|true|true");
    assert_eq!(js(&mut b, "e.remove();[l.length,l[0]].join('|')"), "0|");
    assert_eq!(js(&mut b, "v.appendChild(e);[l.length,l[0]===t].join('|')"), "1|true");
}

#[test]
fn caption_source_replacement_discards_the_old_task() {
    let mut b = browser("<video><track id=t default></video>");
    js(&mut b, "var e=document.getElementById('t'),log=[];e.track.mode='hidden';e.addEventListener('load',()=>log.push('load'));e.addEventListener('error',()=>log.push('error'));e.src='data:text/vtt,WEBVTT%0A%0A00:00.000%20-->%2000:01.000%0Aold';e.src='data:text/vtt,invalid';1");
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "[log.join(),e.readyState,e.track.cues===null].join('|')"), "error|3|true");
}

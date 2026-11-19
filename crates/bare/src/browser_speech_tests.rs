//! Tests of `speechSynthesis` (`shims/speech.js`): a speech API with no voices, as headless Chrome gives.

use crate::browser_fetch_tests::{reply, serve};
use crate::browser_xhr_tests::{browser, js, settle};

fn run(src: &str) -> String {
    let srv = serve(|_| reply(404, &[], b""));
    let mut b = browser(srv.port);
    b.set_document_url(&format!("http://127.0.0.1:{}/page.html", srv.port)).unwrap();
    let r = js(&mut b, &format!("var out = [], finished = false; {src}"));
    assert!(!["throw", "other", "unsupp", "limit"].iter().any(|p| r.starts_with(p)), "{r}");
    settle(&mut b);
    js(&mut b, "out.join('|')")
}

#[test]
fn it_exists_with_no_voices() {
    let got = run("var s = window.speechSynthesis; out.push(s === speechSynthesis, 'speechSynthesis' in window, s instanceof SpeechSynthesis, s instanceof EventTarget,
        JSON.stringify(s.getVoices()), s.speaking, s.pending, s.paused, s.onvoiceschanged, String(s));
        try { new SpeechSynthesis(); } catch (e) { out.push(e.name); } finished = true");
    assert_eq!(got, "true|true|true|true|[]|false|false|false||[object SpeechSynthesis]|TypeError");
}

#[test]
fn speak_fails_with_an_error_event() {
    let got = run("var u = new SpeechSynthesisUtterance('hi'); out.push(u.text, u.lang, u.voice, u.volume, u.rate, u.pitch);
        speechSynthesis.onvoiceschanged = function () {};
        u.onerror = function (e) { out.push(e.type, e instanceof SpeechSynthesisErrorEvent, e instanceof SpeechSynthesisEvent, e.error, e.utterance === u); finished = true; };
        speechSynthesis.cancel(); speechSynthesis.pause(); speechSynthesis.resume(); speechSynthesis.speak(u);
        try { speechSynthesis.speak('hi'); } catch (e) { out.push(e.name); }");
    assert_eq!(got, "hi|||1|1|1|TypeError|error|true|true|synthesis-unavailable|true");
}

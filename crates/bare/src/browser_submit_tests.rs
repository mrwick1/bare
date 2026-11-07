//! Tests of form submission, constraint validation, `maxlength` and the history of documents (`browser_submit.rs`,
//! `browser_session.rs`, `page.rs`). Expectations come from the HTML Standard. The `GET` and `POST` loads run over a loopback server.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use crate::page::{Page, Source};
use crate::resources::test_server::{Reply, serve};
use bare_window::{Event, Mods};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(body: &str) -> Browser {
    let doc = tree::parse(&format!("<!doctype html><html><body>{body}</body></html>"));
    Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("error:{e:?}"),
    }
}

fn html(body: &str) -> Reply {
    Reply::new(200, Some("text/html; charset=utf-8"), body)
}

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };

fn press(p: &mut Page, key: &str, code: &'static str, text: Option<&str>, mods: Mods) {
    for pressed in [true, false] {
        p.handle(Event::Key { key: key.into(), code, pressed, repeat: false, text: text.filter(|_| pressed).map(str::to_string), mods });
    }
}

fn type_text(p: &mut Page, s: &str) {
    for c in s.chars() {
        let code: &'static str = Box::leak(format!("Key{}", c.to_ascii_uppercase()).into_boxed_str());
        press(p, &c.to_string(), code, Some(&c.to_string()), NO);
    }
}

fn click_at(p: &mut Page, x: f64, y: f64) {
    p.handle(Event::PointerMove { x, y, buttons: 0, mods: NO });
    for pressed in [true, false] {
        p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
    }
}

fn eval(p: &mut Page, src: &str) -> String {
    let b = p.browser().expect("browser");
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

/// Run the page until nothing is queued: a navigation that an input starts loads its document in later ticks (`SessionConfig::live`).
fn idle(p: &mut Page) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (ran, next) = p.tick();
        assert!(std::time::Instant::now() < end, "the page did not become idle");
        match next {
            None if !ran => return,
            Some(at) if !ran => std::thread::sleep(at.saturating_duration_since(std::time::Instant::now())),
            _ => {}
        }
    }
}

const FIELD: &str = "position:absolute;left:10px;top:10px;width:100px;height:20px";

#[test]
fn request_submit_validates_and_submit_does_not() {
    big(|| {
        let mut b = page("<form id=f action='/go?old=1#frag' method=get><input name=a value='x y'><input name=r required><button id=go name=b value=v>go</button></form><form id=g></form>");
        b.set_document_url("http://t.test/p").unwrap();
        js(&mut b, "var log = [], f = document.getElementById('f'); f.addEventListener('submit', function (e) { log.push('submit:' + (e.submitter && e.submitter.id)); e.preventDefault() });
            f.addEventListener('invalid', function (e) { log.push('invalid:' + e.target.name) }, true); 'x'");
        // `requestSubmit` validates: the empty required field is reported, nothing is submitted.
        assert_eq!(js(&mut b, "f.requestSubmit(); log.join()"), "invalid:r");
        assert_eq!(js(&mut b, "f.checkValidity() + ',' + f.reportValidity() + ',' + document.activeElement.name"), "false,false,r");
        // `submit()` neither validates nor fires the event; the navigation is a request with the query replaced and no fragment.
        js(&mut b, "log.length = 0; f.submit(); 'x'");
        assert_eq!(js(&mut b, "log.join()"), "");
        assert_eq!(b.take_navigation_request().unwrap().url, "http://t.test/go?a=x+y&r=");
        // A valid form with a submitter: the `submit` event has it, and its name and value are in the entry list.
        js(&mut b, "document.querySelector('[name=r]').value = 'ok'; log.length = 0; f.requestSubmit(document.getElementById('go')); 'x'");
        assert_eq!(js(&mut b, "log.join()"), "submit:go");
        js(&mut b, "f.removeEventListener('submit', f.__x); 'x'");
        assert!(js(&mut b, "try { f.requestSubmit(document.querySelector('[name=a]')) } catch (e) { e.name + ':' + e.message }").starts_with("TypeError:"));
        assert!(js(&mut b, "var g = document.getElementById('g'); try { g.requestSubmit(document.getElementById('go')) } catch (e) { e.name }").starts_with("NotFoundError"));
    });
}

#[test]
fn the_checks_of_the_standard() {
    big(|| {
        let cases = [
            ("<input required>", "", false),
            ("<input required value=x>", "", true),
            ("<input type=email value=a>", "", false),
            ("<input type=email value=a@b.co>", "", true),
            ("<input type=email>", "", true),
            ("<input type=url value=nope>", "", false),
            ("<input type=url value='https://a.test'>", "", true),
            // The value sanitization algorithm turns a markup value that is no number into the empty string.
            ("<input type=number value=abc>", "", true),
            ("<input type=number value=abc required>", "", false),
            ("<input type=number value=5 min=6>", "", false),
            ("<input type=number value=5 min=1 max=9>", "", true),
            ("<input type=checkbox required>", "", false),
            ("<input type=checkbox required checked>", "", true),
            ("<input type=radio name=r required><input type=radio name=r>", "", false),
            ("<input type=radio name=r required><input type=radio name=r checked>", "", true),
            ("<input required disabled>", "", true),
            ("<input required readonly>", "", true),
            ("<input type=hidden required>", "", true),
            // `minlength` counts only what a user typed: the markup value is not checked.
            ("<input minlength=5 value=abc>", "", true),
        ];
        for (html, _, ok) in cases {
            let mut b = page(&format!("<form id=f>{html}</form>"));
            assert_eq!(js(&mut b, "document.getElementById('f').checkValidity()"), ok.to_string(), "{html}");
        }
    });
}

#[test]
fn enter_in_a_field_submits_and_an_invalid_field_does_not() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => html(&format!("<form action=/s><input id=q name=q style='{FIELD}' required></form>")),
            p if p.starts_with("/s") => html("<p>found</p>"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut p = Page::open(&srv.url("/"), (400, 300)).expect("page");
        click_at(&mut p, 20.0, 20.0);
        // Empty and required: no request, and the field keeps the focus.
        press(&mut p, "Enter", "Enter", Some("\r"), NO);
        assert_eq!(srv.hits("/s?q="), 0);
        assert_eq!(eval(&mut p, "document.activeElement.id"), "q");
        type_text(&mut p, "hi");
        press(&mut p, "Enter", "Enter", Some("\r"), NO);
        idle(&mut p);
        assert_eq!(srv.hits("/s?q=hi"), 1);
        assert!(eval(&mut p, "location.href").ends_with("/s?q=hi"));
        assert!(p.paint().iter().any(|&c| c != 0xFFFF_FFFF), "the new document is drawn");
    });
}

#[test]
fn maxlength_cuts_typing() {
    big(|| {
        let html = format!("<!doctype html><form><input id=i maxlength=3 style='{FIELD}'></form><script>window.n = 0; document.getElementById('i').addEventListener('input', function () {{ n++ }})</script>");
        let mut p = page_in_window(&html);
        click_at(&mut p, 20.0, 20.0);
        type_text(&mut p, "abcde");
        assert_eq!(eval(&mut p, "document.getElementById('i').value + ',' + n"), "abc,3");
    });
}

#[test]
fn a_textarea_takes_typing_lines_maxlength_and_paints_its_value() {
    big(|| {
        let area = "position:absolute;left:10px;top:10px;width:150px;height:60px";
        let html = format!("<!doctype html><textarea id=t maxlength=6 style='{area}'>WWWWWWWW</textarea><script>window.log = []; var t = document.getElementById('t'); t.value = ''; ['beforeinput', 'input'].forEach(function (k) {{ t.addEventListener(k, function (e) {{ log.push(k + ':' + e.inputType + ':' + e.data) }}) }})</script>");
        let mut p = page_in_window(&html);
        // The dirty value is empty: nothing is drawn inside the box, not even the text children.
        let ink = |p: &mut Page| p.paint().chunks(400).skip(14).take(50).map(|r| r[14..150].iter().filter(|&&c| c & 0xFF < 0x80).count()).sum::<usize>();
        assert_eq!(ink(&mut p), 0, "the text children are not drawn");
        click_at(&mut p, 20.0, 20.0);
        type_text(&mut p, "ab");
        press(&mut p, "Enter", "Enter", None, NO);
        type_text(&mut p, "cdef");
        assert_eq!(eval(&mut p, "t.value + ',' + t.selectionStart"), "ab\ncde,6");
        assert_eq!(eval(&mut p, "log.slice(2, 6).join()"), "beforeinput:insertText:b,input:insertText:b,beforeinput:insertLineBreak:null,input:insertLineBreak:null");
        assert!(ink(&mut p) > 0, "the lines are drawn");
        // A click under the first line puts the caret on it; typing goes there.
        click_at(&mut p, 150.0, 22.0);
        eval(&mut p, "t.maxLength = 20; 0");
        type_text(&mut p, "X");
        assert_eq!(eval(&mut p, "t.value"), "abX\ncde");
    });
}

fn page_in_window(html: &str) -> Page {
    let script = html.split("<script>").nth(1).and_then(|s| s.split("</script>").next()).unwrap_or("");
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 300.0).unwrap();
    b.set_document_url("http://localhost/p").unwrap();
    if !script.is_empty() {
        b.run_script(script).expect("script");
    }
    Page::new(Source::Browser(Box::new(b)), (400, 300))
}

#[test]
fn a_form_post_sends_its_body_once_and_back_does_not_resend_it() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => html(&format!("<a id=l href=/b style='{FIELD}'>b</a><form action=/post method=post><input name=q value='x&y' style='position:absolute;left:10px;top:60px;width:100px;height:20px'><input type=submit id=s name=go value=ok style='position:absolute;left:10px;top:100px;width:100px;height:20px'></form>")),
            "/b" => html("<p>b</p>"),
            "/post" => html("<p>posted</p>"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut p = Page::open(&srv.url("/"), (400, 300)).expect("page");
        click_at(&mut p, 20.0, 110.0);
        idle(&mut p);
        let sent: Vec<String> = srv.heads.lock().unwrap().iter().filter(|h| h.starts_with("post /post")).cloned().collect();
        assert_eq!(sent.len(), 1, "one POST");
        assert!(sent[0].contains("content-type: application/x-www-form-urlencoded") && sent[0].ends_with("q=x%26y&go=ok"), "{}", sent[0]);
        assert!(eval(&mut p, "location.pathname").ends_with("/post"));
        // Back loads the first page again with a GET. Forward would resend the POST: nothing happens.
        press(&mut p, "ArrowLeft", "ArrowLeft", None, Mods { alt: true, ..NO });
        idle(&mut p);
        assert_eq!(eval(&mut p, "location.pathname"), "/");
        assert_eq!(srv.hits("/"), 2);
        press(&mut p, "ArrowRight", "ArrowRight", None, Mods { alt: true, ..NO });
        idle(&mut p);
        assert_eq!(eval(&mut p, "location.pathname"), "/");
        assert_eq!(srv.heads.lock().unwrap().iter().filter(|h| h.starts_with("post /post")).count(), 1, "the body is never sent twice");
    });
}

#[test]
fn back_and_forward_move_along_the_documents_and_restore_the_scroll() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => html("<a id=l href=/b style='position:fixed;left:0;top:0'>b</a><div style='height:2000px'></div>"),
            "/b" => html("<p>b</p>"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut p = Page::open(&srv.url("/"), (400, 300)).expect("page");
        p.handle(Event::Wheel { x: 100.0, y: 100.0, dx: 0.0, dy: 300.0, mods: NO });
        assert_eq!(eval(&mut p, "scrollY"), "300");
        click_at(&mut p, 5.0, 8.0); // the fixed link
        idle(&mut p);
        assert_eq!(eval(&mut p, "location.pathname"), "/b");
        // The back button of the mouse, then the forward one.
        for (button, want) in [(3u8, "/"), (4, "/b")] {
            for pressed in [true, false] {
                p.handle(Event::PointerButton { x: 5.0, y: 5.0, button, buttons: if pressed { 8 } else { 0 }, pressed, mods: NO });
            }
            idle(&mut p);
            assert_eq!(eval(&mut p, "location.pathname"), want);
        }
        p.handle(Event::Key { key: "ArrowLeft".into(), code: "ArrowLeft", pressed: true, repeat: false, text: None, mods: Mods { alt: true, ..NO } });
        idle(&mut p);
        assert_eq!(eval(&mut p, "location.pathname + ',' + scrollY"), "/,300", "the scroll of the first page came back");
        assert_eq!((srv.hits("/"), srv.hits("/b")), (3, 2));
        // Before the first document there is nothing: the page stays.
        press(&mut p, "ArrowLeft", "ArrowLeft", None, Mods { alt: true, ..NO });
        idle(&mut p);
        assert_eq!(eval(&mut p, "location.pathname"), "/");
    });
}

//! Tests of the user-like driver inputs (`browser_drive.rs`). The scenarios of `tools/fixtures/ui/drive-steps.txt` run on
//! `tools/fixtures/ui/drive.html` (a Radix-Select-like trigger and list, buttons, fields, a form) and must log what headless Chrome 155 logged
//! (`tools/fixtures/ui/chrome-drive.txt`, made by `tools/ui/chrome_drive.mjs`). The other tests pin what Chrome cannot show: the plumbing of
//! `Input`, and the value tracking of React.

use crate::browser::*;
use crate::browser_loader::{Fatal, Input, Stop};
use crate::browser_session::*;
use crate::resources::test_server::{Reply, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use std::time::Duration;

const PAGE: &str = include_str!("../../../tools/fixtures/ui/drive.html");
const STEPS: &str = include_str!("../../../tools/fixtures/ui/drive-steps.txt");
const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-drive.txt");
const REACT: &str = include_str!("../../../tools/fixtures/ui/react.html");
const REACT_STEPS: &str = include_str!("../../../tools/fixtures/ui/react-steps.txt");
const SEL: &str = include_str!("../../../tools/fixtures/ui/sel.html");
const SEL_STEPS: &str = include_str!("../../../tools/fixtures/ui/sel-steps.txt");
const SEL_CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-sel.txt");
const REACT_CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-react.txt");

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(html: &str) -> Browser {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.set_viewport(400.0, 800.0).unwrap();
    // The parser does not run scripts: run the one inline script of the page.
    if let Some((_, rest)) = html.split_once("<script>") {
        b.run_script(rest.split_once("</script>").unwrap().0).unwrap();
    }
    b
}

fn log(b: &mut Browser) -> String {
    let v = b.run_script("window.__log.join('\\n')").unwrap();
    b.it.display(v)
}

fn node(b: &mut Browser, sel: &str) -> bare_html::dom::NodeId {
    b.query(sel).unwrap().unwrap_or_else(|| panic!("no {sel}"))
}

/// The log of one scenario.
fn scenario(page: &str, steps: &[&str]) -> String {
    let mut b = browser(page);
    for step in steps {
        let mut p = step.splitn(3, ' ');
        let (verb, sel, text) = (p.next().unwrap(), p.next().unwrap_or(""), p.next().unwrap_or(""));
        match verb {
            "reset" => drop(b.run_script("window.__log.length = 0").unwrap()),
            "eval" => drop(b.run_script(text).unwrap()),
            "click" => {
                let n = node(&mut b, sel);
                b.drive_click(n).unwrap();
            }
            "key" => {
                let n = if sel == "-" { None } else { Some(node(&mut b, sel)) };
                b.drive_key(n, text).unwrap();
            }
            "type" => {
                let n = node(&mut b, sel);
                b.drive_type(n, &text.replace("\\n", "\n")).unwrap();
            }
            // Mouse steps on the text of a field (`tools/ui/chrome_drive.mjs`): the clock moves between the presses, as the time of a real hand does.
            "clickat" | "dblclick" | "tripleclick" => {
                let n = node(&mut b, sel);
                let at: u32 = text.parse().unwrap();
                for nth in 1..=match verb { "clickat" => 1, "dblclick" => 2, _ => 3 } {
                    b.drive_text_down(n, at, nth).unwrap();
                    b.advance_time(40.0, 10_000).unwrap();
                    b.drive_text_up(n, at).unwrap();
                    b.advance_time(40.0, 10_000).unwrap();
                }
            }
            "drag" => {
                let n = node(&mut b, sel);
                let (from, to) = text.split_once(' ').unwrap();
                b.drive_text_down(n, from.parse().unwrap(), 1).unwrap();
                b.advance_time(60.0, 10_000).unwrap();
                b.drive_text_move(n, to.parse().unwrap()).unwrap();
                b.advance_time(60.0, 10_000).unwrap();
                b.drive_text_up(n, to.parse().unwrap()).unwrap();
            }
            _ => panic!("bad step {step}"),
        }
        b.advance_time(300.0, 10_000).unwrap();
    }
    log(&mut b)
}

/// Run every scenario of `steps` on `page` and compare each log with the Chrome one. Gives the number of scenarios.
fn compare(page: &str, steps_src: &str, chrome_src: &str) -> usize {
    let mut chrome = std::collections::HashMap::new();
    let mut cur = String::new();
    for l in chrome_src.lines() {
        if let Some(n) = l.strip_prefix("== ") {
            cur = n.into();
            chrome.insert(cur.clone(), Vec::new());
        } else {
            chrome.get_mut(&cur).unwrap().push(l);
        }
    }
    let (mut names, mut steps): (Vec<&str>, Vec<Vec<&str>>) = (vec![], vec![]);
    for l in steps_src.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        if let Some(n) = l.strip_prefix("== ") {
            names.push(n);
            steps.push(vec![]);
        } else {
            steps.last_mut().unwrap().push(l);
        }
    }
    let mut bad = Vec::new();
    for (name, s) in names.iter().zip(&steps) {
        let got = scenario(page, s);
        let mut got: Vec<&str> = got.lines().collect();
        let mut want = chrome.get(*name).unwrap_or_else(|| panic!("no Chrome log for {name}")).clone();
        // A name that ends with `~` has events whose order Chrome does not fix (see `sel-steps.txt`).
        if name.ends_with('~') {
            got.sort();
            want.sort();
        }
        if got != want {
            let i = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
            bad.push(format!("{name}: line {i}\n  chrome {:?}\n  bare   {:?}\n  ({} chrome lines, {} bare lines)", want.get(i), got.get(i), want.len(), got.len()));
        }
    }
    assert!(bad.is_empty(), "{} of {} scenarios differ:\n{}", bad.len(), names.len(), bad.join("\n"));
    names.len()
}

#[test]
fn scenarios_log_what_chrome_logs() {
    big(|| assert_eq!(compare(PAGE, STEPS, CHROME), 25));
}

#[test]
fn typing_in_react_controlled_fields_gives_what_chrome_gives() {
    big(|| assert_eq!(compare(REACT, REACT_STEPS, REACT_CHROME), 23));
}

#[test]
fn selection_events_of_text_controls_give_what_chrome_gives() {
    big(|| assert_eq!(compare(SEL, SEL_STEPS, SEL_CHROME), 71));
}

// ------------------------------------------------------------------------------------------ the tracker of React

/// What React 19 does to a controlled field: it wraps the `value` property of the element to learn the last value that script set, and
/// calls `onChange` for an `input` event only when the value now differs from that one. A typed character must pass; a value that
/// script set (`el.value = v`) must not.
const TRACKED: &str = "<input id=i><textarea id=t></textarea><input id=e type=email><script>
    window.changes = [];
    function track(el) {
      var d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value'), last = '' + el.value;
      Object.defineProperty(el, 'value', { configurable: true, get: function () { return d.get.call(this) }, set: function (v) { last = '' + v; d.set.call(this, v) } });
      el.addEventListener('input', function () { var now = d.get.call(el); if (now !== last) { last = now; window.changes.push(el.id + ':' + now) } });
    }
    ['i', 't', 'e'].forEach(function (id) { track(document.getElementById(id)) });
    </script>";

#[test]
fn typed_text_reaches_a_react_style_tracker_one_character_at_a_time() {
    big(|| {
        let mut b = browser(TRACKED);
        for (sel, text) in [("#i", "abc"), ("#t", "x\ny"), ("#e", "a@b.c")] {
            let n = node(&mut b, sel);
            b.drive_type(n, text).unwrap();
        }
        let n = node(&mut b, "#i");
        b.drive_type(n, "z").unwrap();
        b.run_script("document.getElementById('i').value = 'set by script'").unwrap();
        let v = b.run_script("window.changes.join('|')").unwrap();
        assert_eq!(b.it.display(v), "i:a|i:ab|i:abc|t:x|t:x\n|t:x\ny|e:a|e:a@|e:a@b|e:a@b.|e:a@b.c|i:z");
        let v = b.run_script("document.getElementById('t').value + ',' + document.getElementById('e').value").unwrap();
        assert_eq!(b.it.display(v), "x\ny,a@b.c");
    });
}

#[test]
fn type_needs_an_editable_field_and_replaces_its_text() {
    big(|| {
        let mut b = browser("<p id=p>x</p><input id=r readonly value=ro><input id=c type=checkbox><input id=n type=number value=12>");
        for sel in ["#p", "#r", "#c"] {
            let n = node(&mut b, sel);
            assert!(matches!(b.drive_type(n, "a"), Err(bare_js::eval::Error::Unsupported(_))), "{sel}");
        }
        let n = node(&mut b, "#n");
        b.drive_type(n, "7").unwrap();
        let v = b.run_script("document.getElementById('n').value").unwrap();
        assert_eq!(b.it.display(v), "7", "a number field has no selection API, but typing still replaces its text");
        assert!(matches!(b.drive_key(None, "Nope"), Err(bare_js::eval::Error::Unsupported(_))));
    });
}

// ------------------------------------------------------------------------------------------ the session

fn cfg(url: String) -> SessionConfig {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

#[test]
fn input_key_and_click_act_like_a_user_in_a_session() {
    big(|| {
        let page = "<form id=f novalidate><input id=q><textarea id=t></textarea><button id=go>go</button></form><button id=b>b</button><script>
            var f = document.getElementById('f'), b = document.getElementById('b');
            f.addEventListener('submit', function (e) { e.preventDefault(); localStorage.setItem('sub', document.getElementById('q').value + '|' + document.getElementById('t').value) });
            b.addEventListener('pointerdown', function () { localStorage.setItem('down', 'yes') });
            document.addEventListener('keydown', function (e) { localStorage.setItem('key', e.key + ':' + e.target.id) });
            </script>";
        let srv = serve(move |_| Reply::new(200, Some("text/html; charset=utf-8"), page));
        let mut s = Session::new(cfg(srv.url("/")));
        s.run();
        let get = |s: &Session, k: &str| s.storage().get_item(false, &srv.url(""), k);
        // `Key` with no selector goes to the focused element, `Type` focuses first, and a newline in a textarea is a line break.
        let r = s.resume(&[Input::Type("#q", "hi"), Input::Key("", "Tab"), Input::Type("#t", "a\nb"), Input::Key("-", "Tab"), Input::Key("-", "Enter")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, "key").as_deref(), Some("Enter:go"));
        assert_eq!(get(&s, "sub").as_deref(), Some("hi|a\nb"), "Enter on the focused submit button submits the form");
        let r = s.resume(&[Input::Click("#b")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, "down").as_deref(), Some("yes"));
        let r = s.resume(&[Input::Key("#nothing", "a")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::NoTarget(0)));
        let r = s.resume(&[Input::Key("#b", "Hyper")]);
        assert!(matches!(r.stop, SessionStop::Page(Stop::Fatal(Fatal::Unsupported(_)))), "{r:?}");
    });
}

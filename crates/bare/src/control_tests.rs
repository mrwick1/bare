//! Tests of the control interface (`control.rs`). The scenarios of `tools/fixtures/ui/control-steps.txt` run on `drive.html` through
//! `Page::click`, `Page::press` and `Page::type_text` (the hit test of the window frame and `Page::handle`), and must log what headless
//! Chrome 155 logged for real mouse and key input (`tools/fixtures/ui/chrome-control.txt`, made by `tools/ui/chrome_drive.mjs`). The
//! refusals, the waits and the locators are tested with the fixture app in `crates/drive/tests`.

use crate::browser::*;
use crate::control::{Handle, Refusal, key_events};
use crate::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::Event;

const PAGE: &str = include_str!("../../../tools/fixtures/ui/drive.html");
const STEPS: &str = include_str!("../../../tools/fixtures/ui/control-steps.txt");
const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-control.txt");

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A page of `html` at 400 x `h` with a virtual clock; the one inline script runs (the parser does not run scripts).
fn page(html: &str, h: u32) -> Page {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.set_viewport(400.0, h as f32).unwrap();
    b.set_document_url("http://localhost/drive.html").unwrap();
    if let Some((_, rest)) = html.split_once("<script>") {
        b.run_script(rest.split_once("</script>").unwrap().0).unwrap();
    }
    let mut p = Page::new(Source::Browser(Box::new(b)), (400, h));
    p.quiet = true;
    p
}

fn one(p: &mut Page, sel: &str) -> Handle {
    match p.query_all(sel).unwrap().as_slice() {
        [h] => *h,
        l => panic!("{sel}: {} matches", l.len()),
    }
}

fn js(p: &mut Page, code: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(code).unwrap();
    b.it.display(v)
}

/// The log of one scenario.
fn scenario(steps: &[&str]) -> String {
    let mut p = page(PAGE, 800);
    for step in steps {
        let mut w = step.splitn(3, ' ');
        let (verb, sel, text) = (w.next().unwrap(), w.next().unwrap_or(""), w.next().unwrap_or(""));
        match verb {
            "reset" => drop(js(&mut p, "window.__log.length = 0")),
            "click" => {
                let h = one(&mut p, sel);
                p.click(h).unwrap_or_else(|r| panic!("{step}: {r:?}"));
            }
            "key" if sel == "-" => p.press(text).unwrap(),
            "fill" => {
                let h = one(&mut p, sel);
                p.type_text(h, &text.replace("\\n", "\n")).unwrap_or_else(|r| panic!("{step}: {r:?}"));
            }
            _ => panic!("bad step {step}"),
        }
        p.browser().unwrap().advance_time(300.0, 10_000).unwrap();
    }
    js(&mut p, "window.__log.join('\\n')")
}

#[test]
fn control_actions_log_what_chrome_logs() {
    big(|| {
        let mut chrome = std::collections::HashMap::new();
        let mut cur = String::new();
        for l in CHROME.lines() {
            if let Some(n) = l.strip_prefix("== ") {
                cur = n.into();
                chrome.insert(cur.clone(), Vec::new());
            } else {
                chrome.get_mut(&cur).unwrap().push(l);
            }
        }
        let (mut names, mut steps): (Vec<&str>, Vec<Vec<&str>>) = (vec![], vec![]);
        for l in STEPS.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            if let Some(n) = l.strip_prefix("== ") {
                names.push(n);
                steps.push(vec![]);
            } else {
                steps.last_mut().unwrap().push(l);
            }
        }
        let mut bad = Vec::new();
        for (name, s) in names.iter().zip(&steps) {
            let got = scenario(s);
            let got: Vec<&str> = got.lines().collect();
            let want = &chrome[*name];
            if got != *want {
                let i = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
                bad.push(format!("{name}: line {i}\n  chrome {:?}\n  bare   {:?}\n  ({} chrome lines, {} bare lines)", want.get(i), got.get(i), want.len(), got.len()));
            }
        }
        assert!(bad.is_empty(), "{} of {} scenarios differ:\n{}", bad.len(), names.len(), bad.join("\n"));
        assert_eq!(names.len(), 15);
    });
}

#[test]
fn refusals_send_no_event() {
    big(|| {
        let mut p = page(
            r#"<body style="margin:0"><button id=a style="position:absolute;top:0">A</button><div id=veil style="position:fixed;top:0;left:0;width:100px;height:40px"></div>
<button id=off style="position:absolute;top:100px" disabled>Off</button><button id=none style="display:none">N</button><span id=s style="position:absolute;top:150px">text</span>
<script>window.n = 0; addEventListener('pointerdown', function () { n++ }, true); addEventListener('keydown', function () { n++ }, true);</script></body>"#,
            300,
        );
        let (a, off, none, s) = (one(&mut p, "#a"), one(&mut p, "#off"), one(&mut p, "#none"), one(&mut p, "#s"));
        assert!(matches!(p.click(a), Err(Refusal::Covered { ref by, .. }) if by == "div#veil"));
        assert_eq!(p.click(off), Err(Refusal::Disabled));
        assert!(matches!(p.click(none), Err(Refusal::Hidden(_))));
        assert_eq!(p.type_text(s, "x"), Err(Refusal::NotEditable));
        drop(js(&mut p, "document.getElementById('veil').remove()"));
        p.touched();
        assert!(p.click(a).is_ok());
        assert_eq!(js(&mut p, "n"), "1");
        drop(js(&mut p, "document.getElementById('a').remove()"));
        p.touched();
        assert_eq!(p.click(a), Err(Refusal::Detached));
        assert_eq!(js(&mut p, "n"), "1");
    });
}

/// A wheel turn moves the boxes under a still pointer: `:hover` and the boundary events follow the new layout before the next action,
/// as Chrome's fake mouse move does.
#[test]
fn hover_follows_a_scroll_under_a_still_pointer() {
    big(|| {
        let mut p = page(
            r#"<body style="margin:0"><div id=top style="height:300px"></div><div id=h style="height:300px"></div><div style="height:2000px"></div>
<script>window.log = []; document.getElementById('h').addEventListener('mouseover', function () { log.push('over h') });
document.getElementById('top').addEventListener('mouseout', function () { log.push('out top') });</script></body>"#,
            300,
        );
        assert_eq!(p.wheel(None, 0.0, 0.0), Ok(false));
        assert_eq!(js(&mut p, "log.join()"), "");
        assert_eq!(p.wheel(None, 0.0, 200.0), Ok(true));
        assert_eq!(js(&mut p, "log.join()"), "out top,over h");
        assert_eq!(js(&mut p, "document.querySelector(':hover:not(html):not(body)').id"), "h");
    });
}

#[test]
fn key_specs_give_the_events_of_the_window() {
    let keys = |spec| key_events(spec).unwrap().into_iter().map(|e| match e {
        Event::Key { key, code, pressed, text, mods, .. } => format!("{}{key}/{code}/{}/{}", if pressed { "+" } else { "-" }, text.unwrap_or_default(), mods.ctrl as u8 + 2 * mods.shift as u8),
        _ => unreachable!(),
    }).collect::<Vec<_>>().join(" ");
    assert_eq!(keys("a"), "+a/KeyA/a/0 -a/KeyA//0");
    assert_eq!(keys("A"), "+A/KeyA/A/0 -A/KeyA//0");
    assert_eq!(keys("Control+a"), "+Control/ControlLeft//1 +a/KeyA//1 -a/KeyA//1 -Control/ControlLeft//0");
    assert_eq!(keys("Shift+Tab"), "+Shift/ShiftLeft//2 +Tab/Tab//2 -Tab/Tab//2 -Shift/ShiftLeft//0");
    assert_eq!(keys("Enter"), "+Enter/Enter//0 -Enter/Enter//0");
    assert_eq!(keys("+"), "++/Equal/+/0 -+/Equal//0");
    assert_eq!(keys(" "), "+ /Space/ /0 - /Space//0");
    assert!(key_events("Hyper+a").is_none() && key_events("Nope").is_none());
}

//! Tests of window input (`browser_ui.rs`, `page.rs`).
//!
//! The main test replays, through `Page::handle` (the window boundary), the same input that `tools/ui/chrome_record.mjs` sent to
//! headless Chrome, on the same page (`tools/fixtures/ui/events.html`), and compares the event log of each step with the log
//! that Chrome made (`tools/fixtures/ui/chrome-events.txt`). One line is `type @target` and the properties that the page logs.
//! Chrome is the oracle; bare is not its own.
//!
//! Left out of the comparison on purpose: `select` and `selectionchange`. Both are queued tasks in the browser, so where they fall
//! between the key events depends on task timing (Chrome itself puts `selectionchange` before `keyup` in one scenario and after
//! it in another). Their own tests are in `browser_selection_tests.rs`.

use crate::browser::*;
use crate::browser_ui::Mods;
use crate::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::Event;
use bare_window::Mods as WMods;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("../../../tools/fixtures/ui/events.html");
const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-events.txt");

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A page from `html`, with its `<script>` run, in a 400 by 400 window.
fn page_of(html: &str) -> Page {
    let script = html.split("<script>").nth(1).and_then(|s| s.split("</script>").next()).unwrap_or("");
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 400.0).expect("viewport");
    b.set_document_url("http://localhost/events.html").expect("url");
    if !script.is_empty() {
        b.run_script(script).expect("script");
    }
    Page::new(Source::Browser(Box::new(b)), (400, 400))
}

fn chrome() -> BTreeMap<&'static str, Vec<&'static str>> {
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut cur = "";
    for l in CHROME.lines() {
        match l.strip_prefix("== ") {
            Some(name) => {
                cur = name;
                out.entry(cur).or_default();
            }
            None => out.get_mut(cur).expect("a header comes first").push(l),
        }
    }
    out
}

const NO: WMods = WMods { shift: false, ctrl: false, alt: false, meta: false };

struct Driver {
    page: Page,
    buttons: u16,
}

impl Driver {
    fn new(html: &str) -> Driver {
        Driver { page: page_of(html), buttons: 0 }
    }

    fn mv(&mut self, (x, y): (f64, f64)) {
        self.page.handle(Event::PointerMove { x, y, buttons: self.buttons, mods: NO });
    }

    fn button(&mut self, (x, y): (f64, f64), pressed: bool) {
        self.buttons = if pressed { 1 } else { 0 };
        self.page.handle(Event::PointerButton { x, y, button: 0, buttons: self.buttons, pressed, mods: NO });
    }

    fn click(&mut self, at: (f64, f64)) {
        self.mv(at);
        self.button(at, true);
        self.button(at, false);
    }

    /// The key as the recorder sends it: a name of the table below, or one character.
    fn press(&mut self, name: &str, m: WMods) {
        let (key, code, text): (String, &'static str, Option<String>) = match name {
            "Backspace" | "Delete" | "Tab" | "ArrowLeft" | "ArrowRight" | "Home" | "End" => (name.into(), Box::leak(name.to_string().into_boxed_str()), None),
            "Enter" => ("Enter".into(), "Enter", Some("\r".into())),
            " " => (" ".into(), "Space", Some(" ".into())),
            c => {
                let code: &'static str = Box::leak(format!("Key{}", c.to_uppercase()).into_boxed_str());
                (c.into(), code, (!m.ctrl).then(|| c.to_string()))
            }
        };
        for pressed in [true, false] {
            self.page.handle(Event::Key { key: key.clone(), code, pressed, repeat: false, text: text.clone().filter(|_| pressed), mods: m });
        }
    }

    /// The log since the last call, without `select` and `selectionchange`.
    fn take(&mut self) -> Vec<String> {
        let b = self.page.browser().expect("browser");
        // The tasks that the input queued run now, as they would before the next event.
        b.run_scheduled_tasks(100).expect("tasks");
        let v = b.run_script("var l = window.__log.join('\\n'); window.__log.length = 0; l").expect("log");
        let s = b.it.display(v);
        s.lines().filter(|l| !(l.starts_with("select ") || l.starts_with("selectionchange "))).map(str::to_string).collect()
    }

    fn reset(&mut self) {
        self.take();
    }
}

const AT_LINK: (f64, f64) = (50.0, 25.0);
const AT_USER: (f64, f64) = (50.0, 75.0);
const AT_CHK: (f64, f64) = (20.0, 170.0);
const AT_GO: (f64, f64) = (50.0, 215.0);
const AT_PLAIN: (f64, f64) = (50.0, 265.0);
const AT_NONE: (f64, f64) = (350.0, 350.0);
const SHIFT: WMods = WMods { shift: true, ..NO };
const CTRL: WMods = WMods { ctrl: true, ..NO };

fn short(l: &str) -> &str {
    l.split(" bubbles=").next().unwrap_or(l)
}

/// Compare one scenario. A line that differs is shown with the line of Chrome next to it.
fn same(name: &str, got: &[String], want: &BTreeMap<&'static str, Vec<&'static str>>) -> Option<String> {
    let want: Vec<&str> = want[name].iter().copied().filter(|l| !(l.starts_with("select ") || l.starts_with("selectionchange "))).collect();
    if got.len() == want.len() && got.iter().zip(&want).all(|(g, w)| g == w) {
        return None;
    }
    let mut out = format!("scenario {name}:\n");
    for i in 0..got.len().max(want.len()) {
        let (g, w) = (got.get(i).map_or("-", |s| s.as_str()), want.get(i).copied().unwrap_or("-"));
        out.push_str(&format!("{} bare   {}\n  chrome {}\n", if g == w { " " } else { "!" }, short(g), short(w)));
    }
    Some(out)
}

#[test]
fn events_match_chrome() {
    big(|| {
        let want = chrome();
        let mut d = Driver::new(FIXTURE);
        let mut bad = Vec::new();
        let mut check = |d: &mut Driver, name: &str| {
            let got = d.take();
            if let Some(m) = same(name, &got, &want) {
                bad.push(m);
            }
        };
        d.click(AT_NONE);
        d.reset();
        d.mv(AT_PLAIN);
        check(&mut d, "move_into_button");
        d.reset();
        d.button(AT_PLAIN, true);
        d.button(AT_PLAIN, false);
        check(&mut d, "click_plain_button");
        d.reset();
        d.mv(AT_LINK);
        check(&mut d, "move_button_to_link");
        d.reset();
        d.button(AT_LINK, true);
        d.button(AT_LINK, false);
        check(&mut d, "click_link");
        d.reset();
        d.mv(AT_USER);
        d.reset();
        d.button(AT_USER, true);
        d.button(AT_USER, false);
        check(&mut d, "click_text_input_focuses");
        d.reset();
        d.press("a", NO);
        check(&mut d, "type_a");
        d.reset();
        d.press("b", NO);
        d.reset();
        d.press("Backspace", NO);
        check(&mut d, "backspace");
        d.reset();
        d.press("ArrowLeft", NO);
        check(&mut d, "arrow_left");
        d.reset();
        d.press("Delete", NO);
        check(&mut d, "delete");
        d.reset();
        d.press("Home", NO);
        check(&mut d, "home");
        d.reset();
        d.press("Tab", NO);
        check(&mut d, "tab_to_password");
        d.reset();
        d.press("Tab", SHIFT);
        check(&mut d, "shift_tab_back");
        d.reset();
        d.press("Tab", NO);
        d.reset();
        d.press("x", NO);
        d.reset();
        d.press("Enter", NO);
        check(&mut d, "enter_in_password_submits");
        d.reset();
        d.mv(AT_CHK);
        d.reset();
        d.click(AT_CHK);
        check(&mut d, "click_checkbox");
        d.reset();
        d.mv(AT_GO);
        d.reset();
        d.click(AT_GO);
        check(&mut d, "click_submit_button");
        d.reset();
        d.mv(AT_LINK);
        d.mv(AT_NONE);
        check(&mut d, "move_link_to_blank");
        d.reset();
        d.click(AT_USER);
        d.reset();
        d.press("q", NO);
        d.reset();
        d.click(AT_PLAIN);
        check(&mut d, "edit_then_click_away");
        d.reset();
        d.click(AT_USER);
        d.reset();
        d.press("A", SHIFT);
        check(&mut d, "type_capital_with_shift");
        d.reset();
        d.press("ArrowLeft", SHIFT);
        check(&mut d, "shift_arrow_left");
        d.reset();
        d.press("a", CTRL);
        check(&mut d, "ctrl_a_select_all");
        d.reset();
        d.press("ArrowRight", NO);
        check(&mut d, "arrow_right_collapses_selection");
        d.reset();
        d.press("Tab", NO);
        d.press("Tab", NO);
        d.reset();
        d.press(" ", NO);
        check(&mut d, "space_on_checkbox");
        d.reset();
        d.press("Tab", NO);
        d.reset();
        d.press("Enter", NO);
        check(&mut d, "enter_on_submit_button");
        // The end state is Chrome's: the user field "qA", the password "x", the checkbox cleared, focus at the button.
        let b = d.page.browser().expect("browser");
        let user = b.query("#user").unwrap().unwrap();
        let pass = b.query("#pass").unwrap().unwrap();
        let chk = b.query("#chk").unwrap().unwrap();
        assert_eq!((b.input_value(user).as_deref(), b.input_value(pass).as_deref()), (Some("qA"), Some("x")));
        let go = b.query("#go").unwrap().unwrap();
        assert_eq!(b.active_element().unwrap(), Some(go));
        let _ = chk;
        let v = b.run_script("document.getElementById('chk').checked").unwrap();
        assert_eq!(b.it.display(v), "false");
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

#[test]
fn mods_default_is_all_off() {
    assert_eq!(Mods::default(), Mods { shift: false, ctrl: false, alt: false, meta: false });
}

// ------------------------------------------------------------------------------------------ other behavior

const FORM: &str = r#"<!DOCTYPE html><html><head><style>
body { margin: 0; font-size: 16px; }
.c { position: absolute; left: 10px; width: 200px; height: 30px; box-sizing: border-box; margin: 0; padding: 0; border: 1px solid #888; }
</style></head><body>
<form id="f" novalidate>
<input id="a" class="c" style="top:10px" type="text" value="">
<input id="b" class="c" style="top:50px" type="password" value="">
<input id="chk" class="c" style="top:90px;width:20px" type="checkbox">
<input id="r1" class="c" style="top:130px;width:20px" type="radio" name="g" checked>
<input id="r2" class="c" style="top:170px;width:20px" type="radio" name="g">
</form>
<a id="lk" class="c" style="top:210px;display:block" href="/other">link</a>
<label id="lb" for="chk" class="c" style="top:250px;display:block">label</label>
</body></html>"#;

const Y: [f64; 7] = [25.0, 65.0, 105.0, 145.0, 185.0, 225.0, 265.0];

impl Driver {
    fn js(&mut self, src: &str) -> String {
        let b = self.page.browser().expect("browser");
        let v = b.run_script(src).expect("script");
        b.it.display(v)
    }

    fn type_chars(&mut self, text: &str) {
        for c in text.chars() {
            let key = c.to_string();
            for pressed in [true, false] {
                self.page.handle(Event::Key { key: key.clone(), code: "", pressed, repeat: false, text: pressed.then(|| key.clone()), mods: NO });
            }
        }
    }

    fn value(&mut self, id: &str) -> String {
        self.js(&format!("document.getElementById('{id}').value"))
    }

    fn selection(&mut self, id: &str) -> String {
        self.js(&format!("var e = document.getElementById('{id}'); e.selectionStart + ',' + e.selectionEnd"))
    }
}

#[test]
fn tab_order_follows_tabindex_then_tree_order() {
    big(|| {
        let mut d = Driver::new(
            r##"<body><a id="l1" href="#">x</a><input id="i2" tabindex="2"><input id="i1" tabindex="1"><button id="b0">b</button>
<input id="neg" tabindex="-1"><input id="dis" disabled><input id="hid" type="hidden"><div id="dv" tabindex="0">d</div><a id="nohref">n</a>
<div hidden><input id="gone"></div><input id="i3" tabindex="1"></body>"##,
        );
        let b = d.page.browser().unwrap();
        let order = b.tab_order().unwrap();
        let ids = ["i1", "i3", "i2", "l1", "b0", "dv"].map(|i| b.query(&format!("#{i}")).unwrap().unwrap());
        assert_eq!(order, ids);
        // Tab walks the order and wraps; Shift+Tab walks it back and wraps.
        let mut seen = Vec::new();
        for _ in 0..7 {
            d.press("Tab", NO);
            seen.push(d.js("document.activeElement.id"));
        }
        assert_eq!(seen, ["i1", "i3", "i2", "l1", "b0", "dv", "i1"]);
        d.press("Tab", SHIFT);
        assert_eq!(d.js("document.activeElement.id"), "dv");
    });
}

#[test]
fn typing_edits_the_value_at_the_caret() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[0]));
        d.type_chars("h\u{e9}llo\u{1f600}");
        assert_eq!(d.value("a"), "h\u{e9}llo\u{1f600}");
        // Backspace removes the whole emoji (two UTF-16 units).
        d.press("Backspace", NO);
        assert_eq!(d.value("a"), "h\u{e9}llo");
        d.press("Home", NO);
        d.press("Delete", NO);
        assert_eq!(d.value("a"), "\u{e9}llo");
        d.press("ArrowRight", NO);
        d.type_chars("X");
        assert_eq!(d.value("a"), "\u{e9}Xllo");
        assert_eq!(d.selection("a"), "2,2");
        // Shift+Right twice selects "ll"; typing replaces the selection.
        d.press("ArrowRight", SHIFT);
        d.press("ArrowRight", SHIFT);
        assert_eq!(d.selection("a"), "2,4");
        d.type_chars("-");
        assert_eq!(d.value("a"), "\u{e9}X-o");
        d.press("End", SHIFT);
        assert_eq!(d.selection("a"), "3,4");
        d.press("a", CTRL);
        assert_eq!(d.selection("a"), "0,4");
        d.press("Backspace", NO);
        assert_eq!(d.value("a"), "");
    });
}

#[test]
fn a_press_puts_the_caret_where_the_text_is() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[0]));
        d.type_chars("abcdef");
        // The style sheet gives a field a font of 13.3333px, as Chrome does: a letter is 8px wide. The field starts at
        // left 10 + border 1 = 11.
        d.click((11.0 + 8.0 * 2.0 + 1.0, Y[0]));
        assert_eq!(d.selection("a"), "2,2");
        d.click((11.0 + 8.0 * 4.0 - 1.0, Y[0]));
        assert_eq!(d.selection("a"), "4,4");
        d.click((300.0 - 10.0, Y[0]));
        assert_eq!(d.selection("a"), "4,4", "a press outside the field does not move the caret");
    });
}

#[test]
fn a_listener_can_cancel_each_step_of_typing() {
    big(|| {
        for (cancel, typed) in [("keydown", ""), ("keypress", ""), ("beforeinput", ""), ("input", "z")] {
            let mut d = Driver::new(FORM);
            d.js(&format!("document.addEventListener('{cancel}', function (e) {{ e.preventDefault(); }}, true)"));
            d.click((50.0, Y[0]));
            d.type_chars("z");
            // `input` is not cancelable: the value changes. The other three stop the edit.
            assert_eq!(d.value("a"), typed, "{cancel}");
        }
    });
}

#[test]
fn a_canceled_click_leaves_a_checkbox_as_it_was() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.js("window.log = []; var c = document.getElementById('chk'); c.addEventListener('click', function (e) { window.log.push('click:' + c.checked); }); c.addEventListener('input', function () { window.log.push('input'); }); c.addEventListener('change', function () { window.log.push('change'); });");
        d.click((20.0, Y[2]));
        // A listener of `click` sees the new state; `input` and `change` follow.
        assert_eq!(d.js("window.log.join()"), "click:true,input,change");
        assert_eq!(d.js("document.getElementById('chk').checked"), "true");
        d.js("window.log.length = 0; document.getElementById('chk').addEventListener('click', function (e) { e.preventDefault(); });");
        d.click((20.0, Y[2]));
        assert_eq!(d.js("window.log.join()"), "click:false", "the toggle is seen, then undone, with no input or change");
        assert_eq!(d.js("document.getElementById('chk').checked"), "true");
    });
}

#[test]
fn a_radio_click_clears_the_group() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((20.0, Y[4]));
        assert_eq!(d.js("['r1', 'r2'].map(function (i) { return document.getElementById(i).checked; }).join()"), "false,true");
        // A click on the radio that is on gives no `change`.
        d.js("window.n = 0; document.getElementById('r2').addEventListener('change', function () { window.n++; });");
        d.click((20.0, Y[4]));
        assert_eq!(d.js("window.n + ',' + document.getElementById('r2').checked"), "0,true");
        d.click((20.0, Y[3]));
        assert_eq!(d.js("['r1', 'r2'].map(function (i) { return document.getElementById(i).checked; }).join()"), "true,false");
    });
}

#[test]
fn a_label_click_focuses_and_toggles_its_control() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[6]));
        assert_eq!(d.js("document.getElementById('chk').checked + ',' + document.activeElement.id"), "true,chk");
    });
}

#[test]
fn a_link_click_asks_for_a_navigation() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[5]));
        let b = d.page.browser().unwrap();
        let r = b.take_navigation_request().expect("a request");
        assert_eq!((r.url.as_str(), r.from.as_str()), ("http://localhost/other", "http://localhost/events.html"));
        // A canceled click does not navigate.
        d.js("document.getElementById('lk').addEventListener('click', function (e) { e.preventDefault(); });");
        d.click((50.0, Y[5]));
        assert!(d.page.browser().unwrap().take_navigation_request().is_none());
    });
}

#[test]
fn enter_submits_a_form_with_one_field_and_no_button() {
    big(|| {
        let mut d = Driver::new(r#"<body><form id="f" novalidate><input id="a" style="position:absolute;left:0;top:0;width:100px;height:20px"></form></body>"#);
        d.js("window.n = []; document.getElementById('f').addEventListener('submit', function (e) { window.n.push(e.submitter); e.preventDefault(); });");
        d.click((10.0, 10.0));
        d.press("Enter", NO);
        assert_eq!(d.js("window.n.length + ',' + window.n[0]"), "1,null");
        assert!(d.page.errors.is_empty(), "{:?}", d.page.errors);
    });
}

#[test]
fn a_disabled_button_is_not_clicked() {
    big(|| {
        let mut d = Driver::new(r#"<body><button id="b" disabled style="position:absolute;left:0;top:0;width:100px;height:20px">x</button></body>"#);
        d.js("window.n = 0; ['pointerdown','mousedown','click'].forEach(function (t) { document.addEventListener(t, function () { window.n++; }, true); });");
        d.click((10.0, 10.0));
        assert_eq!(d.js("window.n"), "0");
    });
}

#[test]
fn window_events_are_trusted_and_script_events_are_not() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.js("window.t = []; document.addEventListener('click', function (e) { window.t.push(e.isTrusted); }); document.dispatchEvent(new Event('click'));");
        d.click((50.0, Y[5]));
        assert_eq!(d.js("window.t.join()"), "false,true");
    });
}

#[test]
fn script_cannot_construct_a_window_event() {
    big(|| {
        let mut d = Driver::new(FORM);
        assert_eq!(d.js("try { new MouseEvent('click'); 'made' } catch (e) { e.name }"), "TypeError");
        assert_eq!(d.js("Object.getPrototypeOf(PointerEvent.prototype) === MouseEvent.prototype"), "true");
    });
}

#[test]
fn many_events_fit_in_the_event_cap() {
    big(|| {
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[0]));
        // 10,000 key presses and 10,000 moves: each makes several event objects, and `max_events` is 16,384.
        for i in 0..10_000 {
            d.type_chars(if i % 2 == 0 { "a" } else { "b" });
            d.mv((20.0 + (i % 100) as f64, 20.0 + (i % 7) as f64 * 40.0));
        }
        assert!(d.page.errors.is_empty(), "{:?}", &d.page.errors[..d.page.errors.len().min(3)]);
        assert_eq!(d.value("a").len(), 10_000);
    });
}

// ------------------------------------------------------------------------------------------ painting

const PAINT: &str = r#"<!DOCTYPE html><html><head><style>
body { margin: 0; font-size: 16px; }
input { position: absolute; left: 10px; width: 200px; height: 30px; box-sizing: border-box; margin: 0; padding: 0 4px; border: 1px solid #888; }
</style></head><body>
<input id="a" style="top:10px" type="text"><input id="b" style="top:50px" type="password"><input id="c" style="top:90px" type="text">
</body></html>"#;

fn region(px: &[u32], y0: usize, h: usize) -> Vec<u32> {
    (y0..y0 + h).flat_map(|y| px[y * 400 + 12..y * 400 + 208].iter().copied()).collect()
}

#[test]
fn a_password_shows_bullets_and_text_shows_its_letters() {
    big(|| {
        let mut d = Driver::new(PAINT);
        for (y, text) in [(25.0, "abc"), (65.0, "abc"), (105.0, "\u{2022}\u{2022}\u{2022}")] {
            d.click((50.0, y));
            d.type_chars(text);
        }
        d.click(AT_NONE);
        let px = d.page.paint().to_vec();
        let (a, b, c) = (region(&px, 12, 26), region(&px, 52, 26), region(&px, 92, 26));
        assert_ne!(a, b, "a password does not show its letters");
        assert_eq!(b, c, "a password shows one bullet for each letter");
        assert!(a.iter().any(|&p| p == 0xFF00_0000), "the letters are drawn in the text color");
    });
}

#[test]
fn typed_text_is_drawn_once_over_no_placeholder_and_no_old_value() {
    big(|| {
        let page = PAINT.replace(r#"id="a" style="top:10px" type="text""#, r#"id="a" style="top:10px" type="text" placeholder="Email""#).replace(r#"id="c" style="top:90px" type="text""#, r#"id="c" style="top:90px" type="text" value="old""#);
        let mut d = Driver::new(&page);
        let empty = region(&d.page.paint().to_vec(), 12, 26);
        assert!(empty.iter().any(|&p| p != 0xFFFF_FFFF && p != 0xFF88_8888), "an empty field shows its placeholder");
        for y in [25.0, 105.0] {
            d.click((50.0, y));
            d.press("a", CTRL);
            d.type_chars("abc");
        }
        d.click(AT_NONE);
        let px = d.page.paint().to_vec();
        assert_eq!(region(&px, 12, 26), region(&px, 92, 26), "the typed text is alone: no placeholder and no old value under it");
    });
}

#[test]
fn the_focused_field_has_a_caret_and_a_selection() {
    big(|| {
        let mut d = Driver::new(PAINT);
        d.click((50.0, 25.0));
        // Left 10 + border 1 + padding 4 = 15: the caret is one pixel wide at the start of the text.
        let caret = d.page.paint()[20 * 400 + 15];
        assert_eq!(caret, 0xFF00_0000, "the caret is in the text color");
        d.click(AT_NONE);
        assert_ne!(d.page.paint()[20 * 400 + 15], 0xFF00_0000, "no caret without focus");
        assert!(!region(&d.page.paint().to_vec(), 12, 26).contains(&0xFF00_0000), "an empty field draws nothing");
        d.click((50.0, 25.0));
        d.type_chars("abc");
        d.press("a", CTRL);
        let px = d.page.paint().to_vec();
        assert_eq!(px[20 * 400 + 16], 0xFFB3_D7FF, "the selected text has the selection color behind it");
    });
}

// ------------------------------------------------------------------------------------------ measurements

fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap();
    s.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() * 4
}

/// `cargo test --release -p bare --lib measure_input -- --ignored --nocapture`: the numbers of `docs/resources.md`.
#[test]
#[ignore]
fn measure_input() {
    big(|| {
        use std::time::Instant;
        let mut d = Driver::new(FORM);
        d.click((50.0, Y[0]));
        d.page.paint();
        let mut times: Vec<(f64, f64, f64)> = Vec::new();
        let (mut keys, mut moves, mut clicks) = (Vec::new(), Vec::new(), Vec::new());
        let rss0 = rss_kib();
        for i in 0..40_000 {
            #[allow(clippy::manual_range_contains)]
            if [1_000, 5_000, 10_000, 20_000, 40_000].contains(&i) {
                println!("rss after {i} rounds of (key, move, 1/10 click): {} KiB", rss_kib());
            }
            // A field holds a short text, as in a form: the cost of a key grows with the text, so keep it near 100 letters.
            if i % 100 == 99 {
                d.press("a", CTRL);
                d.press("Backspace", NO);
            }
            let t = Instant::now();
            d.type_chars("a");
            let k = t.elapsed();
            d.page.paint();
            keys.push((k.as_secs_f64() * 1e6, t.elapsed().as_secs_f64() * 1e6));
            let t = Instant::now();
            d.mv((20.0 + (i % 100) as f64, 20.0 + (i % 7) as f64 * 40.0));
            moves.push(t.elapsed().as_secs_f64() * 1e6);
            if i % 10 == 0 {
                let t = Instant::now();
                d.click((50.0, Y[0]));
                clicks.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        let rss1 = rss_kib();
        let q = |v: &mut Vec<f64>, p: f64| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[((v.len() - 1) as f64 * p) as usize]
        };
        let mut handle: Vec<f64> = keys.iter().map(|k| k.0).collect();
        let mut total: Vec<f64> = keys.iter().map(|k| k.1).collect();
        println!("key press+release (events, default action, session pump): p50 {:.0}us p99 {:.0}us", q(&mut handle, 0.5), q(&mut handle, 0.99));
        println!("key to painted frame (adds relayout and paint): p50 {:.0}us p99 {:.0}us", q(&mut total, 0.5), q(&mut total, 0.99));
        println!("pointer move: p50 {:.0}us p99 {:.0}us", q(&mut moves, 0.5), q(&mut moves, 0.99));
        println!("click (move, press, release): p50 {:.0}us p99 {:.0}us", q(&mut clicks, 0.5), q(&mut clicks, 0.99));
        println!("rss before {rss0} KiB, after 10000 rounds {rss1} KiB, stats {:?}", d.page.browser().unwrap().stats());
        let _ = &mut times;
    });
}

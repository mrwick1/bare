//! Tests of the frame refresh of the window (`page.rs`): a frame after a change must have the pixels of a frame made from nothing,
//! and those of headless Chrome where the page has no text (`tools/ui/chrome_refresh_pixels.mjs`). A frame whose render key did not
//! move must not lay the page out again.

use crate::browser::*;
use crate::page::{Page, Source};
use bare_css::cascade::{self, Origin, RuleSet};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::{App, Event, Mods};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(html: &str, w: u32, h: u32) -> Page {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_viewport(w as f32, h as f32).expect("viewport");
    b.set_document_url("http://localhost/refresh.html").expect("url");
    let mut p = Page::new(Source::Browser(Box::new(b)), (w, h));
    p.whole = 0;
    p
}

fn mv(p: &mut Page, x: f64, y: f64) {
    p.handle(Event::PointerMove { x, y, buttons: 0, mods: NO });
}

fn button(p: &mut Page, x: f64, y: f64, pressed: bool) {
    p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
}

fn click(p: &mut Page, x: f64, y: f64) {
    mv(p, x, y);
    button(p, x, y, true);
    button(p, x, y, false);
}

fn key(p: &mut Page, name: &str, text: Option<&str>) {
    for pressed in [true, false] {
        p.handle(Event::Key { key: name.into(), code: if name == "Tab" { "Tab" } else { "" }, pressed, repeat: false, text: text.filter(|_| pressed).map(Into::into), mods: NO });
    }
}

/// Run `src` as a driver does, outside the window events.
fn js(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let out = b.it.display(v);
    p.touched();
    out
}

/// The frame of the window now, checked against a frame made from nothing: the pixels, the fragments and the page bars.
fn frame(p: &mut Page, step: &str) -> Vec<u32> {
    let px = p.paint().to_vec();
    if let Err(e) = p.check_frame() {
        panic!("{step}: {e}");
    }
    px
}

/// What `bare --paint` gives for `html` (`main.rs`, `paint_file`): the `<style>` sheets, the cascade, the layout and the paint, with no script.
fn static_paint(html: &str, w: u32, h: u32) -> Vec<u32> {
    let mut doc = tree::parse(html);
    let vp = (w as f32, h as f32);
    let mut rules = RuleSet::new(vp);
    rules.add(&mut doc, cascade::UA_CSS, Origin::UserAgent);
    for s in cascade::sources(&doc, vp) {
        if let cascade::Source::Inline(text) = s {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, vp);
    bare_paint::capture(&doc, &styles, &layout, w, h).pixels
}

/// Hover, focus by a press and by Tab, a held press, script changes, a blur and two resizes: each frame has the pixels of headless
/// Chrome 155 (every pixel; the page has no text) and of a frame made from nothing.
#[test]
fn each_change_draws_the_pixels_of_chrome() {
    const HTML: &str = include_str!("../../../tools/fixtures/ui/refresh-pixels.html");
    const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-refresh-pixels.txt");
    big(|| {
        let mut p = page(HTML, 200, 150);
        let (mut step, mut w, mut px, mut steps) = (String::new(), 200, Vec::new(), 0);
        for line in CHROME.lines() {
            if let Some(head) = line.strip_prefix('@') {
                let (name, width) = head.split_once(' ').unwrap();
                (step, w) = (name.to_string(), width.parse::<usize>().unwrap());
                match name {
                    "start" => {}
                    "hover" => mv(&mut p, 40.0, 25.0),
                    "away" => mv(&mut p, 195.0, 140.0),
                    "click-f" => click(&mut p, 110.0, 30.0),
                    "tab-g" => key(&mut p, "Tab", None),
                    "press-a" => {
                        mv(&mut p, 40.0, 65.0);
                        button(&mut p, 40.0, 65.0, true);
                    }
                    "release-a" => button(&mut p, 40.0, 65.0, false),
                    "script" => drop(js(&mut p, "document.getElementById('d').className = 'b on'; for (let i = 0; i < 3; i++) document.getElementById('list').append(document.createElement('div')); 0")),
                    "script-2" => drop(js(&mut p, "document.getElementById('list').firstChild.remove(); document.getElementById('d').style.top = '110px'; 0")),
                    "blur" => drop(js(&mut p, "document.activeElement.blur(); 0")),
                    "resize" | "resize-back" => p.resize(w as u32, 150),
                    _ => panic!("unknown step {name}"),
                }
                px = frame(&mut p, &step);
                steps += 1;
                continue;
            }
            let (rows, runs) = line.split_once(' ').unwrap();
            let (r0, r1) = rows.split_once('-').unwrap();
            for row in r0.parse::<usize>().unwrap()..=r1.parse::<usize>().unwrap() {
                for run in runs.split(' ') {
                    let (xs, color) = run.split_once('=').unwrap();
                    let (x0, x1) = xs.split_once('-').unwrap();
                    let want = 0xFF00_0000 | u32::from_str_radix(color, 16).unwrap();
                    for x in x0.parse::<usize>().unwrap()..=x1.parse::<usize>().unwrap() {
                        assert_eq!(px[row * w + x], want, "{step}: pixel {x},{row}: chrome {want:08x}");
                    }
                }
            }
        }
        assert_eq!(steps, 12);
    });
}

/// A click and a key that change nothing the layout reads keep the layout; a hover over a page with no `:hover` rule draws no frame.
/// A `:hover` or `:focus` rule that changes colors cascades again and keeps the layout; one that changes a size lays out again.
#[test]
fn a_change_that_the_layout_does_not_read_keeps_the_layout() {
    big(|| {
        let html = "<!doctype html><body style='margin:0'><div id=t style='height:50px;background:#cde'>text</div><input id=i style='position:absolute;top:60px'><script></script>";
        let mut p = page(html, 300, 200);
        js(&mut p, "document.body.addEventListener('click', () => {}); 0");
        frame(&mut p, "start");
        let r = p.renders;
        click(&mut p, 20.0, 20.0);
        assert!(!p.handle(Event::PointerMove { x: 25.0, y: 25.0, buttons: 0, mods: NO }).redraw, "no :hover rule: a move draws nothing");
        click(&mut p, 20.0, 70.0);
        key(&mut p, "a", Some("a"));
        key(&mut p, "b", Some("b"));
        frame(&mut p, "typed");
        assert_eq!(p.renders, r, "a click, a listener that changes nothing and typing do not lay out again");
        assert_eq!(js(&mut p, "document.getElementById('i').value"), "ab");
        js(&mut p, "document.getElementById('t').textContent = 'other'; 0");
        frame(&mut p, "text");
        assert_eq!(p.renders, r + 1, "a text change lays out once");
        frame(&mut p, "again");
        assert_eq!(p.renders, r + 1, "a frame with no change lays out nothing");

        let html = "<!doctype html><style>body{margin:0}#t{height:50px}#t:hover{background:#d00}#u:hover{height:60px}input:focus{color:#0a0}</style>\
            <div id=t>text</div><div id=u>other</div><input id=i>";
        let mut p = page(html, 300, 200);
        frame(&mut p, "start");
        let (r, s) = (p.renders, p.restyles);
        assert!(p.handle(Event::PointerMove { x: 25.0, y: 25.0, buttons: 0, mods: NO }).redraw, "a :hover rule: the move draws");
        frame(&mut p, "hover");
        assert_eq!((p.renders, p.restyles), (r, s + 1), "a color on hover: one cascade, no layout");
        click(&mut p, 20.0, 125.0);
        frame(&mut p, "focus");
        assert_eq!(p.renders, r, "a color on focus: no layout");
        mv(&mut p, 25.0, 55.0);
        frame(&mut p, "hover u");
        assert_eq!(p.renders, r + 1, "a height on hover lays out again");
        mv(&mut p, 299.0, 199.0);
        frame(&mut p, "out");
        assert_eq!(p.renders, r + 2);
    });
}

/// The value, the checkedness and the chosen option that a click or a script sets are drawn at once, and every frame between the
/// changes is the frame of a fresh window.
#[test]
fn form_state_changes_are_drawn() {
    big(|| {
        let html = "<!doctype html><body style='margin:0'><input id=c type=checkbox style='position:absolute;left:10px;top:10px;margin:0'>\
            <input id=r type=radio name=g checked style='position:absolute;left:40px;top:10px;margin:0'><input id=r2 type=radio name=g style='position:absolute;left:70px;top:10px;margin:0'>\
            <select id=s style='position:absolute;left:10px;top:40px'><option>one</option><option>two</option></select>\
            <input id=i value=start style='position:absolute;left:10px;top:80px'>";
        let mut p = page(html, 300, 150);
        let start = frame(&mut p, "start");
        let r = p.renders;
        click(&mut p, 16.0, 16.0);
        let checked = frame(&mut p, "checkbox click");
        assert_eq!(p.renders, r, "no form pseudo-class rule: a click on a checkbox does not lay out");
        assert_ne!(start, checked, "the click draws the mark");
        assert_eq!(js(&mut p, "document.getElementById('c').checked"), "true");
        assert_eq!(checked[14 * 300 + 11], 0xff00_75ff, "a checked box is blue");
        click(&mut p, 76.0, 16.0);
        frame(&mut p, "radio click");
        js(&mut p, "document.getElementById('c').checked = false; document.getElementById('s').selectedIndex = 1; document.getElementById('i').value = 'changed'; 0");
        frame(&mut p, "script state");
        js(&mut p, "document.getElementById('c').checked = false; document.getElementById('r').checked = true; document.getElementById('s').selectedIndex = 0; document.getElementById('i').value = 'start'; 0");
        assert_eq!(frame(&mut p, "back"), start, "the state of the start draws the first frame again");

        // `:checked` and `:placeholder-shown` read the form state: a color cascades again, a size lays out again.
        let html = "<!doctype html><style>body{margin:0}input{margin:0}#c:checked+div{background:#d00}div{height:20px}#i:placeholder-shown+p{height:40px}p{margin:0;height:10px;background:#0a0}</style>\
            <input id=c type=checkbox><div></div><input id=i placeholder=x><p></p>";
        let mut p = page(html, 300, 150);
        frame(&mut p, "start");
        let (r, s) = (p.renders, p.restyles);
        js(&mut p, "document.getElementById('c').checked = true; 0");
        frame(&mut p, ":checked");
        assert_eq!((p.renders, p.restyles), (r, s + 1), "a color on :checked: one cascade, no layout");
        js(&mut p, "document.getElementById('i').value = 'v'; 0");
        frame(&mut p, ":placeholder-shown");
        assert_eq!(p.renders, r + 1, "a height on :placeholder-shown lays out again");
    });
}

/// 200 rounds of mixed changes (text, class, a node in and out, hover, focus, typing, resize): no frame keeps an old picture. The last
/// frame equals `bare --paint` of the final DOM.
#[test]
fn repeated_changes_leave_no_stale_output() {
    big(|| {
        let html = "<!doctype html><style>body{margin:0}.b{height:20px;background:#cde}.b:hover{background:#d00}.on{background:#0a0;height:30px}\
            p:focus{background:#fd0}p:hover{padding-left:4px}@media (max-width:250px){.b{background:#555}}</style><div id=root><div class=b id=a></div><p id=f tabindex=0>focus</p><div class=b id=z>last</div></div>";
        let mut p = page(html, 300, 200);
        frame(&mut p, "start");
        let mut sizes = [(300, 200), (240, 200), (300, 160)].into_iter().cycle();
        for i in 0..200 {
            match i % 7 {
                0 => drop(js(&mut p, &format!("document.getElementById('a').textContent = 'round {i}'; 0"))),
                1 => drop(js(&mut p, "document.getElementById('z').classList.toggle('on'); 0")),
                2 => drop(js(&mut p, &format!("var r = document.getElementById('root'); if (r.children.length > 4) r.lastChild.remove(); else {{ var d = document.createElement('div'); d.className = 'b'; d.textContent = 'new {i}'; r.append(d); }} 0"))),
                3 => mv(&mut p, 50.0, (i % 60) as f64),
                4 => click(&mut p, 10.0, 30.0),
                5 => key(&mut p, "Tab", None),
                _ => {
                    let (w, h) = sizes.next().unwrap();
                    p.resize(w, h);
                }
            }
            frame(&mut p, &format!("round {i}"));
        }
        // The final DOM without the hover and the focus, at the last size, against a fresh static paint.
        mv(&mut p, 299.0, 159.0);
        js(&mut p, "document.activeElement.blur(); 0");
        p.resize(300, 200);
        let win = frame(&mut p, "final");
        let dom = js(&mut p, "document.documentElement.outerHTML");
        let still = static_paint(&format!("<!doctype html>{dom}"), 300, 200);
        let diff = win.iter().zip(&still).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 0, "the window and a fresh --paint of the final DOM differ in {diff} pixels");
    });
}

#[test]
fn the_state_follows_the_page_under_a_still_pointer() {
    big(|| {
        // A key adds a box above `#b`: `#b` moves under the pointer that did not move. The `#f` field is focused, then a script removes it.
        let html = "<!doctype html><style>body{margin:0}div{height:50px}#b:hover{background:#f00}#w:focus-within{background:#0f0}input{border:0;width:10px}</style>\
            <div id=b></div><div id=w><input id=f></div>";
        let mut p = page(html, 100, 150);
        js(&mut p, "var n = 0, $ = id => document.getElementById(id); $('b').onmouseover = () => n++; addEventListener('keydown', e => { if (e.key == 'a') document.body.prepend(document.createElement('div')); }); 0");
        mv(&mut p, 50.0, 70.0);
        assert_eq!(frame(&mut p, "start")[70 * 100 + 50], 0xffffffff);
        key(&mut p, "a", Some("a"));
        assert_eq!(frame(&mut p, "moved")[70 * 100 + 50], 0xffff0000, "#b is under the pointer now");
        assert_eq!(js(&mut p, "n"), "1");
        js(&mut p, "$('f').focus()");
        assert_eq!(frame(&mut p, "focused")[120 * 100 + 50], 0xff00ff00);
        js(&mut p, "$('f').remove()");
        assert_eq!(frame(&mut p, "removed")[120 * 100 + 50], 0xffffffff, "#w has no focused element now");
    })
}

/// Random sequences of window events and script changes on a long page with floats, positioned, fixed and sticky boxes, a scroll
/// container, lists and rules on `:hover`, `:focus`, siblings and `:first-child`. Each frame equals a frame made from nothing
/// (pixels, fragments and page bars), and most changes style and lay out only what changed.
#[test]
fn random_changes_draw_the_frame_made_from_nothing() {
    big(|| {
        let mut html = String::from(
            "<!doctype html><style>body{margin:4px}.l{height:20px}.l:hover{background:#cde}.w{width:120px}.f{float:left;width:30px;height:30px;background:#9c9}\
             .r{position:relative;left:3px}.a{position:absolute;right:5px;top:40px;width:20px;height:20px;background:#c99}.x{position:fixed;left:0;bottom:0;width:40px;height:10px;background:#99c}\
             .s{position:sticky;top:0;background:#eee}.box{overflow:auto;height:60px}.big{font-size:20px}.hide{display:none}.l:first-child{color:#a00}.on+.l{color:#0a0}\
             li{margin-left:20px}.ib{display:inline-block;width:40px}p:focus{background:#fd0}p:hover{padding-left:3px}</style><div class=x></div><div class=s>head</div>",
        );
        for i in 0..120 {
            match i % 12 {
                0 => html.push_str(&format!("<div class=box id=b{i}><div class=l>in {i}</div><div class=l>in</div><div class=l>in</div><div class=l>in</div></div>")),
                3 => html.push_str(&format!("<ul><li class=l id=e{i}>item {i}</li><li class=l>item</li></ul>")),
                5 => html.push_str(&format!("<div class=f id=e{i}></div><div class=l id=t{i}>beside the float {i}</div>")),
                7 => html.push_str(&format!("<div class=r id=e{i}><span class=ib>ib</span> line {i}<div class=a></div></div>")),
                9 => html.push_str(&format!("<p tabindex=0 id=e{i}>focus {i}</p>")),
                _ => html.push_str(&format!("<div class=l id=e{i}>line {i} with some words</div>")),
            }
        }
        let mut p = page(&html, 300, 200);
        js(&mut p, "var $ = id => document.getElementById(id), n = 0; 0");
        frame(&mut p, "start");
        let mut seed = 0x51ED_2701_A3B4_C5D6u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let (k0, mut sizes) = (p.kept, [(300, 200), (260, 200), (300, 240)].into_iter().cycle());
        for step in 0..400 {
            let id = rnd(120);
            let el = format!("(document.getElementById('e{id}') || document.body.lastElementChild)");
            match rnd(14) {
                0 | 1 => drop(js(&mut p, &format!("{el}.firstChild ? ({el}.firstChild.data = 'text ' + (++n)) : ({el}.textContent = 'new'); 0"))),
                2 => drop(js(&mut p, &format!("{el}.classList.toggle(['big', 'on', 'hide', 'w'][n++ % 4]); 0"))),
                3 => drop(js(&mut p, &format!("var d = document.createElement('div'); d.className = 'l'; d.textContent = 'added ' + (++n); {el}.after(d); 0"))),
                4 => drop(js(&mut p, &format!("var e = {el}; if (e.parentNode != document.body || document.body.children.length > 60) e.remove(); 0"))),
                5 | 6 => mv(&mut p, rnd(300) as f64, rnd(200) as f64),
                7 => click(&mut p, rnd(300) as f64, rnd(200) as f64),
                8 => key(&mut p, "Tab", None),
                9 | 10 => drop(p.handle(Event::Wheel { x: rnd(300) as f64, y: rnd(200) as f64, dx: 0.0, dy: (rnd(800) as f64) - 300.0, mods: NO })),
                11 => drop(js(&mut p, &format!("{el}.style.height = (10 + n++ % 30) + 'px'; 0"))),
                12 => drop(js(&mut p, "var b = document.querySelector('.box'); if (b) b.scrollTop = (n++ * 7) % 40; 0")),
                _ => {
                    let (w, h) = sizes.next().unwrap();
                    p.resize(w, h);
                }
            }
            frame(&mut p, &format!("step {step}"));
        }
        assert!(p.kept - k0 > 100, "{} of the frames styled and laid out only what changed", p.kept - k0);
    });
}

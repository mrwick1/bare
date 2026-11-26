//! The keys of the shell on live pages (`shell.rs`, `shell_page.rs`, `shell_draw.rs`): hints, INSERT, find, the command line, the
//! clipboard, scrolling and the status line.

use crate::browser_session::SessionConfig;
use crate::page::Page;
use crate::resources::test_server::{Reply, Server, serve};
use crate::shell::Shell;
use crate::shell_keys::Mode;
use bare_net::http::Client;
use bare_window::{App, Control, Event, Mods};
use std::time::{Duration, Instant};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const LIMIT: Duration = Duration::from_secs(10);
const HOME: &str = "<!DOCTYPE html><title>Modes</title><style>body{margin:0} button{display:block;width:120px;height:30px} input{display:block;width:200px;height:24px} a{display:block;width:80px;height:20px} .tall{height:2000px}</style>\
<button id=b onclick='window.clicked = (window.clicked || 0) + 1'>press</button><input id=i><a id=l href='/second'>go</a><p>alpha needle beta</p><div class=tall></div><p id=far>the far Needle here</p>";

fn big(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

fn site() -> Server {
    serve(|path| match path {
        "/second" => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>Second</title><p>two</p>"),
        _ => Reply::new(200, Some("text/html"), HOME),
    })
}

fn shell(server: &Server) -> Shell {
    let url = server.url("/");
    let mut p = Page::open_cfg(SessionConfig::new(&url), (800, 600)).unwrap();
    p.wait_load(LIMIT).unwrap();
    let mut s = Shell::new(p, &url, Client::new(), (800, 600));
    s.resize(800, 600);
    s
}

fn key_with(s: &mut Shell, key: &str, mods: Mods) {
    let mut ctl = Control::default();
    for pressed in [true, false] {
        let text = (pressed && key.chars().count() == 1).then(|| key.to_string());
        s.event(Event::Key { key: key.into(), code: "", pressed, repeat: false, text, mods }, &mut ctl);
    }
}

fn key(s: &mut Shell, key: &str) {
    key_with(s, key, NO);
}

fn keys(s: &mut Shell, text: &str) {
    for c in text.chars() {
        key(s, &c.to_string());
    }
}

fn line(s: &mut Shell, text: &str) {
    key(s, ":");
    keys(s, text);
    key(s, "Enter");
}

fn js(s: &mut Shell, src: &str) -> String {
    use crate::eng_new::Peek;
    let b = s.page().browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

fn until(s: &mut Shell, what: &str, done: impl Fn(&mut Shell) -> bool) {
    let end = Instant::now() + LIMIT;
    while !done(s) {
        assert!(Instant::now() < end, "{what}");
        let next = s.tick(&mut Control::default());
        std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(5))));
    }
}

fn pixel(s: &mut Shell, x: usize, y: usize) -> u32 {
    let mut out = vec![0u32; 800 * 600];
    assert!(s.frame(&mut Control::default(), &mut out));
    out[y * 800 + x] & 0xFF_FFFF
}

#[test]
fn f_and_a_label_click_a_button() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "f");
        assert_eq!(s.mode(), Mode::Hint);
        // The elements in tree order: the button, the field and the link get `a`, `s` and `d`.
        key(&mut s, "a");
        assert_eq!(s.mode(), Mode::Normal);
        assert_eq!(js(&mut s, "String(window.clicked)"), "1");
    });
}

#[test]
fn a_hint_on_a_field_focuses_it_and_enters_insert_and_text_reaches_it() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "f");
        key(&mut s, "s");
        assert_eq!(s.mode(), Mode::Insert);
        assert_eq!(js(&mut s, "document.activeElement.id"), "i");
        // In INSERT a letter that is a NORMAL key goes to the field.
        keys(&mut s, "hjk");
        assert_eq!(js(&mut s, "document.getElementById('i').value"), "hjk");
        assert_eq!(js(&mut s, "String(window.scrollY)"), "0");
        // Escape reaches the page, then blurs the field and returns to NORMAL.
        key(&mut s, "Escape");
        assert_eq!(s.mode(), Mode::Normal);
        assert_eq!(js(&mut s, "document.activeElement === document.body ? 'body' : document.activeElement.id"), "body");
    });
}

#[test]
fn i_focuses_the_first_text_field() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "i");
        assert_eq!(s.mode(), Mode::Insert);
        assert_eq!(js(&mut s, "document.activeElement.id"), "i");
    });
}

#[test]
fn a_shift_f_hint_on_a_link_opens_a_new_page() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "F");
        key(&mut s, "d");
        assert_eq!((s.len(), s.current()), (2, 1));
        assert!(s.list()[1].2.ends_with("/second"));
        assert_eq!(s.mode(), Mode::Normal);
    });
}

#[test]
fn escape_cancels_hints_and_a_click_ends_them() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "f");
        key(&mut s, "Escape");
        assert_eq!(s.mode(), Mode::Normal);
        assert_eq!(js(&mut s, "String(window.clicked)"), "undefined");
        key(&mut s, "f");
        let mut ctl = Control::default();
        s.event(Event::Wheel { x: 10.0, y: 10.0, dx: 0.0, dy: 10.0, mods: NO }, &mut ctl);
        assert_eq!(s.mode(), Mode::Normal);
    });
}

#[test]
fn slash_finds_and_selects_the_text_and_n_goes_on() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "/");
        assert_eq!(s.mode(), Mode::Find);
        keys(&mut s, "needle");
        key(&mut s, "Enter");
        assert_eq!(s.mode(), Mode::Normal);
        assert_eq!(js(&mut s, "getSelection().toString()"), "needle");
        assert_eq!(s.message(), Some("[1/2]"));
        assert_eq!(js(&mut s, "String(window.scrollY)"), "0");
        // The second match is far down: it is selected and scrolled into view.
        key(&mut s, "n");
        assert_eq!(js(&mut s, "getSelection().toString()"), "Needle");
        assert_eq!(s.message(), Some("[2/2]"));
        assert!(js(&mut s, "String(window.scrollY > 0)") == "true");
        // `N` goes back, and the list wraps.
        key(&mut s, "N");
        assert_eq!(js(&mut s, "getSelection().toString()"), "needle");
        key(&mut s, "N");
        assert_eq!(s.message(), Some("[2/2]"));
        // Nothing found: a message, no change.
        key(&mut s, "/");
        keys(&mut s, "zzz");
        key(&mut s, "Enter");
        assert_eq!(s.message(), Some("not found: zzz"));
    });
}

#[test]
fn the_command_line_opens_a_url_in_this_page_and_in_a_new_one() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        line(&mut s, &format!("open {}", server.url("/second")));
        until(&mut s, "the page did not load /second", |s| s.page().url().is_some_and(|u| u.ends_with("/second")) && s.page().title() == "Second");
        assert_eq!(s.len(), 1);
        line(&mut s, &format!("t {}", server.url("/")));
        assert_eq!((s.len(), s.current()), (2, 1));
        // `buffer 1` shows the first page; an unknown page and an unknown command say so.
        line(&mut s, "b 1");
        assert_eq!(s.current(), 0);
        line(&mut s, "b 9");
        assert_eq!(s.message(), Some("no page 9"));
        line(&mut s, "frobnicate");
        assert_eq!(s.message(), Some("not a command: frobnicate"));
        // `bd 2` removes the second page; `q` closes the window.
        line(&mut s, "bd 2");
        assert_eq!(s.len(), 1);
        assert!(!s.sent.iter().any(|x| x == "close"));
        line(&mut s, "q");
        assert!(s.sent.iter().any(|x| x == "close"));
    });
}

#[test]
fn x_closes_the_page_and_the_last_one_closes_the_window() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        line(&mut s, &format!("t {}", server.url("/second")));
        key(&mut s, "x");
        assert_eq!((s.len(), s.current()), (1, 0));
        assert!(s.sent.is_empty());
        key(&mut s, "x");
        assert_eq!(s.sent, ["close"]);
    });
}

#[test]
fn y_copies_the_url_and_p_opens_the_clipboard() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "y");
        assert_eq!(s.sent, [format!("copy {}", server.url("/"))]);
        assert!(s.message().is_some_and(|m| m.starts_with("copied ")));
        key(&mut s, "p");
        assert_eq!(s.sent.last().unwrap(), "paste");
        // The window answers with the text of the clipboard: the shell opens it.
        let mut ctl = Control::default();
        s.event(Event::Clipboard { items: vec![("text/plain;charset=utf-8".into(), server.url("/second").into_bytes())] }, &mut ctl);
        until(&mut s, "the page did not load /second", |s| s.page().url().is_some_and(|u| u.ends_with("/second")));
    });
}

#[test]
fn ctrl_v_pastes_into_the_command_line() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, ":");
        key_with(&mut s, "v", Mods { ctrl: true, ..NO });
        assert_eq!(s.sent, ["paste"]);
        let mut ctl = Control::default();
        s.event(Event::Clipboard { items: vec![("text/plain".into(), b"open x.test".to_vec())] }, &mut ctl);
        // The paste went to the line, not to the page; the line is still open.
        assert_eq!(s.mode(), Mode::Command);
        key(&mut s, "Escape");
        assert_eq!(s.mode(), Mode::Normal);
    });
}

#[test]
fn j_and_k_scroll_and_the_page_does_not_see_them() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        js(&mut s, "window.seen = ''; addEventListener('keydown', e => { seen += e.key })");
        key(&mut s, "j");
        key(&mut s, "j");
        assert_eq!(js(&mut s, "String(window.scrollY)"), "120");
        key(&mut s, "k");
        assert_eq!(js(&mut s, "String(window.scrollY)"), "60");
        key(&mut s, "G");
        assert!(js(&mut s, "String(window.scrollY > 1000)") == "true");
        key(&mut s, "g");
        key(&mut s, "g");
        assert_eq!(js(&mut s, "String(window.scrollY)"), "0");
        key(&mut s, "d");
        assert_eq!(js(&mut s, "String(window.scrollY)"), "300");
        // A key that is not a command, and a key with Ctrl, reach the page.
        key(&mut s, "z");
        key_with(&mut s, "j", Mods { ctrl: true, ..NO });
        assert_eq!(js(&mut s, "seen"), "zj");
    });
}

#[test]
fn the_status_line_shows_the_command_line_and_goes_away() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        let white = pixel(&mut s, 3, 595);
        key(&mut s, ":");
        // A solid dark line at the bottom of the window, over the page.
        assert_eq!(pixel(&mut s, 700, 595), 0x22_2222);
        assert_eq!(pixel(&mut s, 700, 5), pixel(&mut s, 700, 5));
        key(&mut s, "Escape");
        assert_eq!(pixel(&mut s, 700, 595), white);
        // A message shows for 3 seconds: the first tick after it takes it away.
        key(&mut s, "/");
        keys(&mut s, "zzz");
        key(&mut s, "Enter");
        assert_eq!(pixel(&mut s, 700, 595), 0x22_2222);
        assert_eq!(s.message(), Some("not found: zzz"));
        std::thread::sleep(Duration::from_millis(3100));
        s.tick(&mut Control::default());
        assert_eq!(s.message(), None);
        assert_eq!(pixel(&mut s, 700, 595), white);
    });
}

#[test]
fn hint_labels_are_drawn_over_the_elements() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        let plain = pixel(&mut s, 2, 2);
        key(&mut s, "f");
        // A yellow box at the top left of the first element (the button).
        assert_eq!(pixel(&mut s, 2, 2), 0xFF_E066);
        key(&mut s, "Escape");
        assert_eq!(pixel(&mut s, 2, 2), plain);
    });
}

#[test]
fn o_and_capital_o_fill_the_command_line() {
    big(|| {
        let server = site();
        let mut s = shell(&server);
        key(&mut s, "o");
        assert_eq!(s.mode(), Mode::Command);
        key(&mut s, "Escape");
        // `O` has the URL of the page in the line: Enter loads it again.
        js(&mut s, "window.marker = 1");
        key(&mut s, "O");
        assert_eq!(s.mode(), Mode::Command);
        key(&mut s, "Enter");
        assert_eq!(s.mode(), Mode::Normal);
        assert_eq!(s.message(), None);
        until(&mut s, "the page did not load again", |s| s.page().browser().is_some_and(|b| b.loaded()) && js(s, "String(window.marker)") == "undefined");
        assert_eq!(s.page().url(), Some(server.url("/")));
    });
}

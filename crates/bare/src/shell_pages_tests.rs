//! The page list of the window (`shell.rs`): shared storage, `_blank` links, closing, background timers and the title.

use crate::browser_session::SessionConfig;
use crate::page::Page;
use crate::resources::test_server::{Reply, Server, serve};
use crate::shell::Shell;
use bare_net::http::Client;
use bare_window::{App, Control, Event, Mods};
use std::time::{Duration, Instant};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const LIMIT: Duration = Duration::from_secs(10);

fn big(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

fn site() -> Server {
    serve(|path| match path {
        "/blank" => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>Blank</title><a href='/second' target='_blank' style='display:block;width:200px;height:100px'>go</a>"),
        "/second" => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>Second</title><p>two</p>"),
        "/timer" => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>Timer</title><script>window.fired = 0; setTimeout(() => { window.fired = 1 }, 50)</script>"),
        "/untitled" => Reply::new(200, Some("text/html"), "<!DOCTYPE html><p>no title</p>"),
        _ => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>  Home\n page </title><p>one</p>"),
    })
}

fn shell(url: &str) -> Shell {
    let mut p = Page::open_cfg(SessionConfig::new(url), (800, 600)).unwrap();
    p.wait_load(LIMIT).unwrap();
    Shell::new(p, url, Client::new(), (800, 600))
}

/// Open `url` in a new page and run its ticks until it has loaded.
fn open(s: &mut Shell, url: &str) {
    s.open(url, true).unwrap();
    s.page().wait_load(LIMIT).unwrap();
}

fn js(s: &mut Shell, i: usize, src: &str) -> String {
    use crate::eng_new::Peek;
    let b = s.page_at(i).browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

fn click(s: &mut Shell, x: f64, y: f64) {
    let mut ctl = Control::default();
    s.event(Event::PointerMove { x, y, buttons: 0, mods: NO }, &mut ctl);
    for pressed in [true, false] {
        s.event(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO }, &mut ctl);
    }
}

/// Tick the shell until `done` holds.
fn until(s: &mut Shell, what: &str, done: impl Fn(&mut Shell) -> bool) {
    let end = Instant::now() + LIMIT;
    while !done(s) {
        assert!(Instant::now() < end, "{what}");
        let next = s.tick(&mut Control::default());
        std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(5))));
    }
}

#[test]
fn one_page_ticks_and_draws() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        let mut ctl = Control::default();
        s.resize(800, 600);
        s.tick(&mut ctl);
        let mut out = vec![0u32; 800 * 600];
        assert!(s.frame(&mut ctl, &mut out));
        assert!(out.iter().any(|&p| p & 0xFFFFFF != 0xFFFFFF), "the page drew nothing");
        assert_eq!((s.len(), s.current()), (1, 0));
    });
}

#[test]
fn a_second_page_sees_local_storage_but_not_session_storage() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        js(&mut s, 0, "localStorage.setItem('k', 'v'); sessionStorage.setItem('k', 's'); document.cookie = 'c=1'");
        open(&mut s, &server.url("/second"));
        assert_eq!(js(&mut s, 1, "localStorage.getItem('k')"), "v");
        assert_eq!(js(&mut s, 1, "sessionStorage.getItem('k')"), "null");
        assert_eq!(js(&mut s, 1, "document.cookie"), "c=1");
        // The change of the second page is seen by the first.
        js(&mut s, 1, "localStorage.setItem('k', 'w')");
        assert_eq!(js(&mut s, 0, "localStorage.getItem('k')"), "w");
    });
}

#[test]
fn a_blank_link_opens_a_page_and_switches_to_it() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/blank"));
        s.resize(800, 600);
        click(&mut s, 10.0, 10.0);
        until(&mut s, "the second page did not open", |s| s.len() == 2);
        assert_eq!(s.current(), 1);
        let urls: Vec<_> = s.list().into_iter().map(|(_, _, u)| u).collect();
        assert!(urls[0].ends_with("/blank"), "{urls:?}");
        assert!(urls[1].ends_with("/second"), "{urls:?}");
        // The first page did not navigate.
        assert_eq!(js(&mut s, 0, "location.pathname"), "/blank");
    });
}

#[test]
fn dialogs_keep_the_old_window_answers() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        assert_eq!(js(&mut s, 0, "String(confirm('x')) + String(prompt('y', 'z')) + String(alert('w'))"), "falsenullundefined");
        // `window.open` records a tab, and still gives null.
        assert_eq!(js(&mut s, 0, "String(window.open('/second'))"), "null");
        until(&mut s, "window.open did not open a page", |s| s.len() == 2);
    });
}

#[test]
fn close_goes_to_a_neighbor_and_the_last_page_asks_the_window_to_close() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        open(&mut s, &server.url("/second"));
        open(&mut s, &server.url("/untitled"));
        assert_eq!(s.current(), 2);
        // The last page goes: the one before it takes its place.
        assert!(!s.close(2));
        assert_eq!((s.len(), s.current()), (2, 1));
        // A page before the current one goes: the current page stays the same page.
        assert!(!s.close(0));
        assert_eq!((s.len(), s.current()), (1, 0));
        assert!(js(&mut s, 0, "location.pathname") == "/second");
        assert!(s.close(0), "the last page must ask the window to close");
    });
}

#[test]
fn next_and_prev_wrap_around() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        open(&mut s, &server.url("/second"));
        assert!(s.next());
        assert_eq!(s.current(), 0);
        assert!(s.prev());
        assert_eq!(s.current(), 1);
        assert!(!s.switch(7));
    });
}

#[test]
fn a_page_in_the_background_still_runs_its_timer() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/timer"));
        assert_eq!(js(&mut s, 0, "window.fired"), "0");
        open(&mut s, &server.url("/second"));
        assert_eq!(s.current(), 1);
        until(&mut s, "the timer of the background page did not run", |s| js(s, 0, "window.fired") == "1");
    });
}

#[test]
fn the_title_shows_the_place_in_the_list_and_the_name_of_the_page() {
    big(|| {
        let server = site();
        let mut s = shell(&server.url("/"));
        assert_eq!(s.title(), "[1/1] Home page — bare");
        open(&mut s, &server.url("/untitled"));
        assert_eq!(s.title(), format!("[2/2] {} — bare", server.url("/untitled")));
        s.switch(0);
        assert_eq!(s.title(), "[1/2] Home page — bare");
        let list = s.list();
        assert_eq!(list[0].1, "Home page");
        assert_eq!(list[1].1, "");
    });
}

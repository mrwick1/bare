//! A live page loads an iframe that a script adds after `load` at once, and gets a `BroadcastChannel` message of that frame at once: both
//! are work for the next tick, also when the page has no timer. Outlook adds a hidden sign-in iframe and waits 10 s for its message;
//! the frame loaded, and the message arrived, only when that timer fired.

use crate::browser_session::SessionConfig;
use crate::eng_new::Peek;
use crate::page::Page;
use crate::resources::test_server::{Reply, serve};
use std::time::{Duration, Instant};

#[test]
fn an_iframe_added_after_load_loads_without_a_timer() {
    let hits = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let server = serve(|path| match path {
                "/child" => Reply::new(200, Some("text/html"), "<p>child</p>"),
                _ => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>t</title>"),
            });
            let mut p = Page::open_cfg(SessionConfig::new(&server.url("/")), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            p.browser().unwrap().run_script("var f = document.createElement('iframe'); document.body.appendChild(f); f.src = '/child'").unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            while server.hits("/child") == 0 && Instant::now() < end {
                let (_, next) = p.tick();
                let Some(next) = next else { break };
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            server.hits("/child")
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(hits, 1);
}

#[test]
fn a_broadcast_message_of_a_frame_reaches_the_parent_without_a_timer() {
    let got = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let server = serve(|path| match path {
                "/c" => Reply::new(200, Some("text/html"), "<script>setTimeout(function () { var c = new BroadcastChannel('auth'); c.postMessage(1); c.close() }, 50)</script>"),
                _ => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>t</title>"),
            });
            let mut cfg = SessionConfig::new(&server.url("/"));
            // A turn ends after one round: the parent takes the message of the frame in a later turn.
            cfg.page.slice = Duration::from_micros(50);
            let mut p = Page::open_cfg(cfg, (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            let b = p.browser().unwrap();
            b.run_script("window.got = 0; new BroadcastChannel('auth').onmessage = function (e) { got = e.data }; var f = document.createElement('iframe'); document.body.appendChild(f); f.src = '/c'").unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            let mut got = String::new();
            while got != "1" && Instant::now() < end {
                let (_, next) = p.tick();
                let b = p.browser().unwrap();
                let v = b.run_script("String(got)").unwrap();
                got = b.it.display(v);
                let Some(next) = next else { break };
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            got
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(got, "1");
}

#[test]
fn a_frame_that_a_redirect_with_a_fragment_brings_has_the_final_url_and_the_fragment() {
    let (got, want) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let server = serve(|path| match path {
                "/r" => Reply::new(302, None, "").header("Location", "/c#error=login_required"),
                "/c" => Reply::new(200, Some("text/html"), "<script>var c = new BroadcastChannel('auth'); c.postMessage(location.href); c.close()</script>"),
                _ => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>t</title>"),
            });
            let mut p = Page::open_cfg(SessionConfig::new(&server.url("/")), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            let b = p.browser().unwrap();
            b.run_script("window.got = ''; new BroadcastChannel('auth').onmessage = function (e) { got = e.data }; var f = document.createElement('iframe'); document.body.appendChild(f); f.src = '/r'").unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            let mut got = String::new();
            while got.is_empty() && Instant::now() < end {
                let (_, next) = p.tick();
                let b = p.browser().unwrap();
                let v = b.run_script("got").unwrap();
                got = b.it.display(v);
                let Some(next) = next else { break };
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            (got, server.url("/c#error=login_required"))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(got, want);
}

#[test]
fn a_cross_origin_redirect_back_to_the_parent_origin_gives_the_frame_the_parent_origin() {
    let (got, want) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let server = serve(|path| match path {
                "/c" => Reply::new(200, Some("text/html"), "<script>var c = new BroadcastChannel('auth'); c.postMessage(location.href); c.close()</script>"),
                _ => Reply::new(200, Some("text/html"), "<!DOCTYPE html><title>t</title>"),
            });
            let back = server.url("/c#error=login_required");
            let other = serve(move |_| Reply::new(302, None, "").header("Location", &back));
            let src = other.url("/r");
            let mut p = Page::open_cfg(SessionConfig::new(&server.url("/")), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            let b = p.browser().unwrap();
            b.run_script(&format!("window.got = ''; new BroadcastChannel('auth').onmessage = function (e) {{ got = e.data }}; var f = document.createElement('iframe'); document.body.appendChild(f); f.src = '{src}'")).unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            let mut got = String::new();
            while got.is_empty() && Instant::now() < end {
                let (_, next) = p.tick();
                let b = p.browser().unwrap();
                let v = b.run_script("got").unwrap();
                got = b.it.display(v);
                let Some(next) = next else { break };
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            (got, server.url("/c#error=login_required"))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(got, want);
}

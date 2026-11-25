//! Tests of pictures with a `blob:` URL: an `<img>` and a CSS `url()` read the Blob from the blob URL store of the document.

use crate::browser_session::SessionConfig;
use crate::eng_new::Peek;
use crate::page::Page;
use crate::resources::test_server::{Reply, Server, serve};

const HTML: &str = "<!doctype html><body style=\"margin:0\"><div id=d style=\"width:10px;height:10px\"></div></body>";

/// A 3 x 2 red PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAEElEQVR4nGP4z8AAQQxwFgBB0gX7h/C5SAAAAABJRU5ErkJggg==";

/// A loaded page, and its server (the page lives as long as the server).
fn page() -> (Page, Server) {
    let server = serve(|_| Reply::new(200, Some("text/html"), HTML));
    let mut p = Page::open_cfg(SessionConfig::new(&server.url("/")), (100, 100)).unwrap();
    let _ = p.wait_load(std::time::Duration::from_secs(5));
    (p, server)
}

fn js(p: &mut Page, code: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(code).unwrap();
    b.it.display(v)
}

/// Make a Blob of the PNG and a URL for it (`window.u`).
fn blob_url(p: &mut Page) {
    js(p, &format!("var s = atob('{PNG}'), a = new Uint8Array(s.length); for (var i = 0; i < s.length; i++) a[i] = s.charCodeAt(i); window.u = URL.createObjectURL(new Blob([a], {{type: 'image/png'}})); window.log = []; 0"));
}

/// Let the pictures arrive and the `load` and `error` tasks run.
fn settle(p: &mut Page) {
    for _ in 0..5 {
        p.settle_images();
        p.tick();
    }
}

#[test]
fn an_img_with_a_blob_url_loads_even_when_the_url_is_revoked_after_src() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let (mut p, _server) = page();
        blob_url(&mut p);
        js(&mut p, "var i = new Image(); i.onload = function () { log.push('load') }; i.onerror = function () { log.push('error') }; i.src = u; URL.revokeObjectURL(u); window.i = i; 0");
        settle(&mut p);
        assert_eq!(js(&mut p, "[log.join(), i.complete, i.naturalWidth, i.naturalHeight].join()"), "load,true,3,2");
    }).unwrap().join().unwrap();
}

#[test]
fn an_img_with_a_revoked_blob_url_fires_error() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let (mut p, _server) = page();
        blob_url(&mut p);
        js(&mut p, "URL.revokeObjectURL(u); var i = new Image(); i.onload = function () { log.push('load') }; i.onerror = function () { log.push('error') }; i.src = u; window.i = i; 0");
        settle(&mut p);
        assert_eq!(js(&mut p, "[log.join(), i.complete, i.naturalWidth].join()"), "error,true,0");
    }).unwrap().join().unwrap();
}

#[test]
fn a_css_background_with_a_blob_url_loads() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let (mut p, _server) = page();
        blob_url(&mut p);
        js(&mut p, "document.getElementById('d').setAttribute('style', 'width:10px;height:10px;background-image:url(' + u + ')'); 0");
        settle(&mut p);
        assert_eq!((p.images.done, p.images.failed), (1, 0));
    }).unwrap().join().unwrap();
}

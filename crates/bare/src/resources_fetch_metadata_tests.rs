//! Tests of the Fetch Metadata of subresource requests (`resources::fetch`). Chrome sends `Sec-Fetch-Site`, `Sec-Fetch-Mode` and
//! `Sec-Fetch-Dest` on every request to a potentially trustworthy URL. The WhatsApp CDN answers 400 to a worker script request
//! without `Sec-Fetch-Site`, and caches that answer for the URL.

use crate::browser_fetch_tests::{reply, serve, Rec};
use crate::browser_session::{Session, SessionConfig};

fn meta(r: &Rec) -> Vec<String> {
    ["sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest"].iter().map(|n| r.header(n).join(",")).collect()
}

#[test]
fn a_script_and_a_style_of_the_page_send_the_fetch_metadata() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let srv = serve(|r| match r.path.as_str() {
            "/a.js" => reply(200, &[("Content-Type", "application/javascript")], b"var a = 1;"),
            "/s.css" => reply(200, &[("Content-Type", "text/css")], b"p { color: red }"),
            _ => reply(200, &[("Content-Type", "text/html")], b"<link rel=stylesheet href=/s.css><script src=/a.js></script><p>x</p>"),
        });
        let mut c = SessionConfig::new(&format!("http://127.0.0.1:{}/", srv.port));
        c.virtual_clock = true;
        let mut s = Session::new(c);
        s.run();
        let recs = srv.recs();
        let of = |p: &str| meta(recs.iter().find(|r| r.path == p).unwrap_or_else(|| panic!("no request for {p}")));
        assert_eq!(of("/a.js"), ["same-origin", "no-cors", "script"]);
        assert_eq!(of("/s.css"), ["same-origin", "no-cors", "style"]);
    }).unwrap().join().unwrap();
}

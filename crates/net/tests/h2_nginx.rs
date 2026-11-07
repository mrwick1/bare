//! HTTP/2 through `bare_net::http::Client` against a real nginx (not our code, not nghttp2), run as the current user on a loopback port
//! with a synthetic root made for each test. It checks the server behavior that a CDN has too: gzip on the fly (no content-length), a
//! limit of 1 or 2 concurrent streams, a header over 16 KiB, a small DATA frame size, an idle close, trailers, conditional requests, and
//! uploads. nginx and its `echo` module must be installed; the tests skip (and say so) when they are not.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_net::http::{Client, Method, Request};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, openssl_available};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ECHO: &str = "/usr/lib64/nginx/modules/ngx_http_echo_module.so";

struct Nginx {
    child: Child,
    port: u16,
    dir: PathBuf,
    _pki: Pki,
}

impl Drop for Nginx {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn nginx_binary() -> Option<&'static str> {
    ["nginx", "/usr/sbin/nginx"].into_iter().find(|b| Command::new(b).arg("-v").stderr(Stdio::null()).stdout(Stdio::null()).status().is_ok())
}

fn lcg(n: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect()
}

fn text(n: usize) -> Vec<u8> {
    (0..).flat_map(|i: usize| format!("line {i} of the file, with some words that repeat\n").into_bytes()).take(n).collect()
}

/// `http_extra` goes into the `http` block, `server_extra` into the `server` block.
fn start(http_extra: &str, server_extra: &str) -> Option<(Nginx, Client)> {
    let Some(bin) = nginx_binary() else {
        eprintln!("skipped: nginx is not installed");
        return None;
    };
    if !openssl_available() || !std::path::Path::new(ECHO).exists() {
        eprintln!("skipped: openssl or the nginx echo module is missing");
        return None;
    }
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let dir = std::env::temp_dir().join(format!("bare-nginx-{}-{}", std::process::id(), pki.dir.file_name().unwrap().to_string_lossy()));
    let www = dir.join("www");
    std::fs::create_dir_all(&www).unwrap();
    std::fs::write(www.join("text.txt"), text(3 << 20)).unwrap();
    std::fs::write(www.join("small.txt"), b"small file\n").unwrap();
    std::fs::write(www.join("bin.dat"), lcg(5 << 20)).unwrap();
    std::fs::write(www.join("mid.dat"), lcg(200_000)).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let d = dir.display();
    let conf = format!(
        "load_module {ECHO};
daemon off; master_process off; pid {d}/nginx.pid; error_log {d}/error.log info; worker_processes 1;
events {{ worker_connections 256; }}
http {{
  access_log off;
  client_body_temp_path {d}/t1; proxy_temp_path {d}/t2; fastcgi_temp_path {d}/t3; uwsgi_temp_path {d}/t4; scgi_temp_path {d}/t5;
  default_type text/plain;
  gzip on; gzip_min_length 20; gzip_types text/plain application/octet-stream;
  client_max_body_size 64m; client_body_buffer_size 8m; large_client_header_buffers 4 128k;
  {http_extra}
  server {{
    listen 127.0.0.1:{port} ssl; http2 on;
    ssl_certificate {p}/good.pem; ssl_certificate_key {p}/good.key;
    root {d}/www;
    location = /id {{ echo $connection; }}
    location = /proto {{ echo $server_protocol; }}
    location = /slow {{ echo_sleep 0.02; echo $connection; }}
    location = /hdr {{ echo $http_x_big; }}
    location = /cookie {{ echo $http_cookie; }}
    location = /upload {{ echo_read_request_body; echo_request_body; }}
    location = /n204 {{ return 204; }}
    location = /moved {{ return 301 /small.txt; }}
    location = /trailer {{ add_trailer X-Sum 42 always; echo trailed; }}
    location = /flush {{ echo part1; echo_flush; echo_sleep 0.05; echo part2; }}
    {server_extra}
  }}
}}
",
        p = pki.dir.display()
    );
    std::fs::write(dir.join("nginx.conf"), conf).unwrap();
    let child = Command::new(bin).args(["-p", &dir.to_string_lossy(), "-c", &dir.join("nginx.conf").to_string_lossy(), "-e", &dir.join("error.log").to_string_lossy()]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < until, "nginx did not start: {}", std::fs::read_to_string(dir.join("error.log")).unwrap_or_default());
        std::thread::sleep(Duration::from_millis(20));
    }
    let client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    Some((Nginx { child, port, dir, _pki: pki }, client))
}

fn url(n: &Nginx, path: &str) -> Url {
    Url::parse(&format!("https://localhost:{}{path}", n.port)).unwrap()
}

fn gzip_get(c: &mut Client, u: &Url) -> std::sync::Arc<bare_net::http::Response> {
    c.send(&Request { method: Method::Get, url: u, headers: &[("Accept-Encoding", "gzip")], body: &[] }).unwrap()
}

#[test]
fn a_gzip_reply_with_no_content_length_is_decoded() {
    let Some((n, mut c)) = start("", "") else { return };
    // The reply is on HTTP/2, not HTTP/1.1.
    assert_eq!(String::from_utf8_lossy(&c.get(&url(&n, "/proto")).unwrap().body).trim(), "HTTP/2.0");
    let r = gzip_get(&mut c, &url(&n, "/text.txt"));
    assert_eq!((r.status, r.body == text(3 << 20)), (200, true));
    assert_eq!(r.header("content-encoding"), Some("gzip"));
    assert_eq!(r.header("content-length"), None);
    // A file that is too small to compress, and one that nginx does not compress, come as they are.
    let r = gzip_get(&mut c, &url(&n, "/small.txt"));
    assert_eq!((r.status, r.body.as_slice()), (200, &b"small file\n"[..]));
    let r = gzip_get(&mut c, &url(&n, "/bin.dat"));
    assert!(r.body == lcg(5 << 20));
    // Without a request for gzip the file is plain.
    let r = c.get(&url(&n, "/text.txt?plain")).unwrap();
    assert_eq!((r.header("content-encoding"), r.body == text(3 << 20)), (None, true));
}

#[test]
fn head_and_conditional_requests_on_a_gzip_file() {
    let Some((n, mut c)) = start("", "") else { return };
    let u = url(&n, "/text.txt");
    let r = c.send(&Request { method: Method::Head, url: &u, headers: &[("Accept-Encoding", "gzip")], body: &[] }).unwrap();
    assert_eq!((r.status, r.body.len(), r.header("content-encoding")), (200, 0, Some("gzip")));
    let etag = r.header("etag").unwrap().to_string();
    // 304: the headers may name the encoding, and there is no body.
    let r = c.send(&Request { method: Method::Get, url: &u, headers: &[("Accept-Encoding", "gzip"), ("If-None-Match", &etag)], body: &[] }).unwrap();
    assert_eq!((r.status, r.body.len()), (304, 0));
    let r = c.get(&url(&n, "/n204")).unwrap();
    assert_eq!((r.status, r.body.len()), (204, 0));
    let r = c.get(&url(&n, "/moved")).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"small file\n"[..]));
}

#[test]
fn twenty_requests_pass_a_server_that_allows_one_or_two_streams() {
    for max in [1, 2] {
        let Some((n, client)) = start(&format!("http2_max_concurrent_streams {max};"), "") else { return };
        let t = Instant::now();
        let tasks: Vec<_> = (0..20)
            .map(|i| {
                let u = url(&n, &format!("/slow?i={i}"));
                client.spawn(move |c| c.get(&u).map(|r| (r.status, String::from_utf8_lossy(&r.body).trim().to_string())).map_err(|e| e.to_string()))
            })
            .collect();
        let got: Vec<_> = tasks.into_iter().map(|t| t.wait().unwrap()).collect();
        let conns: std::collections::BTreeSet<_> = got.iter().map(|r| r.as_ref().unwrap_or_else(|e| panic!("max {max}: {e}")).1.clone()).collect();
        assert!(got.iter().all(|r| r.as_ref().unwrap().0 == 200), "max {max}");
        // The connect gate lets 2 connections open at once; more would mean that a stream was refused and run again elsewhere.
        assert!(conns.len() <= 2, "max {max}: connections {conns:?}");
        assert!(t.elapsed() < Duration::from_secs(5), "max {max}: {:?}", t.elapsed());
    }
}

#[test]
fn a_header_over_16_kib_goes_in_continuation_frames() {
    let Some((n, mut c)) = start("", "") else { return };
    for len in [10_000, 16_384, 40_000, 100_000] {
        let big = "v".repeat(len);
        let r = c.get_with(&url(&n, &format!("/hdr?{len}")), &[("X-Big", &big)]);
        assert_eq!((r.status, r.body.len()), (200, len + 1), "{len}");
    }
    // The table of the server stays in step: a plain request works after.
    assert_eq!(c.get(&url(&n, "/small.txt")).unwrap().status, 200);
    // Many cookies, as a page has: one header of 20 KB.
    let cookie = (0..800).map(|i| format!("c{i}=value{i}")).collect::<Vec<_>>().join("; ");
    let r = c.get_with(&url(&n, "/cookie"), &[("Cookie", &cookie)]);
    assert_eq!(String::from_utf8_lossy(&r.body).trim(), cookie);
}

trait GetWith {
    fn get_with(&mut self, u: &Url, headers: &[(&str, &str)]) -> std::sync::Arc<bare_net::http::Response>;
}

impl GetWith for Client {
    fn get_with(&mut self, u: &Url, headers: &[(&str, &str)]) -> std::sync::Arc<bare_net::http::Response> {
        self.send(&Request { method: Method::Get, url: u, headers, body: &[] }).unwrap()
    }
}

#[test]
fn small_data_frames_and_a_rate_limit() {
    // nginx cuts the body into frames of 64 bytes: about 3,100 frames for 200 KB.
    let Some((n, mut c)) = start("http2_chunk_size 64;", "") else { return };
    assert!(c.get(&url(&n, "/mid.dat")).unwrap().body == lcg(200_000));
    drop((n, c));
    // A rate limit: the 5 MiB come in over about 0.6 s, with the stream window empty and refilled many times.
    let Some((n, mut c)) = start("limit_rate 8m;", "") else { return };
    let t = Instant::now();
    assert!(c.get(&url(&n, "/bin.dat")).unwrap().body == lcg(5 << 20));
    assert!(t.elapsed() > Duration::from_millis(300), "{:?}", t.elapsed());
}

#[test]
fn an_upload_of_3_mib_comes_back_whole() {
    let Some((n, mut c)) = start("", "") else { return };
    let up = lcg(3 << 20);
    let r = c.post(&url(&n, "/upload"), &[("Content-Type", "application/octet-stream")], &up).unwrap();
    assert_eq!((r.status, r.body == up), (200, true));
}

#[test]
fn nginx_closes_idle_sessions_and_the_next_request_still_works() {
    let Some((n, mut c)) = start("keepalive_timeout 1s;", "") else { return };
    let id = |c: &mut Client, i: u64| String::from_utf8_lossy(&c.get(&url(&n, &format!("/id?{i}"))).unwrap().body).trim().to_string();
    let first = id(&mut c, 0);
    assert_eq!(first, id(&mut c, 1), "the session is kept");
    std::thread::sleep(Duration::from_millis(1600));
    assert_ne!(first, id(&mut c, 2), "a new connection after the idle close");
    // Requests around the moment of the close (1 s): a GET never fails, and a POST runs again if the server did not take it.
    for i in 0..30u64 {
        std::thread::sleep(Duration::from_millis(940 + i * 4));
        id(&mut c, 10 + i);
        let r = c.post(&url(&n, "/upload"), &[], b"abc").map_err(|e| e.to_string());
        assert_eq!(r.map(|r| r.body.clone()), Ok(b"abc".to_vec()), "post {i}");
    }
}

#[test]
fn trailers_and_a_reply_in_parts() {
    let Some((n, mut c)) = start("", "") else { return };
    let r = c.get(&url(&n, "/trailer")).unwrap();
    assert_eq!((r.status, String::from_utf8_lossy(&r.body).trim().to_string()), (200, "trailed".into()));
    let r = c.get(&url(&n, "/flush")).unwrap();
    assert_eq!(String::from_utf8_lossy(&r.body), "part1\npart2\n");
    // The same session served both.
    assert_eq!(c.get(&url(&n, "/id")).unwrap().status, 200);
}

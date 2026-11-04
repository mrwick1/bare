//! The scaling cases for the HTTP client. A server in this process, on 127.0.0.1, sends bad replies.
//! DNS is not tested here: the client finds the host `127.0.0.1` without a DNS query.

use crate::Case;
use bare_net::http::Client;
use bare_url::Url;
use std::hint::black_box;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

type Handler = Arc<dyn Fn(&mut TcpStream, usize) + Send + Sync>;

/// Start a server. It calls `handler` for each request, with the number of this request on the connection.
/// JS: like `http.createServer`, but we write the raw bytes ourselves.
fn serve(handler: Handler) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || connection(stream, handler));
        }
    });
    port
}

/// Read requests one by one and answer each. A request with `Content-Length` has a body: read it and throw it away,
/// so that the next request on the connection starts in the right place. The thread ends when the client closes the connection.
fn connection(mut stream: TcpStream, handler: Handler) {
    stream.set_nodelay(true).ok();
    let mut seen = Vec::new();
    let mut piece = vec![0u8; 65536];
    let mut count = 0;
    loop {
        let mut from = 0;
        // Search only the new bytes (and 3 before them), or a head of many reads costs a scan of everything for each read.
        let end = loop {
            if let Some(at) = seen[from..].windows(4).position(|w| w == b"\r\n\r\n") {
                break from + at + 4;
            }
            from = seen.len().saturating_sub(3);
            match stream.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
        let mut need = head.split("content-length: ").nth(1).and_then(|r| r.split("\r\n").next()).and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let have = (seen.len() - end).min(need);
        seen.drain(..end + have);
        need -= have;
        while need > 0 {
            match stream.read(&mut piece[..need.min(65536)]) {
                Ok(0) | Err(_) => return,
                Ok(n) => need -= n,
            }
        }
        handler(&mut stream, count);
        count += 1;
    }
}

/// Write `total` bytes of `fill`, in writes of `piece` bytes. Stop quietly when the client is gone.
fn pour(stream: &mut TcpStream, total: usize, piece: usize, fill: u8) {
    let buf = vec![fill; piece.min(total).max(1)];
    let mut left = total;
    while left > 0 {
        let n = left.min(buf.len());
        if stream.write_all(&buf[..n]).is_err() {
            return;
        }
        left -= n;
    }
}

/// A case that fetches `/` from a server and ignores the result. The server stays alive.
fn fetch(handler: impl Fn(&mut TcpStream, usize) + Send + Sync + 'static) -> Box<dyn FnOnce()> {
    let port = serve(Arc::new(handler));
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    Box::new(move || {
        black_box(Client::new().get(&url).map(|r| r.body.len()).map_err(|e| e.to_string()).ok());
    })
}

/// Like `fetch`, but the client reads the body in pieces of 64 KB and throws each away: the body is never in memory.
fn stream(handler: impl Fn(&mut TcpStream, usize) + Send + Sync + 'static) -> Box<dyn FnOnce()> {
    let port = serve(Arc::new(handler));
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    Box::new(move || {
        let Ok(mut streamed) = Client::new().get_stream(&url) else { return };
        let (mut piece, mut total) = (vec![0u8; 65536], 0usize);
        while let Ok(n) = streamed.body.read(&mut piece) {
            if n == 0 {
                break;
            }
            total += n;
        }
        black_box(total);
    })
}

/// A page of `n` KB of small divs. A case fetches it and builds the DOM: all in memory (`get`, `decode`, `parse`),
/// or in pieces (`get_stream`, `Decoder`, `Parser`).
fn page(n: usize, in_pieces: bool) -> Box<dyn FnOnce()> {
    let port = serve(Arc::new(move |s: &mut TcpStream, _| {
        let div = b"<div class=a>text &amp; more</div>\n";
        let total = n * 1000 / div.len() * div.len();
        s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {total}\r\n\r\n").as_bytes()).ok();
        for _ in 0..total / div.len() {
            if s.write_all(div).is_err() {
                return;
            }
        }
    }));
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    Box::new(move || {
        if !in_pieces {
            let Ok(r) = Client::new().get(&url) else { return };
            let text = bare_html::encoding::decode(&r.body, r.header("content-type"));
            black_box(bare_html::tree::parse(&text).len());
            return;
        }
        let Ok(mut streamed) = Client::new().get_stream(&url) else { return };
        let mut decoder = bare_html::encoding::Decoder::new(streamed.response.header("content-type"));
        let mut parser = bare_html::tree::Parser::new();
        let mut piece = vec![0u8; 65536];
        while let Ok(n) = streamed.body.read(&mut piece) {
            if n == 0 {
                break;
            }
            parser.feed(&decoder.feed(&piece[..n]));
        }
        parser.feed(&decoder.finish());
        black_box(parser.finish().len());
    })
}

fn case(name: &'static str, base: usize, build: fn(usize) -> Box<dyn FnOnce()>) -> Case {
    Case { name, base, build, single: false }
}

pub fn all() -> Vec<Case> {
    vec![
        // `n` is in KB for the body cases.
        case("net-body-length", 1_000, |n| {
            fetch(move |s, _| {
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", n * 1000).as_bytes()).ok();
                pour(s, n * 1000, 65536, b'x');
            })
        }),
        case("net-body-trickle", 100, |n| {
            fetch(move |s, _| {
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", n * 1000).as_bytes()).ok();
                pour(s, n * 1000, 1000, b'x');
            })
        }),
        // `n` is in KB: 5,000 is a page of 5 MB.
        case("net-page-whole", 5_000, |n| page(n, false)),
        case("net-page-stream", 5_000, |n| page(n, true)),
        case("net-stream-length", 1_000, |n| {
            stream(move |s, _| {
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", n * 1000).as_bytes()).ok();
                pour(s, n * 1000, 65536, b'x');
            })
        }),
        case("net-stream-chunked", 100, |n| {
            stream(move |s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").ok();
                for _ in 0..n * 10 {
                    s.write_all(b"3e8\r\n").ok();
                    pour(s, 1000, 1000, b'x');
                    s.write_all(b"\r\n").ok();
                }
                s.write_all(b"0\r\n\r\n").ok();
            })
        }),
        case("net-body-chunked-big", 100, |n| {
            fetch(move |s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").ok();
                for _ in 0..n {
                    s.write_all(b"3e8\r\n").ok();
                    pour(s, 1000, 1000, b'x');
                    s.write_all(b"\r\n").ok();
                }
                s.write_all(b"0\r\n\r\n").ok();
            })
        }),
        case("net-body-chunked-tiny", 2_000, |n| {
            fetch(move |s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").ok();
                let all: Vec<u8> = b"1\r\nx\r\n".repeat(n);
                s.write_all(&all).ok();
                s.write_all(b"0\r\n\r\n").ok();
            })
        }),
        case("net-body-chunked-tiny-trickle", 2_000, |n| {
            fetch(move |s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").ok();
                for _ in 0..n {
                    // One chunk for each write: the client reads small pieces.
                    s.write_all(b"1\r\nx\r\n").ok();
                }
                s.write_all(b"0\r\n\r\n").ok();
            })
        }),
        case("net-headers-many", 1_000, |n| {
            fetch(move |s, _| {
                let mut head = String::from("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n");
                head += &"X-A: b\r\n".repeat(n);
                head += "\r\n";
                s.write_all(head.as_bytes()).ok();
            })
        }),
        case("net-header-long", 10_000, |n| {
            fetch(move |s, _| {
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-A: {}\r\n\r\n", "b".repeat(n)).as_bytes()).ok();
            })
        }),
        case("net-status-line-long", 10_000, |n| {
            fetch(move |s, _| {
                s.write_all(format!("HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n", "x".repeat(n)).as_bytes()).ok();
            })
        }),
        case("net-cache-entries", 500, |n| {
            let port = serve(Arc::new(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nCache-Control: max-age=3600\r\nContent-Length: 2\r\n\r\nhi").ok();
            }));
            Box::new(move || {
                let mut client = Client::new();
                for i in 0..n {
                    let url = Url::parse(&format!("http://127.0.0.1:{port}/p{i}")).unwrap();
                    black_box(client.get(&url).ok());
                }
                black_box(&client);
            })
        }),
        case("net-cache-big-bodies", 50, |n| {
            let port = serve(Arc::new(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nCache-Control: max-age=3600\r\nContent-Length: 1000000\r\n\r\n").ok();
                pour(s, 1_000_000, 65536, b'x');
            }));
            Box::new(move || {
                let mut client = Client::new();
                for i in 0..n {
                    let url = Url::parse(&format!("http://127.0.0.1:{port}/p{i}")).unwrap();
                    black_box(client.get(&url).ok());
                }
                black_box(&client);
            })
        }),
        // `n` is in KB: one POST of `n` KB, written from the slice of the caller. The server reads it and answers.
        case("net-post-body", 1_000, |n| {
            let port = serve(Arc::new(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").ok();
            }));
            let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
            let body = vec![b'x'; n * 1000];
            Box::new(move || {
                black_box(Client::new().post(&url, &[("Content-Type", "application/octet-stream")], &body).map(|r| r.status).map_err(|e| e.to_string()).ok());
            })
        }),
        // `n` JSON login-sized POSTs over one keep-alive connection: the cost of one request, many times.
        case("net-post-small", 1_000, |n| {
            let port = serve(Arc::new(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").ok();
            }));
            let url = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();
            Box::new(move || {
                let mut client = Client::new();
                let headers = [("Content-Type", "application/json"), ("Authorization", "Bearer abcdefghijklmnopqrstuvwxyz")];
                for _ in 0..n {
                    black_box(client.post(&url, &headers, br#"{"email":"user@example.com","password":"correct horse"}"#).ok());
                }
            })
        }),
        // `n` chains of POST, 303, GET: the redirect rewrite (method, body, headers, URL join) many times.
        case("net-post-redirect", 500, |n| {
            let port = serve(Arc::new(|s, count| {
                let reply: &[u8] = if count % 2 == 0 {
                    b"HTTP/1.1 303 See Other\r\nLocation: /home\r\nContent-Length: 0\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
                };
                s.write_all(reply).ok();
            }));
            let url = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();
            Box::new(move || {
                let mut client = Client::new();
                let headers = [("Content-Type", "application/json"), ("Authorization", "Bearer t")];
                for _ in 0..n {
                    black_box(client.post(&url, &headers, b"{}").ok());
                }
            })
        }),
        // One request with `n` custom headers: validation and the head. The server sees a head of up to 3.2 MB.
        case("net-post-many-headers", 100, |n| {
            let port = serve(Arc::new(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").ok();
            }));
            let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
            let names: Vec<String> = (0..n * 100).map(|i| format!("X-Header-{i}")).collect();
            Box::new(move || {
                let headers: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "value-value")).collect();
                black_box(Client::new().post(&url, &headers, b"{}").ok());
            })
        }),
        case("net-redirect-loop", 1, |_| {
            fetch(|s, _| {
                s.write_all(b"HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n").ok();
            })
        }),
        // From here the server never stops. A good client gives up at some size or time.
        single("net-endless-headers", |_| {
            fetch(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\n").ok();
                loop {
                    if s.write_all(&b"X-A: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\r\n".repeat(1000)).is_err() {
                        return;
                    }
                }
            })
        }),
        single("net-stream-endless", |_| {
            // The client reads and throws away. It must stop at the size limit, with almost no memory.
            stream(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").ok();
                loop {
                    pour(s, 65536, 65536, b'x');
                }
            })
        }),
        single("net-endless-body", |_| {
            fetch(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").ok();
                loop {
                    pour(s, 65536, 65536, b'x');
                }
            })
        }),
        single("net-endless-chunks", |_| {
            fetch(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").ok();
                loop {
                    if s.write_all(&b"1\r\nx\r\n".repeat(10000)).is_err() {
                        return;
                    }
                }
            })
        }),
        single("net-huge-content-length", |_| {
            // The server promises 1 TB and sends a little. The client must not reserve the memory.
            fetch(|s, _| {
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000000\r\n\r\nhello").ok();
                std::thread::sleep(std::time::Duration::from_secs(60));
            })
        }),
        single("net-silent-server", |_| {
            fetch(|_, _| std::thread::sleep(std::time::Duration::from_secs(60)))
        }),
        single("net-drip-server", |_| {
            // One byte each 5 seconds: each read succeeds, so a limit for each read never fires.
            fetch(|s, _| {
                for b in b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".iter().chain(std::iter::repeat(&b'x').take(100)) {
                    if s.write_all(&[*b]).is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            })
        }),
    ]
}

fn single(name: &'static str, build: fn(usize) -> Box<dyn FnOnce()>) -> Case {
    Case { name, base: 1, build, single: true }
}

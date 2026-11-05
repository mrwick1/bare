//! A small loopback HTTP server for the resource tests and the load harness. It is not part of the browser.
//! It answers each request with a `Reply` that the handler builds from the request path.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// The bytes of a reply body.
pub enum Payload {
    Bytes(Vec<u8>),
    /// `n` bytes of `b'a'`, written from one small buffer: a big body that is never all in the server memory.
    Filler(usize),
}

impl Payload {
    fn len(&self) -> usize {
        match self {
            Payload::Bytes(b) => b.len(),
            Payload::Filler(n) => *n,
        }
    }
}

/// How the server puts the body on the wire.
pub enum Shape {
    /// `Content-Length`, written in one go.
    Length,
    /// `Transfer-Encoding: chunked`, with chunks of this size.
    Chunked(usize),
    /// `Content-Length`, written in pieces of this size with a flush after each one (the client sees short reads).
    Short(usize),
    /// `Content-Length` is `extra` bytes more than the body, then the connection closes.
    Truncated(usize),
    /// `Content-Length` is this number. The body is as long as it is. Used for a length that is far too big.
    Declared(u64),
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Payload,
    pub shape: Shape,
}

impl Reply {
    pub fn new(status: u16, content_type: Option<&str>, body: impl Into<Vec<u8>>) -> Reply {
        let headers = content_type.map(|t| vec![("Content-Type".to_string(), t.to_string())]).unwrap_or_default();
        Reply { status, headers, body: Payload::Bytes(body.into()), shape: Shape::Length }
    }

    pub fn js(body: impl Into<Vec<u8>>) -> Reply {
        Reply::new(200, Some("application/javascript; charset=UTF-8"), body)
    }

    pub fn css(body: impl Into<Vec<u8>>) -> Reply {
        Reply::new(200, Some("text/css; charset=UTF-8"), body)
    }

    pub fn shaped(mut self, shape: Shape) -> Reply {
        self.shape = shape;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

pub type Handler = Arc<dyn Fn(&str) -> Reply + Send + Sync>;

/// The server: its port, and the head of every request it got (lowercase), in order.
pub struct Server {
    pub port: u16,
    pub heads: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// How many requests asked for this path (the query is part of the path).
    pub fn hits(&self, path: &str) -> usize {
        let line = format!("get {path} http/1.1");
        self.heads.lock().unwrap().iter().filter(|h| h.starts_with(&line)).count()
    }

    pub fn total(&self) -> usize {
        self.heads.lock().unwrap().len()
    }
}

/// Start a server on a free port. One thread for each connection. The threads end when the client closes.
pub fn serve(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let (handler, log) = (Arc::new(handler) as Handler, heads.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (handler, log) = (handler.clone(), log.clone());
            std::thread::spawn(move || connection(stream, handler, log));
        }
    });
    Server { port, heads }
}

fn connection(mut stream: TcpStream, handler: Handler, log: Arc<Mutex<Vec<String>>>) {
    stream.set_nodelay(true).ok();
    let mut seen = Vec::new();
    let mut piece = [0u8; 4096];
    loop {
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match stream.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
        seen.drain(..end);
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        // A request body is read and logged after the head (lowercase), so it is not taken for the next request.
        let want = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        while seen.len() < want {
            match stream.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        let body = String::from_utf8_lossy(&seen[..want]).to_ascii_lowercase();
        seen.drain(..want);
        log.lock().unwrap().push(format!("{head}{body}"));
        // The handler is given the path as the client wrote it (lowercase: tests use lowercase paths).
        let reply = handler(&path);
        if respond(&mut stream, reply).is_err() {
            return;
        }
    }
}

fn respond(stream: &mut TcpStream, reply: Reply) -> std::io::Result<()> {
    let len = reply.body.len();
    let mut head = format!("HTTP/1.1 {} X\r\n", reply.status);
    for (n, v) in &reply.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    let close = matches!(reply.shape, Shape::Truncated(_));
    match reply.shape {
        Shape::Chunked(_) => head.push_str("Transfer-Encoding: chunked\r\n"),
        Shape::Truncated(extra) => head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n", len + extra)),
        Shape::Declared(n) => head.push_str(&format!("Content-Length: {n}\r\n")),
        _ => head.push_str(&format!("Content-Length: {len}\r\n")),
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    let filler = [b'a'; 64 * 1024];
    let mut pos = 0;
    let step = match reply.shape {
        Shape::Chunked(n) | Shape::Short(n) => n.max(1),
        _ => 64 * 1024,
    };
    while pos < len {
        let n = step.min(len - pos);
        let part: &[u8] = match &reply.body {
            Payload::Bytes(b) => &b[pos..pos + n],
            Payload::Filler(_) => &filler[..n.min(filler.len())],
        };
        let n = part.len();
        if let Shape::Chunked(_) = reply.shape {
            stream.write_all(format!("{n:x}\r\n").as_bytes())?;
            stream.write_all(part)?;
            stream.write_all(b"\r\n")?;
        } else {
            stream.write_all(part)?;
            if let Shape::Short(_) = reply.shape {
                stream.flush()?;
            }
        }
        pos += n;
    }
    if let Shape::Chunked(_) = reply.shape {
        stream.write_all(b"0\r\n\r\n")?;
    }
    if close {
        stream.shutdown(std::net::Shutdown::Both).ok();
        return Err(std::io::ErrorKind::BrokenPipe.into());
    }
    Ok(())
}

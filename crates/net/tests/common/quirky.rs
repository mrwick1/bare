//! A loopback HTTP server whose connection habits are chosen by the test. Shared by the net tests and the page tests
//! (`#[path]` include). It is not part of the browser.
//!
//! - `Style` says how one reply ends: kept open, `Connection: close`, HTTP/1.0 with a body that ends at the close, or a
//!   close with no `Connection` header (the client finds out when it reuses the connection).
//! - `Limit` caps the connections served at once. A connection over the cap waits, or is closed at once. A server at its cap
//!   closes idle kept-alive connections for a waiting one, as real servers do. The close is an event, not a timer.
//! - The server logs `METHOD /path` when it has read a request, before it writes the reply.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    KeepAlive,
    /// `Connection: close`, then the server closes.
    CloseHeader,
    /// `HTTP/1.0`, no `Content-Length`: the body ends where the server closes.
    Http10,
    /// `Content-Length`, no `Connection` header, and the server closes right after the reply.
    SilentClose,
}

#[derive(Clone, Copy, Debug)]
pub enum Limit {
    /// At most this many connections are served at once. A new one waits for a free place.
    Wait(usize),
    /// As `Wait`, but a new connection over the cap is closed at once, with no reply.
    Refuse(usize),
}

#[derive(Clone)]
pub struct Config {
    /// The style of the reply to the request with this number (counted from 0 over all connections).
    pub style: fn(usize) -> Style,
    pub delay: Duration,
    pub limit: Option<Limit>,
}

impl Config {
    pub fn new(style: fn(usize) -> Style, delay: Duration) -> Config {
        Config { style, delay, limit: None }
    }
}

pub struct Server {
    pub port: u16,
    log: Arc<Mutex<Vec<String>>>,
    /// Connections the server accepted.
    pub accepted: Arc<AtomicUsize>,
    /// The most connections served at once.
    pub peak: Arc<AtomicUsize>,
    /// Connections closed at once because of `Limit::Refuse`.
    pub refused: Arc<AtomicUsize>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The requests read so far, as `METHOD /path`, in order.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn hits(&self, method: &str, path: &str) -> usize {
        let line = format!("{method} {path}");
        self.requests().iter().filter(|r| **r == line).count()
    }

    pub fn total(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

/// `(content type, body)` of a path, or `None` for a 404. The method is the first argument.
pub type Pages = Arc<dyn Fn(&str, &str) -> Option<(&'static str, Vec<u8>)> + Send + Sync>;

#[derive(Default)]
struct State {
    live: usize,
    waiting: usize,
    /// Kept-alive connections that wait for their next request: (id, a handle to close them).
    idle: Vec<(usize, TcpStream)>,
    /// Ids closed to make room. Their place was freed at the close, so their thread frees nothing when it ends.
    evicted: Vec<usize>,
}

struct Shared {
    cfg: Config,
    pages: Pages,
    log: Arc<Mutex<Vec<String>>>,
    state: Mutex<State>,
    free: Condvar,
    next_request: AtomicUsize,
    next_id: AtomicUsize,
    peak: Arc<AtomicUsize>,
}

pub fn start(cfg: Config, pages: impl Fn(&str, &str) -> Option<(&'static str, Vec<u8>)> + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted, refused, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let log = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::new(Shared {
        cfg,
        pages: Arc::new(pages),
        log: log.clone(),
        state: Mutex::new(State::default()),
        free: Condvar::new(),
        next_request: AtomicUsize::new(0),
        next_id: AtomicUsize::new(0),
        peak: peak.clone(),
    });
    let (count, no) = (accepted.clone(), refused.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            count.fetch_add(1, Ordering::SeqCst);
            let shared = shared.clone();
            let cap = match shared.cfg.limit {
                None => None,
                Some(Limit::Wait(n)) | Some(Limit::Refuse(n)) => Some(n),
            };
            let mut st = shared.state.lock().unwrap();
            if let Some(cap) = cap
                && st.live >= cap
            {
                // At the cap: free the places that idle connections hold.
                let idle: Vec<_> = st.idle.drain(..).collect();
                for (id, s) in idle {
                    let _ = s.shutdown(Shutdown::Both);
                    st.live -= 1;
                    st.evicted.push(id);
                }
                if st.live >= cap {
                    if matches!(shared.cfg.limit, Some(Limit::Refuse(_))) {
                        no.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    st.waiting += 1;
                    drop(st);
                    std::thread::spawn(move || {
                        let mut st = shared.state.lock().unwrap();
                        while st.live >= cap {
                            st = shared.free.wait(st).unwrap();
                        }
                        st.waiting -= 1;
                        admit(&shared, st, stream);
                    });
                    continue;
                }
            }
            admit(&shared, st, stream);
        }
    });
    Server { port, log, accepted, peak, refused }
}

/// Take a place and serve the connection on a new thread.
fn admit(shared: &Arc<Shared>, mut st: std::sync::MutexGuard<'_, State>, stream: TcpStream) {
    st.live += 1;
    shared.peak.fetch_max(st.live, Ordering::SeqCst);
    drop(st);
    let shared = shared.clone();
    std::thread::spawn(move || connection(stream, shared));
}

fn connection(mut stream: TcpStream, sh: Arc<Shared>) {
    let id = sh.next_id.fetch_add(1, Ordering::SeqCst);
    stream.set_nodelay(true).ok();
    let mut seen = Vec::new();
    let mut served = 0;
    let mut piece = [0u8; 4096];
    loop {
        // Wait for the next request. A kept-alive connection is idle now, and a waiting connection may take its place.
        {
            let mut st = sh.state.lock().unwrap();
            if served > 0 && st.waiting > 0 {
                st.live -= 1;
                sh.free.notify_all();
                return;
            }
            // A new connection is not idle: its first request is on the way, so it is never closed to make room.
            if seen.is_empty() && served > 0 {
                st.idle.push((id, stream.try_clone().unwrap()));
            }
        }
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match stream.read(&mut piece) {
                Ok(0) | Err(_) => return leave(&sh, id),
                Ok(n) => {
                    if seen.is_empty() && served > 0 && !busy(&sh, id) {
                        return; // This connection was closed to make room: its request is lost, the client sees a close.
                    }
                    seen.extend_from_slice(&piece[..n]);
                }
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_string();
        seen.drain(..end);
        let mut words = head.split_whitespace();
        let (method, path) = (words.next().unwrap_or("").to_string(), words.next().unwrap_or("/").to_string());
        let want = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
        while seen.len() < want {
            match stream.read(&mut piece) {
                Ok(0) | Err(_) => return leave(&sh, id),
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        seen.drain(..want);
        sh.log.lock().unwrap().push(format!("{method} {path}"));
        let style = (sh.cfg.style)(sh.next_request.fetch_add(1, Ordering::SeqCst));
        if !sh.cfg.delay.is_zero() {
            std::thread::sleep(sh.cfg.delay);
        }
        let (status, ctype, body) = match (sh.pages)(&method, &path) {
            Some((t, b)) => ("200 OK", t, b),
            None => ("404 Not Found", "text/plain", b"no".to_vec()),
        };
        let mut out = Vec::new();
        match style {
            Style::KeepAlive => out.extend(format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n", body.len()).bytes()),
            Style::CloseHeader => out.extend(format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).bytes()),
            Style::Http10 => out.extend(format!("HTTP/1.0 {status}\r\nContent-Type: {ctype}\r\n\r\n").bytes()),
            Style::SilentClose => out.extend(format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n", body.len()).bytes()),
        }
        out.extend(body);
        if stream.write_all(&out).is_err() {
            return leave(&sh, id);
        }
        served += 1;
        if style != Style::KeepAlive {
            let _ = stream.shutdown(Shutdown::Both);
            let mut st = sh.state.lock().unwrap();
            st.live -= 1;
            sh.free.notify_all();
            return;
        }
    }
}

/// The connection got bytes: it is not idle. False when it was already closed to make room.
fn busy(sh: &Shared, id: usize) -> bool {
    let mut st = sh.state.lock().unwrap();
    match st.idle.iter().position(|(i, _)| *i == id) {
        Some(at) => {
            st.idle.remove(at);
            true
        }
        None => {
            st.evicted.retain(|i| *i != id);
            false
        }
    }
}

/// The client closed the connection, or a read or write failed, or the connection was closed to make room.
fn leave(sh: &Shared, id: usize) {
    let mut st = sh.state.lock().unwrap();
    if let Some(at) = st.evicted.iter().position(|i| *i == id) {
        st.evicted.remove(at);
        return;
    }
    st.idle.retain(|(i, _)| *i != id);
    st.live -= 1;
    sh.free.notify_all();
}

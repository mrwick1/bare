//! WebSocket client (RFC 6455) on a new HTTP/1.1 connection of `http.rs`.
//!
//! - `connect` sends the opening handshake: the cookies of the jar, the identity of the client, `Origin`, a random key and the
//!   subprotocols. The reply must be `101` with the right `Sec-WebSocket-Accept`. No extension is offered, so one in the reply fails.
//! - The connection is split in a `Reader` and a `Writer`, as the HTTP/2 client does (`h2.rs`). The reader waits on the socket
//!   (`peek`), then takes the stream for one read only. So a writer never waits behind a reader that waits for the peer, and no
//!   thread polls. A writer takes the stream for one frame.
//! - The reader puts fragments together, answers a ping with a pong and a close with a close. It checks each frame: a masked
//!   frame, a set RSV bit, an unknown opcode or a bad control frame is a protocol error (1002), a text that is not UTF-8 is 1007,
//!   and a message over the cap of the caller is 1009. The reader then sends a close with that code and stops.
//! - Each frame of the client is masked with 4 bytes from the OS random source. A `Writer` takes them 64 at a time.

use crate::http::{Client, Stream, proxy_base64};
use bare_url::Url;
use std::io::{self, ErrorKind, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

type Error = Box<dyn std::error::Error>;

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const CONT: u8 = 0;
pub const TEXT: u8 = 1;
pub const BINARY: u8 = 2;
const CLOSE: u8 = 8;
const PING: u8 = 9;
const PONG: u8 = 10;

/// A message from the server.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    /// The server closed: its code (1005 when it sent none) and reason. The reader answered with a close.
    Close(u16, String),
}

/// Why the reader stopped without a close of the server.
#[derive(Debug)]
pub enum Stop {
    /// The server broke the protocol. The reader sent a close with this code.
    Protocol(u16, &'static str),
    /// The connection failed or ended.
    Io(io::Error),
}

struct Shared {
    io: Mutex<Stream>,
    sock: TcpStream,
    /// A close frame was sent.
    closed: AtomicBool,
}

fn lock(m: &Mutex<Stream>) -> MutexGuard<'_, Stream> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Open a WebSocket. `url` has the scheme `http` or `https` (for `ws` and `wss`). Gives the reader, the writer and the subprotocol
/// that the server chose (empty when none).
pub fn connect(client: &Client, url: &Url, protocols: &[String], origin: Option<&str>, max: usize) -> Result<(Reader, Writer, String), Error> {
    let mut key = [0u8; 16];
    bare_os::random::fill(&mut key)?;
    let key = proxy_base64(&key);
    let offer = protocols.join(", ");
    let mut headers = vec![("Pragma", "no-cache"), ("Cache-Control", "no-cache"), ("Upgrade", "websocket"), ("Connection", "Upgrade"), ("Sec-WebSocket-Key", key.as_str()), ("Sec-WebSocket-Version", "13")];
    if let Some(o) = origin {
        headers.push(("Origin", o));
    }
    if !protocols.is_empty() {
        headers.push(("Sec-WebSocket-Protocol", offer.as_str()));
    }
    let (stream, reply, rest) = client.upgrade(url, &headers)?;
    if reply.status != 101 {
        return Err(format!("Error during WebSocket handshake: Unexpected response code: {}", reply.status).into());
    }
    if !reply.header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        return Err("Error during WebSocket handshake: 'Upgrade' header value is not 'websocket'".into());
    }
    if !reply.headers.iter().filter(|(n, _)| n == "connection").any(|(_, v)| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade"))) {
        return Err("Error during WebSocket handshake: 'Connection' header value is not 'Upgrade'".into());
    }
    let mut seed = key.into_bytes();
    seed.extend_from_slice(GUID);
    if reply.header("sec-websocket-accept") != Some(proxy_base64(&bare_tls::sha1::sha1(&seed)).as_str()) {
        return Err("Error during WebSocket handshake: Incorrect 'Sec-WebSocket-Accept' header value".into());
    }
    if reply.header("sec-websocket-extensions").is_some_and(|v| !v.trim().is_empty()) {
        return Err("Error during WebSocket handshake: Received an extension that was not offered".into());
    }
    let protocol = reply.header("sec-websocket-protocol").unwrap_or("").trim().to_string();
    if !protocol.is_empty() && !protocols.contains(&protocol) {
        return Err("Error during WebSocket handshake: 'Sec-WebSocket-Protocol' header value is not one that was offered".into());
    }
    let sock = stream.socket().ok_or("no socket")?.try_clone()?;
    let sh = Arc::new(Shared { io: Mutex::new(stream), sock, closed: AtomicBool::new(false) });
    let w = Writer::new(sh.clone());
    Ok((Reader { sh, buf: rest, frag: None, w: w.clone(), max }, w, protocol))
}

/// Sends frames. A clone shares the connection.
pub struct Writer {
    sh: Arc<Shared>,
    keys: [u8; 64],
    used: usize,
}

impl Clone for Writer {
    fn clone(&self) -> Writer {
        Writer::new(self.sh.clone())
    }
}

impl Writer {
    fn new(sh: Arc<Shared>) -> Writer {
        Writer { sh, keys: [0; 64], used: 64 }
    }

    /// Send a text (`TEXT`) or binary (`BINARY`) message in one frame.
    pub fn send(&mut self, op: u8, data: &[u8]) -> io::Result<()> {
        self.frame(op, data)
    }

    /// Send a close frame, once. `code` `None` sends an empty body (the page gives no code). The reader goes on until the server
    /// answers.
    pub fn close(&mut self, code: Option<u16>, reason: &str) -> io::Result<()> {
        if self.sh.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut body = Vec::with_capacity(2 + reason.len());
        if let Some(c) = code {
            body.extend_from_slice(&c.to_be_bytes());
            body.extend_from_slice(reason.as_bytes());
        }
        self.frame(CLOSE, &body)
    }

    /// End the connection now (the page went away).
    pub fn shutdown(&self) {
        let _ = self.sh.sock.shutdown(Shutdown::Both);
    }

    fn frame(&mut self, op: u8, data: &[u8]) -> io::Result<()> {
        if self.used + 4 > self.keys.len() {
            bare_os::random::fill(&mut self.keys)?;
            self.used = 0;
        }
        let key: [u8; 4] = self.keys[self.used..self.used + 4].try_into().expect("4 bytes");
        self.used += 4;
        let mut out = Vec::with_capacity(14 + data.len());
        out.push(0x80 | op);
        match data.len() {
            n @ 0..=125 => out.push(0x80 | n as u8),
            n @ 126..=0xffff => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&key);
        let start = out.len();
        out.extend_from_slice(data);
        mask(&mut out[start..], key);
        let mut io = lock(&self.sh.io);
        io.write_all(&out)?;
        io.flush()
    }
}

/// XOR with the key, 4 bytes at a time.
fn mask(data: &mut [u8], key: [u8; 4]) {
    let mut chunks = data.chunks_exact_mut(4);
    for c in &mut chunks {
        for i in 0..4 {
            c[i] ^= key[i];
        }
    }
    for (i, b) in chunks.into_remainder().iter_mut().enumerate() {
        *b ^= key[i];
    }
}

/// Reads messages. Only one thread reads.
pub struct Reader {
    sh: Arc<Shared>,
    buf: Vec<u8>,
    /// The opcode and the bytes of a message in fragments.
    frag: Option<(u8, Vec<u8>)>,
    w: Writer,
    max: usize,
}

impl Reader {
    /// The next message. After a `Close` or a `Stop`, do not call it again.
    pub fn recv(&mut self) -> Result<Message, Stop> {
        match self.next_inner() {
            Err(Stop::Protocol(code, why)) => {
                let _ = self.w.close(Some(code), "");
                self.w.shutdown();
                Err(Stop::Protocol(code, why))
            }
            r => r,
        }
    }

    fn next_inner(&mut self) -> Result<Message, Stop> {
        loop {
            self.fill(2)?;
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (fin, op) = (b0 & 0x80 != 0, b0 & 0x0f);
            if b0 & 0x70 != 0 {
                return Err(Stop::Protocol(1002, "a frame has an RSV bit set"));
            }
            if b1 & 0x80 != 0 {
                return Err(Stop::Protocol(1002, "a frame of the server is masked"));
            }
            let (head, len) = match b1 & 0x7f {
                126 => {
                    self.fill(4)?;
                    (4, u16::from_be_bytes([self.buf[2], self.buf[3]]) as u64)
                }
                127 => {
                    self.fill(10)?;
                    (10, u64::from_be_bytes(self.buf[2..10].try_into().expect("8 bytes")))
                }
                n => (2, n as u64),
            };
            let control = op >= 8;
            if control && (len > 125 || !fin) {
                return Err(Stop::Protocol(1002, "a control frame is too long or in fragments"));
            }
            let have = self.frag.as_ref().map_or(0, |f| f.1.len()) as u64;
            if len.saturating_add(have) > self.max as u64 {
                return Err(Stop::Protocol(1009, "a message is bigger than the cap"));
            }
            let len = len as usize;
            self.fill(head + len)?;
            let payload: Vec<u8> = self.buf.drain(..head + len).skip(head).collect();
            match op {
                PING => self.w.frame(PONG, &payload).map_err(Stop::Io)?,
                PONG => {}
                CLOSE => return self.on_close(&payload),
                TEXT | BINARY if self.frag.is_some() => return Err(Stop::Protocol(1002, "a new message started inside a message in fragments")),
                TEXT | BINARY if !fin => self.frag = Some((op, payload)),
                TEXT | BINARY => return message(op, payload),
                CONT => {
                    let Some((first, mut data)) = self.frag.take() else {
                        return Err(Stop::Protocol(1002, "a continuation frame has no message to continue"));
                    };
                    data.extend_from_slice(&payload);
                    if fin {
                        return message(first, data);
                    }
                    self.frag = Some((first, data));
                }
                _ => return Err(Stop::Protocol(1002, "a frame has an unknown opcode")),
            }
        }
    }

    fn on_close(&mut self, payload: &[u8]) -> Result<Message, Stop> {
        let (code, reason) = match payload {
            [] => (1005, String::new()),
            [_] => return Err(Stop::Protocol(1002, "a close frame has a body of 1 byte")),
            [a, b, rest @ ..] => {
                let code = u16::from_be_bytes([*a, *b]);
                if !matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999) {
                    return Err(Stop::Protocol(1002, "a close frame has a code that is not allowed"));
                }
                let Ok(reason) = std::str::from_utf8(rest) else { return Err(Stop::Protocol(1007, "the reason of a close frame is not UTF-8")) };
                (code, reason.to_string())
            }
        };
        // Answer with the same code (none when the server sent none), then end the connection.
        let _ = self.w.close((code != 1005).then_some(code), "");
        self.w.shutdown();
        Ok(Message::Close(code, reason))
    }

    /// Read until `buf` holds `need` bytes. Waits on the socket, not on the stream (see the module docs).
    fn fill(&mut self, need: usize) -> Result<(), Stop> {
        let mut piece = [0u8; 16 << 10];
        while self.buf.len() < need {
            if !lock(&self.sh.io).has_buffered() {
                match self.sh.sock.peek(&mut [0u8; 1]) {
                    Ok(_) => {}
                    // Silence: an open socket may stay idle for as long as the page lives. After our close, the server has the read
                    // time limit to answer.
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) && !self.sh.closed.load(Ordering::Acquire) => continue,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Stop::Io(e)),
                }
            }
            match lock(&self.sh.io).read_ready(&mut piece) {
                Ok(Some(0)) => return Err(Stop::Io(ErrorKind::UnexpectedEof.into())),
                Ok(Some(n)) => self.buf.extend_from_slice(&piece[..n]),
                Ok(None) => {}
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(e) => return Err(Stop::Io(e)),
            }
        }
        Ok(())
    }
}

fn message(op: u8, data: Vec<u8>) -> Result<Message, Stop> {
    if op == BINARY {
        return Ok(Message::Binary(data));
    }
    String::from_utf8(data).map(Message::Text).map_err(|_| Stop::Protocol(1007, "a text message is not UTF-8"))
}

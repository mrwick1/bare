//! HTTP/2 client (RFC 9113) over the TLS stream. One `Conn` for each origin; many requests at once, each on its own stream.
//!
//! The client is synchronous, so a `Conn` has one reader thread. The thread waits for bytes on the socket (`peek`), then takes the
//! TLS stream for one `read`, so a writer never waits behind a reader that waits for the peer. Writers take the stream for one
//! write each. The streams share the state in `State`; a thread that waits for a change sleeps on one `Condvar`.
//!
//! Limits: a frame is at most 16 KiB (we never raise our setting); a header block at most `MAX_BLOCK` bytes in all its frames; a
//! header list at most `MAX_HEADER_LIST`; we receive at most `STREAM_WINDOW` bytes for a stream, and `CONN_WINDOW` in all, that the
//! caller has not read (a `WINDOW_UPDATE` follows the reads, not the arrival). Push is off (`SETTINGS_ENABLE_PUSH` 0).
//!
//! Lock order: `io`, then `st`. The reader never takes `io` while it holds `st`.

use crate::{hpack, trace};
use bare_tls::client::ClientStream;
use std::collections::{HashMap, VecDeque};
use std::error::Error as StdError;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

type Error = Box<dyn StdError>;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
/// What we let the server send for one stream, and for all streams, before we read.
const STREAM_WINDOW: u32 = 1 << 20;
const CONN_WINDOW: u32 = 4 << 20;
/// The largest frame we take. This is the default of the protocol: we never ask for more.
const MAX_FRAME: usize = 16 * 1024;
/// The header list we accept (the sum of name + value + 32 for each header, as the protocol counts it), and the encoded block.
const MAX_HEADER_LIST: usize = 256 * 1024;
const MAX_BLOCK: usize = 256 * 1024;
/// A read that waits this long with no byte from the server is an error. A connection with no stream that is silent this long closes.
const SILENCE: Duration = Duration::from_secs(30);
/// Until the server says its own limit (RFC 9113 section 6.5.2 allows any; browsers assume 100).
const FIRST_MAX_STREAMS: u32 = 100;
const MAX_ID: u32 = (1 << 31) - 1;

const DATA: u8 = 0;
const HEADERS: u8 = 1;
const PRIORITY: u8 = 2;
const RST_STREAM: u8 = 3;
const SETTINGS: u8 = 4;
const PUSH_PROMISE: u8 = 5;
const PING: u8 = 6;
const GOAWAY: u8 = 7;
const WINDOW_UPDATE: u8 = 8;
const CONTINUATION: u8 = 9;

const END_STREAM: u8 = 1;
const ACK: u8 = 1;
const END_HEADERS: u8 = 4;
const PADDED: u8 = 8;
const PRIORITY_FLAG: u8 = 0x20;

const NO_ERROR: u32 = 0;
const PROTOCOL_ERROR: u32 = 1;
const FLOW_CONTROL_ERROR: u32 = 3;
const STREAM_CLOSED: u32 = 5;
const FRAME_SIZE_ERROR: u32 = 6;
const REFUSED_STREAM: u32 = 7;
const CANCEL: u32 = 8;
const COMPRESSION_ERROR: u32 = 9;
const ENHANCE_YOUR_CALM: u32 = 11;

/// The server did not start to handle the request: a `GOAWAY` that excludes it, a `REFUSED_STREAM`, or a connection that was
/// already closed. It is safe to send the request again on another connection, whatever its method.
#[derive(Debug)]
pub struct Unprocessed(pub String);

impl fmt::Display for Unprocessed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "the server did not handle the request: {}", self.0)
    }
}

impl StdError for Unprocessed {}

/// The status and headers of a reply. Names are lowercase.
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// Why a stream ended with no good end.
enum Reset {
    Peer(u32),
    Unprocessed(&'static str),
    Local(&'static str),
}

impl Reset {
    fn to_error(&self) -> Error {
        match self {
            Reset::Peer(REFUSED_STREAM) => Unprocessed("the stream was refused".into()).into(),
            Reset::Unprocessed(why) => Unprocessed((*why).into()).into(),
            Reset::Peer(code) => io::Error::new(ErrorKind::ConnectionReset, format!("the server reset the stream (code {code})")).into(),
            Reset::Local(why) => io::Error::new(ErrorKind::InvalidData, *why).into(),
        }
    }
}

struct Stream {
    head: Option<Head>,
    got_head: bool,
    data: VecDeque<u8>,
    /// The server sent `END_STREAM`, and we sent ours.
    remote_done: bool,
    local_done: bool,
    /// Counted out of `State::active`.
    closed: bool,
    reset: Option<Reset>,
    /// What the server lets us send for this stream, and what it may still send us.
    send: i64,
    recv: i64,
    /// Bytes that the caller read and that the server does not know yet.
    unacked: u32,
    declared: Option<u64>,
    received: u64,
    /// A reply that has no body whatever its headers say (HEAD, 204, 304).
    no_body: bool,
}

struct State {
    streams: HashMap<u32, Stream>,
    next_id: u32,
    /// Streams that are not closed: the number that the server limits.
    active: usize,
    max_streams: u32,
    conn_send: i64,
    conn_recv: i64,
    conn_unacked: u32,
    /// The server's `SETTINGS_INITIAL_WINDOW_SIZE`, and `SETTINGS_MAX_FRAME_SIZE`.
    init_send: i64,
    max_frame: usize,
    goaway: Option<u32>,
    dead: Option<(ErrorKind, String)>,
}

struct Shared {
    io: Mutex<ClientStream<TcpStream>>,
    /// The same socket: the reader waits on it, and `die` shuts it.
    sock: TcpStream,
    st: Mutex<State>,
    cv: Condvar,
}

/// A connection to one origin. Share it with `Arc`. The last owner closes it.
pub struct Conn {
    sh: Arc<Shared>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn frame(out: &mut Vec<u8>, ty: u8, flags: u8, id: u32, payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&[ty, flags]);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(payload);
}

fn window_update(out: &mut Vec<u8>, id: u32, n: u32) {
    frame(out, WINDOW_UPDATE, 0, id, &n.to_be_bytes());
}

impl Shared {
    fn wait<'a>(&self, st: MutexGuard<'a, State>, until: Instant) -> io::Result<MutexGuard<'a, State>> {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(ErrorKind::TimedOut, "the server stopped answering"));
        }
        Ok(self.cv.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0)
    }

    /// Write whole frames. A failed write ends the connection.
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut io = lock(&self.io);
        let r = io.write_all(bytes).and_then(|_| io.flush());
        drop(io);
        if let Err(e) = &r {
            self.die(e.kind(), "the connection broke while writing");
        }
        r
    }

    /// End the connection: every waiter sees the error, and the reader wakes up and leaves.
    fn die(&self, kind: ErrorKind, why: &str) {
        let mut st = lock(&self.st);
        st.dead.get_or_insert((kind, why.to_string()));
        drop(st);
        let _ = self.sock.shutdown(Shutdown::Both);
        self.cv.notify_all();
    }
}

impl State {
    /// A stream that has both ends done, or a reset, does not count against the limit of the server.
    fn settle(&mut self, id: u32) {
        if let Some(s) = self.streams.get_mut(&id)
            && !s.closed
            && (s.reset.is_some() || (s.remote_done && s.local_done))
        {
            s.closed = true;
            self.active -= 1;
        }
    }

    /// End a stream with an error of ours: tell the server, and keep the reason for the caller.
    fn reset_stream(&mut self, id: u32, code: u32, why: &'static str, out: &mut Vec<u8>) {
        if let Some(s) = self.streams.get_mut(&id)
            && s.reset.is_none()
        {
            s.reset = Some(Reset::Local(why));
            frame(out, RST_STREAM, 0, id, &code.to_be_bytes());
            self.settle(id);
        }
    }

    /// Tell the server about the bytes that the caller read, once they are half of a window.
    fn credit(&mut self, out: &mut Vec<u8>) {
        if self.conn_unacked >= CONN_WINDOW / 2 {
            window_update(out, 0, self.conn_unacked);
            self.conn_recv += self.conn_unacked as i64;
            self.conn_unacked = 0;
        }
    }
}

impl Conn {
    /// Send the preface and our settings on a TLS stream where ALPN chose `h2`, and start the reader.
    pub fn start(tls: ClientStream<TcpStream>) -> io::Result<Arc<Conn>> {
        let sock = tls.get_ref().try_clone()?;
        let mut hello = PREFACE.to_vec();
        let mut settings = Vec::new();
        // ENABLE_PUSH 0, INITIAL_WINDOW_SIZE, MAX_HEADER_LIST_SIZE.
        for (id, value) in [(2u16, 0u32), (4, STREAM_WINDOW), (6, MAX_HEADER_LIST as u32)] {
            settings.extend_from_slice(&id.to_be_bytes());
            settings.extend_from_slice(&value.to_be_bytes());
        }
        frame(&mut hello, SETTINGS, 0, 0, &settings);
        window_update(&mut hello, 0, CONN_WINDOW - 65535);
        let st = State {
            streams: HashMap::new(),
            next_id: 1,
            active: 0,
            max_streams: FIRST_MAX_STREAMS,
            conn_send: 65535,
            conn_recv: CONN_WINDOW as i64,
            conn_unacked: 0,
            init_send: 65535,
            max_frame: MAX_FRAME,
            goaway: None,
            dead: None,
        };
        let sh = Arc::new(Shared { io: Mutex::new(tls), sock, st: Mutex::new(st), cv: Condvar::new() });
        sh.write(&hello)?;
        let reader = sh.clone();
        thread::Builder::new().name("h2".into()).stack_size(256 * 1024).spawn(move || run(&reader))?;
        Ok(Arc::new(Conn { sh }))
    }

    /// False once the connection is closed, or the server said `GOAWAY`, or the stream numbers are used up.
    pub fn usable(&self) -> bool {
        let st = lock(&self.sh.st);
        st.dead.is_none() && st.goaway.is_none() && st.next_id <= MAX_ID
    }

    /// Send a request and wait for the status and the headers. `headers` has the pseudo headers first (`:method`, `:scheme`,
    /// `:authority`, `:path`). The body is read from the returned `Body`. An `Unprocessed` error says that no byte of the request
    /// was handled.
    pub fn request(self: &Arc<Self>, headers: &[(&str, &str)], body: &[u8], head_only: bool, max_body: usize, deadline: Instant) -> Result<(Head, Body), Error> {
        let sh = &self.sh;
        let mut block = Vec::with_capacity(256);
        for (name, value) in headers {
            let name = name.to_ascii_lowercase();
            hpack::encode(&mut block, name.as_bytes(), value.as_bytes(), matches!(name.as_str(), "authorization" | "cookie" | "proxy-authorization"));
        }
        let refusal = |st: &State| -> Option<Error> {
            let why = match (&st.dead, st.goaway) {
                (Some((_, why)), _) => why.clone(),
                (None, Some(_)) => "the server said GOAWAY".into(),
                _ if st.next_id > MAX_ID => "the stream numbers are used up".into(),
                _ => return None,
            };
            Some(Unprocessed(why).into())
        };
        let id = loop {
            {
                let mut st = lock(&sh.st);
                while refusal(&st).is_none() && st.active >= st.max_streams as usize {
                    st = sh.wait(st, deadline)?;
                }
                if let Some(e) = refusal(&st) {
                    return Err(e);
                }
            }
            // The stream number and the first frame go out in the same order: one hold of `io`.
            let mut io = lock(&sh.io);
            let mut st = lock(&sh.st);
            if let Some(e) = refusal(&st) {
                return Err(e);
            }
            if st.active >= st.max_streams as usize {
                continue;
            }
            let id = st.next_id;
            st.next_id += 2;
            st.active += 1;
            let send = st.init_send;
            st.streams.insert(id, Stream {
                head: None,
                got_head: false,
                data: VecDeque::new(),
                remote_done: false,
                local_done: body.is_empty(),
                closed: false,
                reset: None,
                send,
                recv: STREAM_WINDOW as i64,
                unacked: 0,
                declared: None,
                received: 0,
                no_body: head_only,
            });
            let max = st.max_frame;
            drop(st);
            let mut out = Vec::with_capacity(block.len() + 64);
            let mut chunks = block.chunks(max);
            let first = chunks.next().unwrap_or(&[]);
            let end = if block.len() <= max { END_HEADERS } else { 0 } | if body.is_empty() { END_STREAM } else { 0 };
            frame(&mut out, HEADERS, end, id, first);
            let rest: Vec<&[u8]> = chunks.collect();
            for (i, c) in rest.iter().enumerate() {
                frame(&mut out, CONTINUATION, if i + 1 == rest.len() { END_HEADERS } else { 0 }, id, c);
            }
            if let Err(e) = io.write_all(&out).and_then(|_| io.flush()) {
                drop(io);
                sh.die(e.kind(), "the connection broke while writing");
                return Err(e.into());
            }
            break id;
        };
        // From here, dropping the `Body` cleans up the stream, on every way out.
        let mut reply = Body { conn: self.clone(), id, declared: None, given: 0, max: max_body, start: Instant::now(), rec: None };
        let mut sent = 0;
        while sent < body.len() {
            let take = {
                let mut st = lock(&sh.st);
                loop {
                    if let Some((kind, why)) = &st.dead {
                        return Err(io::Error::new(*kind, why.clone()).into());
                    }
                    let s = st.streams.get(&id).expect("an open stream");
                    if s.reset.is_some() || s.remote_done {
                        break 0;
                    }
                    let room = s.send.min(st.conn_send).min(st.max_frame as i64).min((body.len() - sent) as i64);
                    if room > 0 {
                        st.streams.get_mut(&id).unwrap().send -= room;
                        st.conn_send -= room;
                        break room as usize;
                    }
                    st = sh.wait(st, deadline)?;
                }
            };
            if take == 0 {
                break; // the server answered before it read it all: its answer is the result
            }
            sent += take;
            let mut out = Vec::with_capacity(take + 9);
            frame(&mut out, DATA, if sent == body.len() { END_STREAM } else { 0 }, id, &body[sent - take..sent]);
            sh.write(&out)?;
        }
        let head = {
            let mut st = lock(&sh.st);
            if sent == body.len() {
                st.streams.get_mut(&id).expect("an open stream").local_done = true;
                st.settle(id);
            }
            loop {
                let s = st.streams.get_mut(&id).expect("an open stream");
                if let Some(head) = s.head.take() {
                    reply.declared = s.declared.map(|d| d as usize);
                    break head;
                }
                if let Some(reset) = &s.reset {
                    return Err(reset.to_error());
                }
                if let Some((kind, why)) = &st.dead {
                    return Err(io::Error::new(*kind, why.clone()).into());
                }
                st = sh.wait(st, deadline)?;
            }
        };
        Ok((head, reply))
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let sh = &self.sh;
        if let Ok(mut io) = sh.io.try_lock() {
            let mut out = Vec::new();
            frame(&mut out, GOAWAY, 0, 0, &[0, 0, 0, 0, 0, 0, 0, NO_ERROR as u8]);
            let _ = io.write_all(&out).and_then(|_| io.shutdown());
        }
        sh.die(ErrorKind::ConnectionAborted, "the connection was closed here");
    }
}

/// The body of a reply. It reads what the reader thread put in the stream, and tells the server what it read.
pub struct Body {
    conn: Arc<Conn>,
    id: u32,
    /// The `Content-Length` of the reply, when it has one.
    pub declared: Option<usize>,
    given: usize,
    max: usize,
    pub start: Instant,
    /// The timing record of this request (only when `BARE_NET_TRACE` is set). It goes out when the body is dropped.
    pub rec: Option<Box<trace::Rec>>,
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let sh = &self.conn.sh;
        let until = Instant::now() + SILENCE;
        let mut st = lock(&sh.st);
        loop {
            let s = st.streams.get_mut(&self.id).expect("an open stream");
            if let Some(reset) = &s.reset {
                return Err(io::Error::new(reset.kind(), reset.to_error().to_string()));
            }
            if !s.data.is_empty() {
                let n = s.data.read(out)?;
                s.unacked += n as u32;
                let mut update = Vec::new();
                if !s.remote_done && s.unacked >= STREAM_WINDOW / 2 {
                    window_update(&mut update, self.id, s.unacked);
                    s.recv += s.unacked as i64;
                    s.unacked = 0;
                }
                st.conn_unacked += n as u32;
                st.credit(&mut update);
                drop(st);
                if !update.is_empty() {
                    let _ = sh.write(&update);
                }
                self.given += n;
                if self.given > self.max {
                    return Err(io::Error::new(ErrorKind::InvalidData, "the reply is too big"));
                }
                return Ok(n);
            }
            if s.remote_done {
                return Ok(0);
            }
            if let Some((kind, why)) = &st.dead {
                return Err(io::Error::new(*kind, why.clone()));
            }
            st = sh.wait(st, until)?;
        }
    }
}

impl Reset {
    fn kind(&self) -> ErrorKind {
        match self {
            Reset::Peer(_) | Reset::Unprocessed(_) => ErrorKind::ConnectionReset,
            Reset::Local(_) => ErrorKind::InvalidData,
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        if let Some(mut r) = self.rec.take() {
            r.body = self.start.elapsed().as_micros() as u64;
            r.bytes = self.given as u64;
            trace::push(*r);
        }
        let sh = &self.conn.sh;
        let mut out = Vec::new();
        {
            let mut st = lock(&sh.st);
            if let Some(s) = st.streams.remove(&self.id) {
                // What the caller never read still counts against the window of the connection.
                st.conn_unacked += s.data.len() as u32;
                if !s.closed {
                    st.active -= 1;
                    if s.reset.is_none() {
                        frame(&mut out, RST_STREAM, 0, self.id, &CANCEL.to_be_bytes());
                    }
                }
                st.credit(&mut out);
            }
        }
        sh.cv.notify_all();
        if !out.is_empty() {
            let _ = sh.write(&out);
        }
    }
}

// ---- the reader ----

/// Why the reader stops.
enum Stop {
    /// A fault of the peer: say `GOAWAY` with this code, and end.
    Conn(u32, &'static str),
    /// The transport failed or the peer closed.
    Io(io::Error),
    /// Nothing is left to do: the connection was idle, or a `GOAWAY` has no streams left.
    Done,
}

fn conn_error<T>(code: u32, why: &'static str) -> Result<T, Stop> {
    Err(Stop::Conn(code, why))
}

struct Block {
    id: u32,
    end_stream: bool,
    data: Vec<u8>,
}

struct Reader {
    dec: hpack::Decoder,
    buf: Vec<u8>,
    /// A header block that waits for its `CONTINUATION` frames.
    block: Option<Block>,
}

fn run(sh: &Shared) {
    let mut rd = Reader { dec: hpack::Decoder::new(4096), buf: Vec::new(), block: None };
    match rd.pump(sh) {
        Err(Stop::Conn(code, why)) => {
            let mut out = Vec::new();
            let mut payload = [0u8; 8];
            payload[4..].copy_from_slice(&code.to_be_bytes());
            frame(&mut out, GOAWAY, 0, 0, &payload);
            let _ = sh.write(&out);
            sh.die(ErrorKind::InvalidData, why);
        }
        Err(Stop::Io(e)) => sh.die(e.kind(), &format!("the connection ended: {e}")),
        Err(Stop::Done) | Ok(()) => sh.die(ErrorKind::ConnectionAborted, "the connection is closed"),
    }
}

/// Wait until the socket or the TLS stream has bytes, without holding the TLS stream.
fn wait_readable(sh: &Shared) -> Result<(), Stop> {
    loop {
        if lock(&sh.io).has_buffered() {
            return Ok(());
        }
        match sh.sock.peek(&mut [0u8; 1]) {
            Ok(0) => return Err(Stop::Io(ErrorKind::UnexpectedEof.into())),
            Ok(_) => return Ok(()),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // Silence. With no stream open, the connection ends: the mark is set under the lock that `request` takes, so a
                // request either comes first and keeps the connection, or sees it closed and opens another.
                let mut st = lock(&sh.st);
                if st.active == 0 {
                    st.dead.get_or_insert((ErrorKind::ConnectionAborted, "the connection was idle".into()));
                    return Err(Stop::Done);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(Stop::Io(e)),
        }
    }
}

impl Reader {
    fn fill(&mut self, sh: &Shared, need: usize) -> Result<(), Stop> {
        while self.buf.len() < need {
            wait_readable(sh)?;
            let mut piece = [0u8; 16 * 1024];
            let got = lock(&sh.io).read_ready(&mut piece);
            match got {
                Ok(Some(0)) => return Err(Stop::Io(ErrorKind::UnexpectedEof.into())),
                Ok(Some(n)) => self.buf.extend_from_slice(&piece[..n]),
                Ok(None) => {}
                Err(e) => return Err(Stop::Io(e)),
            }
        }
        Ok(())
    }

    fn pump(&mut self, sh: &Shared) -> Result<(), Stop> {
        loop {
            self.fill(sh, 9)?;
            let len = u32::from_be_bytes([0, self.buf[0], self.buf[1], self.buf[2]]) as usize;
            if len > MAX_FRAME {
                return conn_error(FRAME_SIZE_ERROR, "the server sent a frame bigger than we allow");
            }
            self.fill(sh, 9 + len)?;
            let buf = std::mem::take(&mut self.buf);
            let (ty, flags, id) = (buf[3], buf[4], u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff);
            let r = self.on_frame(sh, ty, flags, id, &buf[9..9 + len]);
            self.buf = buf;
            self.buf.drain(..9 + len);
            r?;
            let mut st = lock(&sh.st);
            if st.goaway.is_some() && st.active == 0 {
                st.dead.get_or_insert((ErrorKind::ConnectionAborted, "the server said GOAWAY".into()));
                return Err(Stop::Done);
            }
        }
    }

    fn on_frame(&mut self, sh: &Shared, ty: u8, flags: u8, id: u32, p: &[u8]) -> Result<(), Stop> {
        if let Some(b) = &self.block
            && (ty != CONTINUATION || id != b.id)
        {
            return conn_error(PROTOCOL_ERROR, "a frame inside a header block");
        }
        let mut out = Vec::new();
        let r = match ty {
            DATA => self.data(sh, flags, id, p, &mut out),
            HEADERS => self.headers(sh, flags, id, p, &mut out),
            CONTINUATION => self.continuation(sh, flags, id, p, &mut out),
            PRIORITY => match (id, p.len()) {
                (0, _) => conn_error(PROTOCOL_ERROR, "PRIORITY on stream 0"),
                (_, 5) => Ok(()),
                _ => conn_error(FRAME_SIZE_ERROR, "PRIORITY is not 5 bytes"),
            },
            RST_STREAM => self.rst_stream(sh, id, p),
            SETTINGS => self.settings(sh, flags, id, p, &mut out),
            PUSH_PROMISE => conn_error(PROTOCOL_ERROR, "PUSH_PROMISE, and push is off"),
            PING => match (id, p.len()) {
                (0, 8) => {
                    if flags & ACK == 0 {
                        frame(&mut out, PING, ACK, 0, p);
                    }
                    Ok(())
                }
                (0, _) => conn_error(FRAME_SIZE_ERROR, "PING is not 8 bytes"),
                _ => conn_error(PROTOCOL_ERROR, "PING on a stream"),
            },
            GOAWAY => self.goaway(sh, id, p),
            WINDOW_UPDATE => self.window_update(sh, id, p, &mut out),
            _ => Ok(()), // an unknown type is ignored (section 4.1)
        };
        r?;
        if !out.is_empty() {
            sh.write(&out).map_err(Stop::Io)?;
        }
        Ok(())
    }

    /// Strip the padding of a `DATA` or `HEADERS` payload.
    fn unpad(flags: u8, p: &[u8]) -> Result<&[u8], Stop> {
        if flags & PADDED == 0 {
            return Ok(p);
        }
        let pad = *p.first().ok_or(Stop::Conn(FRAME_SIZE_ERROR, "a padded frame with no length"))? as usize;
        if pad >= p.len() {
            return conn_error(PROTOCOL_ERROR, "padding as long as the frame");
        }
        Ok(&p[1..p.len() - pad])
    }

    fn data(&mut self, sh: &Shared, flags: u8, id: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), Stop> {
        if id == 0 {
            return conn_error(PROTOCOL_ERROR, "DATA on stream 0");
        }
        let data = Self::unpad(flags, p)?;
        let mut st = lock(&sh.st);
        st.conn_recv -= p.len() as i64;
        if st.conn_recv < 0 {
            return conn_error(FLOW_CONTROL_ERROR, "the server sent more than the connection window");
        }
        let overhead = (p.len() - data.len()) as u32;
        if id >= st.next_id {
            return conn_error(PROTOCOL_ERROR, "DATA on a stream we did not open");
        }
        let mut problem = None;
        let mut kept = false;
        if let Some(s) = st.streams.get_mut(&id)
            && s.reset.is_none()
        {
            s.recv -= p.len() as i64;
            s.received += data.len() as u64;
            let end = flags & END_STREAM != 0;
            problem = if s.remote_done {
                Some((STREAM_CLOSED, "DATA after the end of the stream"))
            } else if !s.got_head {
                Some((PROTOCOL_ERROR, "DATA before the headers"))
            } else if s.recv < 0 {
                Some((FLOW_CONTROL_ERROR, "the server sent more than the stream window"))
            } else if s.declared.is_some_and(|d| s.received > d || (end && !s.no_body && s.received != d)) {
                Some((PROTOCOL_ERROR, "the body is not as long as Content-Length says"))
            } else {
                s.data.extend(data);
                s.unacked += overhead;
                s.remote_done = end;
                kept = true;
                None
            };
        }
        // What we keep is credited when the caller reads it. The rest (padding, a stream that is gone or failed) is credited now.
        st.conn_unacked += if kept { overhead } else { p.len() as u32 };
        if let Some((code, why)) = problem {
            st.reset_stream(id, code, why, out);
        }
        st.settle(id);
        st.credit(out);
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }

    fn headers(&mut self, sh: &Shared, flags: u8, id: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), Stop> {
        if id == 0 || id.is_multiple_of(2) {
            return conn_error(PROTOCOL_ERROR, "HEADERS on a stream that the server may not open");
        }
        let mut payload = Self::unpad(flags, p)?;
        if flags & PRIORITY_FLAG != 0 {
            payload = payload.get(5..).ok_or(Stop::Conn(FRAME_SIZE_ERROR, "HEADERS too short for its priority"))?;
        }
        if payload.len() > MAX_BLOCK {
            return conn_error(ENHANCE_YOUR_CALM, "a header block that is too big");
        }
        self.block = Some(Block { id, end_stream: flags & END_STREAM != 0, data: payload.to_vec() });
        if flags & END_HEADERS != 0 {
            self.finish_block(sh, out)?;
        }
        Ok(())
    }

    fn continuation(&mut self, sh: &Shared, flags: u8, id: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), Stop> {
        let Some(b) = &mut self.block else {
            return conn_error(PROTOCOL_ERROR, "CONTINUATION with no header block");
        };
        if id != b.id {
            return conn_error(PROTOCOL_ERROR, "CONTINUATION on another stream");
        }
        if b.data.len() + p.len() > MAX_BLOCK {
            return conn_error(ENHANCE_YOUR_CALM, "a header block that is too big");
        }
        b.data.extend_from_slice(p);
        if flags & END_HEADERS != 0 {
            self.finish_block(sh, out)?;
        }
        Ok(())
    }

    /// Decode a whole header block (always: the table must stay in step with the server), and hand it to its stream.
    fn finish_block(&mut self, sh: &Shared, out: &mut Vec<u8>) -> Result<(), Stop> {
        let Block { id, end_stream, data } = self.block.take().expect("a header block");
        let mut fields = Vec::new();
        let mut bad = None;
        let (mut size, mut status, mut regular) = (0, None, false);
        self.dec
            .decode(&data, |name, value| {
                size += name.len() + value.len() + 32;
                if bad.is_some() {
                    return;
                }
                if size > MAX_HEADER_LIST {
                    bad = Some("the headers of the reply are too long");
                } else if name.is_empty() || name.iter().any(u8::is_ascii_uppercase) || value.iter().any(|&b| matches!(b, 0 | b'\r' | b'\n')) {
                    bad = Some("a header that is not allowed");
                } else if name[0] == b':' {
                    if regular || name != b":status" || status.is_some() {
                        bad = Some("a pseudo header that is not allowed");
                    } else {
                        status = std::str::from_utf8(value).ok().filter(|v| v.len() == 3).and_then(|v| v.parse::<u16>().ok());
                        bad = status.is_none().then_some("a bad :status");
                    }
                } else if matches!(name, b"connection" | b"keep-alive" | b"proxy-connection" | b"transfer-encoding" | b"upgrade") {
                    bad = Some("a header that belongs to HTTP/1");
                } else {
                    regular = true;
                    fields.push((String::from_utf8_lossy(name).into_owned(), String::from_utf8_lossy(value).into_owned()));
                }
            })
            .map_err(|e| Stop::Conn(COMPRESSION_ERROR, e.0))?;
        let mut st = lock(&sh.st);
        if id >= st.next_id {
            return conn_error(PROTOCOL_ERROR, "HEADERS on a stream we did not open");
        }
        if let Some(s) = st.streams.get_mut(&id)
            && s.reset.is_none()
        {
            let problem = if let Some(why) = bad {
                Some(why)
            } else if s.remote_done {
                Some("HEADERS after the end of the stream")
            } else if !s.got_head {
                match status {
                    None => Some("a reply with no :status"),
                    // An interim reply (100 Continue) carries no body and is not the answer.
                    Some(100..=199) => end_stream.then_some("an interim reply that ends the stream"),
                    Some(code) => {
                        s.got_head = true;
                        s.no_body |= code == 204 || code == 304;
                        let length = fields.iter().find(|(n, _)| n == "content-length").map(|(_, v)| v.as_str());
                        match length.map(|v| v.parse::<u64>().ok().filter(|_| v.bytes().all(|b| b.is_ascii_digit()))) {
                            Some(None) => Some("a bad Content-Length"),
                            other => {
                                s.declared = other.flatten();
                                s.head = Some(Head { status: code, headers: std::mem::take(&mut fields) });
                                None
                            }
                        }
                    }
                }
            } else if !end_stream {
                Some("trailers that do not end the stream")
            } else {
                None
            };
            if problem.is_none() && end_stream {
                s.remote_done = true;
                if !s.no_body && s.declared.is_some_and(|d| d != s.received) {
                    st.reset_stream(id, PROTOCOL_ERROR, "the body is shorter than Content-Length says", out);
                }
            } else if let Some(why) = problem {
                let code = if why.contains("too long") { CANCEL } else { PROTOCOL_ERROR };
                st.reset_stream(id, code, why, out);
            }
            st.settle(id);
        }
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }

    fn rst_stream(&mut self, sh: &Shared, id: u32, p: &[u8]) -> Result<(), Stop> {
        if id == 0 {
            return conn_error(PROTOCOL_ERROR, "RST_STREAM on stream 0");
        }
        let code = u32::from_be_bytes(p.try_into().map_err(|_| Stop::Conn(FRAME_SIZE_ERROR, "RST_STREAM is not 4 bytes"))?);
        let mut st = lock(&sh.st);
        if id >= st.next_id {
            return conn_error(PROTOCOL_ERROR, "RST_STREAM on a stream we did not open");
        }
        if let Some(s) = st.streams.get_mut(&id)
            && !s.remote_done
            && s.reset.is_none()
        {
            s.reset = Some(Reset::Peer(code));
            st.settle(id);
        }
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }

    fn settings(&mut self, sh: &Shared, flags: u8, id: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), Stop> {
        if id != 0 {
            return conn_error(PROTOCOL_ERROR, "SETTINGS on a stream");
        }
        if flags & ACK != 0 {
            return if p.is_empty() { Ok(()) } else { conn_error(FRAME_SIZE_ERROR, "SETTINGS ACK with a payload") };
        }
        if !p.len().is_multiple_of(6) {
            return conn_error(FRAME_SIZE_ERROR, "SETTINGS is not a multiple of 6 bytes");
        }
        let mut st = lock(&sh.st);
        for pair in p.chunks(6) {
            let value = u32::from_be_bytes([pair[2], pair[3], pair[4], pair[5]]);
            match u16::from_be_bytes([pair[0], pair[1]]) {
                2 if value > 1 => return conn_error(PROTOCOL_ERROR, "ENABLE_PUSH is not 0 or 1"),
                3 => st.max_streams = value,
                4 => {
                    if value > MAX_ID {
                        return conn_error(FLOW_CONTROL_ERROR, "INITIAL_WINDOW_SIZE above 2^31-1");
                    }
                    // A new initial window moves the window of every open stream by the difference (section 6.9.2).
                    let delta = value as i64 - st.init_send;
                    st.init_send = value as i64;
                    for s in st.streams.values_mut() {
                        s.send += delta;
                        if s.send > MAX_ID as i64 {
                            return conn_error(FLOW_CONTROL_ERROR, "a stream window above 2^31-1");
                        }
                    }
                }
                5 => {
                    if !(16384..=16_777_215).contains(&value) {
                        return conn_error(PROTOCOL_ERROR, "MAX_FRAME_SIZE out of range");
                    }
                    st.max_frame = value as usize;
                }
                _ => {} // HEADER_TABLE_SIZE needs nothing: we never add to the table of the peer. The rest is ignored.
            }
        }
        frame(out, SETTINGS, ACK, 0, &[]);
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }

    fn goaway(&mut self, sh: &Shared, id: u32, p: &[u8]) -> Result<(), Stop> {
        if id != 0 {
            return conn_error(PROTOCOL_ERROR, "GOAWAY on a stream");
        }
        if p.len() < 8 {
            return conn_error(FRAME_SIZE_ERROR, "GOAWAY is shorter than 8 bytes");
        }
        let last = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff;
        let mut st = lock(&sh.st);
        let last = st.goaway.map_or(last, |old| old.min(last));
        st.goaway = Some(last);
        // The server did not look at the streams past `last`. They can run again somewhere else.
        let ids: Vec<u32> = st.streams.iter().filter(|(i, s)| **i > last && s.reset.is_none()).map(|(i, _)| *i).collect();
        for i in ids {
            let s = st.streams.get_mut(&i).unwrap();
            if !s.remote_done {
                s.reset = Some(Reset::Unprocessed("the server said GOAWAY"));
                st.settle(i);
            }
        }
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }

    fn window_update(&mut self, sh: &Shared, id: u32, p: &[u8], out: &mut Vec<u8>) -> Result<(), Stop> {
        let add = u32::from_be_bytes(p.try_into().map_err(|_| Stop::Conn(FRAME_SIZE_ERROR, "WINDOW_UPDATE is not 4 bytes"))?) & 0x7fff_ffff;
        let mut st = lock(&sh.st);
        if id != 0 && id >= st.next_id {
            return conn_error(PROTOCOL_ERROR, "WINDOW_UPDATE on a stream we did not open");
        }
        if add == 0 {
            if id == 0 {
                return conn_error(PROTOCOL_ERROR, "a WINDOW_UPDATE of 0");
            }
            st.reset_stream(id, PROTOCOL_ERROR, "a WINDOW_UPDATE of 0", out);
        } else if id == 0 {
            st.conn_send += add as i64;
            if st.conn_send > MAX_ID as i64 {
                return conn_error(FLOW_CONTROL_ERROR, "the connection window is above 2^31-1");
            }
        } else if let Some(s) = st.streams.get_mut(&id) {
            s.send += add as i64;
            if s.send > MAX_ID as i64 {
                st.reset_stream(id, FLOW_CONTROL_ERROR, "a stream window above 2^31-1", out);
            }
        }
        drop(st);
        sh.cv.notify_all();
        Ok(())
    }
}

//! Record and replay of replies, so that a benchmark does not depend on live sites.
//!
//! - `BARE_NET_RECORD=<dir>`: every reply that a client gets is also written to `<dir>`. Use an empty directory. The client works as usual.
//! - `BARE_NET_REPLAY=<dir>`: no request leaves the process (no DNS lookup, no socket). Each request is answered from `<dir>`. A request
//!   with no recorded reply fails with the error `replay: no recorded reply for <label>`, and the same text goes to stderr.
//! - `BARE_NET_FILL=<dir>`: as replay, but a request with no recorded reply goes to the network and its reply is recorded in `<dir>`. A page
//!   that runs fast on replay asks for files that the live recording never asked for (the recording ended first): fill until a run has no
//!   such line on stderr, and the recording is closed for that journey.
//!   Precedence: `BARE_NET_FILL`, then `BARE_NET_REPLAY`, then `BARE_NET_RECORD`.
//!
//! What is stored. One file for each request, named `<hash of the label>.<n>`. The label is `METHOD scheme://host:port/path?query`, then
//! the FNV-1a hash of the request body when the body is not empty. `n` counts the requests with the same label, from 0: a page that polls
//! one URL gets its replies in the order that it got them, and the last reply again after the last one. A reply is stored as the client
//! got it from the wire: the status, the headers, and the body BEFORE `Content-Encoding` is undone and after chunking is undone. A replay
//! decodes the body again, so the cost of gzip and brotli stays in the measurement. A request that failed (DNS, connect, TLS, a bad reply)
//! is stored with its error text and fails in the same way when it is replayed.
//!
//! A reply whose body did not end (the page went away while an `EventSource` or a long poll was open, or the read failed) is stored with the
//! bytes that came. A replay gives these bytes, then a `TimedOut` read error: what a live idle stream gives after its read limit.
//!
//! Not stored: the request headers (a request that differs only in a header such as `Accept` or `Range` gets the same reply), and the
//! time that the server took. The page has no WebSocket in this browser, so there is nothing to replay for it.
//! The cookie jar and the cache run as in a live run: `Set-Cookie` headers are in the stored head and the jar sees them.
//!
//! The process reads the two variables once (`Tape::from_env`). A test makes its own tape with `Tape::record` or `Tape::replay`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// A directory of stored replies, and how far each label has got in this process.
pub struct Tape {
    dir: PathBuf,
    /// Answers from the directory when it can.
    reads: bool,
    /// Goes to the network, and records the reply, when the directory cannot answer.
    writes: bool,
    seen: Mutex<HashMap<String, usize>>,
}

/// One stored reply, or one stored failure (`error` is set).
#[derive(Debug, Default, PartialEq)]
pub struct Entry {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub close: bool,
    /// The body ended well. False: the stored body is what came before a cut or an error.
    pub complete: bool,
    pub error: Option<String>,
    pub body: Vec<u8>,
}

/// Where the next reply with one label goes (record), or comes from (replay).
pub struct Slot {
    path: PathBuf,
    label: String,
}

const MAGIC: &[u8; 4] = b"BNT1";

impl Tape {
    /// The tape that the environment asks for, shared by every client of the process. `None` when neither variable is set.
    pub fn from_env() -> Option<Arc<Tape>> {
        static TAPE: OnceLock<Option<Arc<Tape>>> = OnceLock::new();
        TAPE.get_or_init(|| {
            let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
            match (var("BARE_NET_FILL"), var("BARE_NET_REPLAY"), var("BARE_NET_RECORD")) {
                (Some(dir), ..) => Some(Tape::fill(dir)),
                (None, Some(dir), _) => Some(Tape::replay(dir)),
                (None, None, Some(dir)) => Some(Tape::record(dir)),
                _ => None,
            }
        })
        .clone()
    }

    pub fn record(dir: impl Into<PathBuf>) -> Arc<Tape> {
        Arc::new(Tape { dir: dir.into(), reads: false, writes: true, seen: Mutex::default() })
    }

    pub fn replay(dir: impl Into<PathBuf>) -> Arc<Tape> {
        Arc::new(Tape { dir: dir.into(), reads: true, writes: false, seen: Mutex::default() })
    }

    pub fn fill(dir: impl Into<PathBuf>) -> Arc<Tape> {
        Arc::new(Tape { dir: dir.into(), reads: true, writes: true, seen: Mutex::default() })
    }

    /// Look in the directory before a request.
    pub fn reads(&self) -> bool {
        self.reads
    }

    /// Record the replies of requests that go to the network.
    pub fn writes(&self) -> bool {
        self.writes
    }

    /// The label of a request. `url` is `scheme://host:port/path?query`.
    pub fn label(method: &str, url: &str, body: &[u8]) -> String {
        if body.is_empty() { format!("{method} {url}") } else { format!("{method} {url} {:016x}", fnv(body)) }
    }

    /// The slot for the next request with this label (it goes to the network, and `save` records the reply there). Call it once for each such request.
    pub fn slot(&self, label: String) -> Slot {
        let n = self.bump(&label);
        Slot { path: self.path(&label, n), label }
    }

    fn bump(&self, label: &str) -> usize {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let c = seen.entry(label.to_string()).or_insert(0);
        *c += 1;
        *c - 1
    }

    fn path(&self, label: &str, n: usize) -> PathBuf {
        self.dir.join(format!("{:016x}.{n}", fnv(label.as_bytes())))
    }

    /// The reply for the next request with this label: the one that was recorded `n`-th, or the last one. `None`: nothing is recorded for the
    /// label (the miss is logged, and does not count as a request of the label: a `slot` for it records the reply at the same place).
    pub fn find(&self, label: &str) -> Option<Entry> {
        let n = self.seen.lock().unwrap_or_else(|e| e.into_inner()).get(label).copied().unwrap_or(0);
        let found = (0..=n).rev().find_map(|i| read(&self.path(label, i)).filter(|(l, _)| l == label).map(|(_, e)| e));
        match found {
            Some(_) => {
                self.bump(label);
            }
            None => eprintln!("bare-net: replay: no recorded reply for {label}{}", if self.writes { " (fetching it)" } else { "" }),
        }
        found
    }
}

impl Slot {
    /// Write the entry. A failed write is reported and the run goes on: the recording is then incomplete, and a replay says which request.
    pub fn save(&self, entry: &Entry) {
        let mut out = Vec::with_capacity(entry.body.len() + 512);
        out.extend_from_slice(MAGIC);
        put(&mut out, self.label.as_bytes());
        out.extend_from_slice(&entry.status.to_le_bytes());
        out.push(entry.close as u8 | (entry.complete as u8) << 1 | (entry.error.is_some() as u8) << 2);
        match &entry.error {
            Some(text) => put(&mut out, text.as_bytes()),
            None => {
                out.extend_from_slice(&(entry.headers.len() as u32).to_le_bytes());
                for (n, v) in &entry.headers {
                    put(&mut out, n.as_bytes());
                    put(&mut out, v.as_bytes());
                }
                out.extend_from_slice(&entry.body);
            }
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let done = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&tmp, &out)?;
            fs::rename(&tmp, &self.path)
        };
        if let Err(e) = done() {
            eprintln!("bare-net: cannot record {}: {e}", self.label);
        }
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// A reader over the bytes of a file. Every `take` gives `None` on a short file.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u32(&mut self) -> Option<usize> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize)
    }
    fn text(&mut self) -> Option<String> {
        let n = self.u32()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// The label and the entry of a file, or `None` when the file is missing or is not a stored reply.
fn read(path: &Path) -> Option<(String, Entry)> {
    let data = fs::read(path).ok()?;
    let mut c = Cursor(&data);
    if c.take(4)? != MAGIC {
        return None;
    }
    let label = c.text()?;
    let status = u16::from_le_bytes(c.take(2)?.try_into().ok()?);
    let flags = c.take(1)?[0];
    let mut entry = Entry { status, close: flags & 1 != 0, complete: flags & 2 != 0, ..Entry::default() };
    if flags & 4 != 0 {
        entry.error = Some(c.text()?);
    } else {
        for _ in 0..c.u32()? {
            entry.headers.push((c.text()?, c.text()?));
        }
        entry.body = c.0.to_vec();
    }
    Some((label, entry))
}

/// FNV-1a, 64 bit. It never changes between Rust versions, so a recording stays valid for any build.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bare-replay-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn reply(body: &str) -> Entry {
        Entry { status: 200, headers: vec![("content-type".into(), "text/plain".into())], close: false, complete: true, error: None, body: body.into() }
    }

    #[test]
    fn a_stored_reply_comes_back_in_order_and_the_last_one_repeats() {
        let d = dir("order");
        let rec = Tape::record(&d);
        let label = || Tape::label("GET", "http://a:80/x", b"");
        rec.slot(label()).save(&reply("one"));
        rec.slot(label()).save(&reply("two"));
        let play = Tape::replay(&d);
        let bodies: Vec<_> = (0..3).map(|_| String::from_utf8(play.find(&label()).unwrap().body).unwrap()).collect();
        assert_eq!(bodies, ["one", "two", "two"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_body_of_a_request_is_part_of_the_label() {
        let d = dir("body");
        let rec = Tape::record(&d);
        rec.slot(Tape::label("POST", "http://a:80/q", b"a=1")).save(&reply("first"));
        let play = Tape::replay(&d);
        assert!(play.find(&Tape::label("POST", "http://a:80/q", b"a=2")).is_none());
        assert_eq!(play.find(&Tape::label("POST", "http://a:80/q", b"a=1")).unwrap().body, b"first");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_fill_that_missed_records_at_the_place_of_the_miss() {
        let d = dir("fill");
        Tape::record(&d).slot("GET http://a:80/x".into()).save(&reply("one"));
        let fill = Tape::fill(&d);
        assert_eq!(fill.find("GET http://a:80/x").unwrap().body, b"one");
        // The second request of the label is a miss: the network answers, and the slot holds the next number.
        assert!(fill.find("GET http://a:80/x").is_some(), "after the last reply, the last one repeats");
        assert!(fill.find("GET http://a:80/y").is_none());
        fill.slot("GET http://a:80/y".into()).save(&reply("two"));
        assert_eq!(Tape::replay(&d).find("GET http://a:80/y").unwrap().body, b"two");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_and_a_cut_body_survive_the_file() {
        let d = dir("kinds");
        let rec = Tape::record(&d);
        rec.slot("GET http://a:80/f".into()).save(&Entry { error: Some("connection refused".into()), ..Entry::default() });
        rec.slot("GET http://a:80/c".into()).save(&Entry { complete: false, ..reply("half") });
        let play = Tape::replay(&d);
        assert_eq!(play.find("GET http://a:80/f").unwrap().error.as_deref(), Some("connection refused"));
        let cut = play.find("GET http://a:80/c").unwrap();
        assert!(!cut.complete && cut.body == b"half");
        let _ = fs::remove_dir_all(&d);
    }
}

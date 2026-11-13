//! The engine of one download: resume, retry and parallel byte ranges (`docs/design-downloads.md`, section 5).
//!
//! How it works
//! - The first reply streams into segment 0, as before. When the server can serve ranges (`Accept-Ranges: bytes`, a known size, a
//!   validator, no `Content-Encoding`, a GET), the job may split. A free connection takes the segment with the most bytes left and cuts
//!   it in the middle: the old connection stops at the cut, a new one asks for the second half with `Range` and `If-Range`. This is
//!   work stealing: there is no HEAD request, and a small file costs one request, as before.
//! - A slow segment is taken over the same way: when the others end, their free connections cut the largest part left, which is the
//!   slow one. A dead connection fails its read after 30 s (`tcp.rs`) and retries.
//! - Each segment thread retries on its own: a failed read asks again from its last byte, after 1, 2, 4, 8 and 16 s (or `Retry-After`).
//!   A segment that made progress starts its count again.
//! - A `200` to a range request means that the file changed (or the server forgot ranges): the job starts again from byte 0, once.
//!   A `401`, `403` or `410` to a range request (a URL signed for one use): the job stops splitting, and the range goes back to the
//!   segment before it when that one still runs.
//! - A job that cannot resume (no validator, no size, a coding, a POST) has one segment. A GET of it starts again from 0 once.
//! - Each segment writes at its own offset of the `.part` file. The file is renamed when the last byte is in.
//! - Each range uses its own client with its own connections: an HTTP/2 server would put all streams on one TCP connection, and one TCP
//!   connection is what a split must avoid. At most `MAX_CONNS` for a job and `MAX_ALL` for the process.
//!
//! Locks: one mutex for each job (the segment list). A segment takes it once for each 64 KiB piece, and writes the piece under it.

use super::{DownloadState, Downloads, MAX_DOWNLOAD, PIECE};
use bare_net::http::{Body, Client, Cookies, Ctx, Request, Response};
use bare_url::Url;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// A segment is cut only when both halves are at least this big.
pub(crate) const MIN_SPLIT: u64 = 4 << 20;
/// Connections for one job.
pub(crate) const MAX_CONNS: usize = 8;
/// Connections for all jobs of the process.
const MAX_ALL: usize = 16;
/// Jobs that run at once for one `Downloads`; more wait as `Queued`.
pub(crate) const MAX_JOBS: usize = 3;
/// Tries of a segment without progress.
const TRIES: u32 = 5;
const TICK: Duration = Duration::from_millis(250);

static CONNS: AtomicUsize = AtomicUsize::new(0);

/// What the first reply gave: the job goes on from here.
pub(crate) struct Start {
    /// A fork of the client of the page, with `allow_big`.
    pub client: Client,
    /// The URL after redirects.
    pub url: Url,
    /// The site that started the navigation, for the `SameSite` rules of the jar (see `Ctx::Navigation`).
    pub from: Option<String>,
    pub get: bool,
    pub response: Response,
    /// The body of the first reply, with `unlimited`.
    pub body: Body,
}

/// A byte range `[start, end)` of the file. `pos` is the next byte to write. `end` may move down (a split) or up (a range given back).
#[derive(Clone, Debug)]
struct Seg {
    pos: u64,
    end: u64,
    /// A thread reads it now.
    running: bool,
}

struct State {
    segs: Vec<Seg>,
    /// The bytes that are written.
    done: u64,
    /// Ranges are refused or not possible: no new split.
    no_split: bool,
    /// The file changed on the server: the job starts again with this reply (once).
    changed: Option<(Response, Body)>,
    restarted: bool,
    /// Raised on a restart: the threads of the old generation stop.
    generation: u32,
    failed: Option<String>,
    /// Threads that run.
    threads: usize,
    /// `pause`: the threads stop (a new generation) and no new one starts until `resume`.
    paused: bool,
    cancelled: bool,
    /// Bytes per second for the whole job, and when the next piece may go (a token bucket of one piece).
    limit: Option<u64>,
    next_free: std::time::Instant,
}

/// A job that waits for a place.
pub(crate) struct Waiting {
    id: usize,
    name: String,
    mime: String,
    start: Start,
}

pub(crate) struct Job {
    url: Url,
    from: Option<String>,
    get: bool,
    /// Copied to each segment client: they share the cookies, the identity and the root certificates.
    client: Client,
    file: File,
    total: Option<u64>,
    validator: Mutex<Option<String>>,
    resumable: bool,
    st: Mutex<State>,
    cv: Condvar,
}

/// What the first reply says about ranges: `(total, validator, resumable)`.
fn plan(r: &Response, get: bool) -> (Option<u64>, Option<String>, bool) {
    let total = r.header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
    let validator = r.header("etag").filter(|e| !e.trim_start().starts_with("W/")).or_else(|| r.header("last-modified")).map(|v| v.trim().to_string());
    let ranges = r.header("accept-ranges").is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("bytes")));
    let plain = r.header("content-encoding").is_none_or(|c| c.trim().is_empty() || c.trim().eq_ignore_ascii_case("identity"));
    (total, validator.clone(), get && ranges && plain && total.is_some() && validator.is_some())
}

/// `Content-Range: bytes a-b/total` gives `(a, total)`.
fn content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (a, _) = range.split_once('-')?;
    Some((a.trim().parse().ok()?, total.trim().parse().ok()))
}

fn write_at(f: &File, buf: &[u8], at: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(f, buf, at)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < buf.len() {
            done += std::os::windows::fs::FileExt::seek_write(f, &buf[done..], at + done as u64)?;
        }
        Ok(())
    }
}

/// Why a request for a range did not give a body.
enum Miss {
    /// Try again after this wait.
    Retry(String, Option<Duration>),
    /// Ranges do not work for this URL now.
    Refused(String),
    /// A `200`: the whole file, maybe a new one.
    Whole(Response, Body),
    Fatal(String),
}

impl Job {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A client of its own (its own connections), with the cookies and identity of the page.
    fn own_client(&self) -> Client {
        let mut c = self.client.fork_alone();
        c.allow_big();
        c
    }

    /// Ask for `[pos, end)`, or for the whole file when `pos` is 0 and the job cannot resume.
    fn request(&self, client: &mut Client, pos: u64, end: u64) -> Result<Body, Miss> {
        let range = format!("bytes={pos}-{}", end - 1);
        let validator = self.validator.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut headers = vec![("Accept", "*/*")];
        if self.resumable {
            headers.extend([("Range", range.as_str()), ("Accept-Encoding", "identity")]);
            if let Some(v) = &validator {
                headers.push(("If-Range", v.as_str()));
            }
        }
        let cookies = Cookies { on: true, ctx: Ctx::Navigation(self.from.as_deref()) };
        let req = Request { cookies, headers: &headers, ..Request::get(&self.url) };
        let s = client.send_stream_once(&req).map_err(|e| Miss::Retry(format!("network error: {e}"), None))?;
        let r = Arc::try_unwrap(s.response).unwrap_or_else(|a| (*a).clone());
        let mut body = s.body;
        body.unlimited();
        match r.status {
            206 if self.resumable => {
                let ok = r.header("content-range").and_then(content_range).is_some_and(|(a, t)| a == pos && t.is_none_or(|t| Some(t) == self.total));
                if ok { Ok(body) } else { Err(Miss::Fatal("the server sent another range".into())) }
            }
            200 => Err(Miss::Whole(r, body)),
            401 | 403 | 410 if self.resumable => Err(Miss::Refused(format!("status {}", r.status))),
            408 | 429 | 500 | 502 | 503 | 504 => {
                let wait = r.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(|s| Duration::from_secs(s.min(60)));
                Err(Miss::Retry(format!("status {}", r.status), wait))
            }
            s => Err(Miss::Fatal(format!("status {s}"))),
        }
    }

    /// Read `body` into segment `i` until its end, the end of the body, an error, or a new generation. Gives the bytes written.
    fn pump(&self, i: usize, generation: u32, body: &mut Body, piece: &mut [u8]) -> io::Result<u64> {
        let mut wrote = 0;
        loop {
            let n = body.read(piece)?;
            let mut st = self.lock();
            if st.generation != generation || st.failed.is_some() {
                return Ok(wrote);
            }
            let s = &mut st.segs[i];
            if n == 0 {
                return Ok(wrote);
            }
            let take = (n as u64).min(s.end.saturating_sub(s.pos));
            let at = s.pos;
            if at + take > MAX_DOWNLOAD {
                drop(st);
                return Err(io::Error::other("over 1 GiB"));
            }
            // The write happens under the lock, so a thread of an old generation (a restart) never writes after the new one began.
            // A 64 KiB write to the page cache takes microseconds.
            write_at(&self.file, &piece[..take as usize], at)?;
            let s = &mut st.segs[i];
            s.pos += take;
            st.done += take;
            let at_end = st.segs[i].pos >= st.segs[i].end;
            let wait = st.limit.map(|l| {
                let now = std::time::Instant::now();
                let from = st.next_free.max(now);
                st.next_free = from + Duration::from_secs_f64(take as f64 / l.max(1) as f64);
                from - now
            });
            drop(st);
            if let Some(w) = wait.filter(|w| !w.is_zero()) {
                std::thread::sleep(w);
            }
            wrote += take;
            if at_end {
                return Ok(wrote);
            }
        }
    }

    /// `(pos, end)` of segment `i`, or `None` when its generation is over or the job failed.
    fn seg(&self, i: usize, generation: u32) -> Option<(u64, u64)> {
        let st = self.lock();
        (st.generation == generation && st.failed.is_none()).then(|| (st.segs[i].pos, st.segs[i].end))
    }

    fn fail(&self, why: String) {
        let mut st = self.lock();
        st.failed.get_or_insert(why);
        self.cv.notify_all();
    }

    /// Give back the range of segment `i` to the segment that ends at its start, when that one still runs. Else the job fails.
    fn give_back(&self, i: usize, generation: u32, why: String) {
        let mut st = self.lock();
        if st.generation != generation {
            return;
        }
        st.no_split = true;
        let (pos, end) = (st.segs[i].pos, st.segs[i].end);
        match st.segs.iter().position(|s| s.end == pos && s.running) {
            Some(j) => {
                st.segs[j].end = end;
                st.segs[i].end = pos;
            }
            None => {
                st.failed.get_or_insert(format!("the server refused a range request: {why}"));
            }
        }
        self.cv.notify_all();
    }

    /// The thread of segment `i`. `body` is the reply that it starts with, if any.
    fn segment(self: Arc<Self>, i: usize, generation: u32, mut body: Option<Body>) {
        let mut client = self.own_client();
        let mut piece = vec![0u8; PIECE];
        let mut tries = 0;
        loop {
            let Some((pos, end)) = self.seg(i, generation) else { break };
            if pos >= end {
                break;
            }
            let mut b = match body.take() {
                Some(b) => b,
                None => match self.request(&mut client, pos, end) {
                    Ok(b) => b,
                    Err(Miss::Whole(r, b)) => {
                        let mut st = self.lock();
                        if st.generation != generation {
                            break;
                        }
                        // Not resumable: the plain GET starts again from 0 (`retry`). Resumable: the file changed; the coordinator restarts.
                        if !self.resumable && pos == 0 {
                            drop(st);
                            body = Some(b);
                            continue;
                        }
                        if st.restarted {
                            st.failed.get_or_insert("the file changed on the server again".into());
                        } else {
                            st.changed = Some((r, b));
                        }
                        self.cv.notify_all();
                        break;
                    }
                    Err(Miss::Refused(why)) => {
                        self.give_back(i, generation, why);
                        break;
                    }
                    Err(Miss::Fatal(why)) => {
                        self.fail(why);
                        break;
                    }
                    Err(Miss::Retry(why, wait)) => {
                        if !self.wait_retry(&mut tries, wait) {
                            self.fail(why);
                            break;
                        }
                        continue;
                    }
                },
            };
            let r = self.pump(i, generation, &mut b, &mut piece);
            let Some((pos2, end2)) = self.seg(i, generation) else { break };
            if pos2 > pos {
                tries = 0;
            }
            match r {
                Ok(_) if pos2 >= end2 => break,
                // The reply ended before the range did. With no known size, the end of the body is the end of the file.
                Ok(_) if self.total.is_none() => {
                    let mut st = self.lock();
                    if st.generation == generation {
                        st.segs[i].end = pos2;
                    }
                    break;
                }
                Ok(_) => {
                    let can = self.resumable || pos2 == 0 || self.restart_plain(i, generation);
                    if !can || !self.wait_retry(&mut tries, None) {
                        self.fail("the transfer ended early".into());
                        break;
                    }
                }
                Err(e) if e.to_string() == "over 1 GiB" => {
                    self.fail("over 1 GiB".into());
                    break;
                }
                Err(e) => {
                    let can = self.resumable || pos2 == 0 || self.restart_plain(i, generation);
                    if !can || !self.wait_retry(&mut tries, None) {
                        self.fail(format!("the transfer failed: {}", e.kind()));
                        break;
                    }
                }
            }
        }
        CONNS.fetch_sub(1, Ordering::Relaxed);
        let mut st = self.lock();
        if st.generation == generation {
            st.segs[i].running = false;
        }
        st.threads -= 1;
        self.cv.notify_all();
    }

    /// A job that cannot resume lost its connection: start again from byte 0, once.
    fn restart_plain(&self, i: usize, generation: u32) -> bool {
        let mut st = self.lock();
        if !self.get || st.restarted || st.generation != generation {
            return false;
        }
        st.restarted = true;
        st.done = 0;
        st.segs[i].pos = 0;
        true
    }

    /// Wait before the next try. False when the tries are spent.
    fn wait_retry(&self, tries: &mut u32, wait: Option<Duration>) -> bool {
        if *tries >= TRIES {
            return false;
        }
        let d = wait.unwrap_or(Duration::from_secs(1 << *tries));
        *tries += 1;
        std::thread::sleep(d.min(retry_cap()));
        true
    }
}

/// The longest wait between tries (short in tests).
fn retry_cap() -> Duration {
    if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(60) }
}

/// Take a connection slot of the process, or `None`.
fn take_conn() -> bool {
    CONNS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < MAX_ALL).then_some(n + 1)).is_ok()
}

/// Start a thread for segment `i`. The caller holds a connection slot.
fn spawn(job: &Arc<Job>, st: &mut State, i: usize, body: Option<Body>) {
    st.segs[i].running = true;
    st.threads += 1;
    let (j, generation) = (job.clone(), st.generation);
    let ok = std::thread::Builder::new().name("bare-download".into()).stack_size(8 << 20).spawn(move || j.segment(i, generation, body));
    if ok.is_err() {
        st.segs[i].running = false;
        st.threads -= 1;
        CONNS.fetch_sub(1, Ordering::Relaxed);
        st.failed.get_or_insert("no thread for the download".into());
    }
}

/// Cut the running segment with the most bytes left, and give the second half to a new thread.
fn split(job: &Arc<Job>, st: &mut State) -> bool {
    let running = st.segs.iter().filter(|s| s.running).count();
    if st.no_split || !job.resumable || running >= MAX_CONNS {
        return false;
    }
    let Some(i) = (0..st.segs.len()).filter(|&i| st.segs[i].running).max_by_key(|&i| st.segs[i].end - st.segs[i].pos.min(st.segs[i].end)) else { return false };
    let left = st.segs[i].end.saturating_sub(st.segs[i].pos);
    if left < 2 * MIN_SPLIT || !take_conn() {
        return false;
    }
    let mid = st.segs[i].pos + left / 2;
    let end = st.segs[i].end;
    st.segs[i].end = mid;
    st.segs.push(Seg { pos: mid, end, running: false });
    let k = st.segs.len() - 1;
    spawn(job, st, k, None);
    true
}

/// Run the segments that are not done again, when no thread runs (after `resume`, or when a thread ended early). The `running`
/// marks of an old generation are stale then, so they are cleared first.
fn revive(job: &Arc<Job>, st: &mut State) {
    for s in &mut st.segs {
        s.running = false;
    }
    for i in 0..st.segs.len() {
        let s = &st.segs[i];
        if !s.running && s.pos < s.end && st.failed.is_none() && take_conn() {
            spawn(job, st, i, None);
        }
    }
}

impl Downloads {
    /// Run a download that `Start` began: on a thread of its own, so the page and its workers go on. `name` is clean (`sanitize`).
    /// When `MAX_JOBS` hold a place, it waits as `Queued`, and its first reply waits unread (a resumable job asks again if that
    /// connection died meanwhile).
    pub(crate) fn begin(&self, id: usize, name: &str, mime: &str, start: Start) {
        let w = Waiting { id, name: name.to_string(), mime: mime.to_string(), start };
        let mut s = self.inner();
        if s.active >= MAX_JOBS {
            s.list[id].state = DownloadState::Queued;
            s.waiting.push_back(w);
            return;
        }
        s.active += 1;
        drop(s);
        self.launch(w);
    }

    fn launch(&self, w: Waiting) {
        let dl = self.clone();
        let id = w.id;
        let ok = std::thread::Builder::new().name("bare-download-job".into()).spawn(move || {
            dl.run(w.id, &w.name, &w.mime, w.start);
            dl.next();
        });
        if ok.is_err() {
            self.fail(id, "no thread for the download".into());
            self.next();
        }
    }

    /// A job gave its place back: start the next one that waits.
    fn next(&self) {
        let mut s = self.inner();
        s.active -= 1;
        let Some(w) = s.waiting.pop_front() else { return };
        s.active += 1;
        s.list[w.id].state = DownloadState::Running;
        drop(s);
        self.launch(w);
    }

    fn job(&self, id: usize) -> Result<Arc<Job>, String> {
        let s = self.inner();
        match s.list.get(id).map(|d| &d.state) {
            None => Err(format!("no download {}", id + 1)),
            Some(DownloadState::Queued) => Err("the download waits for a place; it has not started".into()),
            Some(DownloadState::Running) => s.jobs.get(&id).cloned().ok_or_else(|| "the download is starting; try again".into()),
            _ => s.jobs.get(&id).cloned().ok_or_else(|| "the download has ended".into()),
        }
    }

    /// Stop the transfer of download `id` and keep its bytes. `resume` goes on from them. A download that cannot resume (no ranges)
    /// cannot pause.
    pub fn pause(&self, id: usize) -> Result<(), String> {
        let job = self.job(id)?;
        if !job.resumable {
            return Err("the server does not allow resume for this file, so it cannot pause".into());
        }
        let mut st = job.lock();
        if !st.paused {
            st.paused = true;
            st.generation += 1;
        }
        drop(st);
        self.inner().list[id].state = DownloadState::Paused;
        job.cv.notify_all();
        Ok(())
    }

    pub fn resume(&self, id: usize) -> Result<(), String> {
        let job = self.job(id)?;
        job.lock().paused = false;
        self.inner().list[id].state = DownloadState::Running;
        job.cv.notify_all();
        Ok(())
    }

    /// Stop download `id` and remove its file. A queued one leaves the queue.
    pub fn cancel(&self, id: usize) -> Result<(), String> {
        let mut s = self.inner();
        if let Some(i) = s.waiting.iter().position(|w| w.id == id) {
            // Dropping the waiting reply closes its connection.
            s.waiting.remove(i);
            s.list[id].state = DownloadState::Cancelled;
            return Ok(());
        }
        drop(s);
        let job = self.job(id)?;
        let mut st = job.lock();
        st.cancelled = true;
        st.failed.get_or_insert("cancelled".into());
        drop(st);
        job.cv.notify_all();
        Ok(())
    }

    /// Limit download `id` to `bytes_per_second` (`None`: no limit).
    pub fn limit(&self, id: usize, bytes_per_second: Option<u64>) -> Result<(), String> {
        let job = self.job(id)?;
        job.lock().limit = bytes_per_second.filter(|&b| b > 0);
        Ok(())
    }

    fn run(&self, id: usize, name: &str, mime: &str, start: Start) {
        let Start { client, url, from, get, response, body } = start;
        let (total, validator, resumable) = plan(&response, get);
        if total.is_some_and(|t| t > MAX_DOWNLOAD) {
            return self.fail(id, "over 1 GiB".into());
        }
        let (final_file, path) = match self.create(name) {
            Ok(x) => x,
            Err(e) => return self.fail(id, format!("the file cannot be made: {}", e.kind())),
        };
        drop(final_file);
        let part = PathBuf::from(format!("{}.part", path.display()));
        let made = File::options().read(true).write(true).create(true).truncate(true).open(&part).and_then(|f| {
            if let Some(t) = total {
                f.set_len(t)?;
            }
            Ok(f)
        });
        let file = match made {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return self.fail(id, format!("the file cannot be made: {}", e.kind()));
            }
        };
        {
            let mut s = self.inner();
            s.list[id].path = path.clone();
            s.list[id].mime = mime.to_string();
            s.list[id].total = total;
        }
        let end = total.unwrap_or(u64::MAX);
        let st = State {
            segs: vec![Seg { pos: 0, end, running: false }],
            done: 0,
            no_split: false,
            changed: None,
            restarted: false,
            generation: 0,
            failed: None,
            threads: 0,
            paused: false,
            cancelled: false,
            limit: None,
            next_free: std::time::Instant::now(),
        };
        let job = Arc::new(Job { url, from, get, client, file, total, validator: Mutex::new(validator), resumable, st: Mutex::new(st), cv: Condvar::new() });
        self.inner().jobs.insert(id, job.clone());
        // The first segment always runs, even when the process is at `MAX_ALL`: it holds a connection already.
        CONNS.fetch_add(1, Ordering::Relaxed);
        let outcome = {
            let mut st = job.lock();
            spawn(&job, &mut st, 0, Some(body));
            loop {
                if st.failed.is_some() {
                    break Err(st.failed.clone().unwrap_or_default());
                }
                if let Some((r, b)) = st.changed.take() {
                    // The file changed: one segment from 0, with the new validator, and a new generation stops the old threads.
                    let (t2, v2, _) = plan(&r, true);
                    if t2 != job.total {
                        break Err("the file changed on the server".into());
                    }
                    *job.validator.lock().unwrap_or_else(|e| e.into_inner()) = v2;
                    st.generation += 1;
                    st.restarted = true;
                    st.done = 0;
                    st.segs = vec![Seg { pos: 0, end, running: false }];
                    if take_conn() {
                        spawn(&job, &mut st, 0, Some(b));
                    } else {
                        break Err("no connection for the download".into());
                    }
                    continue;
                }
                if st.segs.iter().all(|s| s.pos >= s.end) && st.threads == 0 {
                    break Ok(());
                }
                // Fill the free connections. A paused job only waits for its threads to stop.
                if !st.paused {
                    while split(&job, &mut st) {}
                    if st.threads == 0 {
                        revive(&job, &mut st);
                    }
                }
                self.inner().list[id].bytes = st.done;
                st = job.cv.wait_timeout(st, TICK).unwrap_or_else(|e| e.into_inner()).0;
            }
        };
        // Wait for the threads to stop before the file goes.
        {
            let mut st = job.lock();
            st.generation += 1;
            while st.threads > 0 {
                st = job.cv.wait_timeout(st, TICK).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
        let done = job.lock().done;
        let size = total.unwrap_or(job.lock().segs.first().map_or(0, |s| s.pos));
        let end = outcome.and_then(|()| std::fs::rename(&part, &path).map_err(|e| format!("the file cannot be renamed: {}", e.kind())));
        let cancelled = job.lock().cancelled;
        let mut s = self.inner();
        s.jobs.remove(&id);
        s.list[id].bytes = if end.is_ok() { size } else { done };
        s.list[id].state = match end {
            Ok(()) => DownloadState::Done,
            Err(why) => {
                let _ = std::fs::remove_file(&part);
                let _ = std::fs::remove_file(&path);
                if cancelled { DownloadState::Cancelled } else { DownloadState::Failed(why) }
            }
        };
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

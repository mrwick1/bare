use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;

/// A test server for one file. It serves ranges (when `ranges`), can cut a reply, limit the speed of each connection, change the file
/// and refuse ranges. It logs the `Range` header of each request ("-" for none).
#[derive(Clone, Default)]
struct Cfg {
    ranges: bool,
    /// Bytes per second for each connection (0: no limit).
    rate: usize,
    /// For the request with this index (0 first), close the connection after this many body bytes.
    cut: Vec<(usize, usize)>,
    /// From the second request on, the file is this one, with a new ETag.
    change: Option<Vec<u8>>,
    /// A request with `Range` gets 403.
    refuse_ranges: bool,
}

struct Srv {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
    /// The most connections that sent a body at the same time.
    peak: Arc<AtomicUsize>,
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[1] ^ seed).collect()
}

fn serve(data: Vec<u8>, cfg: Cfg) -> Srv {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/file.bin", l.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Vec::new()));
    let (peak, active) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (log2, peak2) = (log.clone(), peak.clone());
    let changed = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(c) = c else { break };
            let (data, cfg, log, peak, active, changed) = (data.clone(), cfg.clone(), log2.clone(), peak2.clone(), active.clone(), changed.clone());
            std::thread::spawn(move || {
                let _ = conn(c, &data, &cfg, &log, &peak, &active, &changed);
            });
        }
    });
    Srv { url, log, peak }
}

fn conn(c: TcpStream, data: &[u8], cfg: &Cfg, log: &Mutex<Vec<String>>, peak: &AtomicUsize, active: &AtomicUsize, changed: &AtomicBool) -> io::Result<()> {
    let mut r = BufReader::new(c.try_clone()?);
    let mut w = c;
    loop {
        let (mut range, mut if_range) = (None, None);
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        loop {
            let mut h = String::new();
            r.read_line(&mut h)?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap_or((h, ""));
            match k.to_ascii_lowercase().as_str() {
                "range" => range = Some(v.trim().to_string()),
                "if-range" => if_range = Some(v.trim().to_string()),
                _ => {}
            }
        }
        let index = {
            let mut l = log.lock().unwrap();
            l.push(range.clone().unwrap_or_else(|| "-".into()));
            l.len() - 1
        };
        let new_file = cfg.change.as_ref().filter(|_| index > 0);
        if new_file.is_some() {
            changed.store(true, Ordering::Relaxed);
        }
        let (data, etag) = match new_file {
            Some(d) => (d.as_slice(), "\"v2\""),
            None => (data, "\"v1\""),
        };
        if range.is_some() && cfg.refuse_ranges {
            w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")?;
            continue;
        }
        let want = range.as_deref().filter(|_| cfg.ranges && if_range.as_deref().is_none_or(|v| v == etag)).and_then(|r| {
            let (a, b) = r.strip_prefix("bytes=")?.split_once('-')?;
            let a: usize = a.parse().ok()?;
            let b: usize = if b.is_empty() { data.len() - 1 } else { b.parse::<usize>().ok()?.min(data.len() - 1) };
            Some((a, b))
        });
        let accept = if cfg.ranges { "Accept-Ranges: bytes\r\n" } else { "" };
        let (status, body, extra) = match want {
            Some((a, b)) => ("206 Partial Content", &data[a..=b], format!("Content-Range: bytes {a}-{b}/{}\r\n", data.len())),
            None => ("200 OK", data, String::new()),
        };
        write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nETag: {etag}\r\n{accept}{extra}\r\n", body.len())?;
        let cut = cfg.cut.iter().find(|(i, _)| *i == index).map(|(_, n)| *n);
        let now = active.fetch_add(1, Ordering::Relaxed) + 1;
        peak.fetch_max(now, Ordering::Relaxed);
        let sent = send(&mut w, body, cut, cfg.rate);
        active.fetch_sub(1, Ordering::Relaxed);
        sent?;
        if cut.is_some() {
            return Ok(());
        }
    }
}

fn send(w: &mut TcpStream, body: &[u8], cut: Option<usize>, rate: usize) -> io::Result<()> {
    let body = &body[..cut.unwrap_or(body.len()).min(body.len())];
    let step = if rate == 0 { 64 << 10 } else { (rate / 20).max(1) };
    for piece in body.chunks(step) {
        w.write_all(piece)?;
        if rate > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

/// Start a download of `url` in `dl` as the session does. Gives its number.
fn begin(dl: &Downloads, url: &str) -> usize {
    let mut client = Client::new();
    client.allow_big();
    let u = Url::parse(url).unwrap();
    let s = client.send_stream_once(&Request::get(&u)).unwrap();
    let response = Arc::try_unwrap(s.response).unwrap_or_else(|a| (*a).clone());
    let mut body = s.body;
    body.unlimited();
    let id = dl.start(url);
    dl.begin(id, "file.bin", "application/octet-stream", Start { client, url: u, from: None, get: true, response, body });
    id
}

fn folder(tag: &str) -> Downloads {
    let dir = std::env::temp_dir().join(format!("bare-dl-run-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Downloads::new(dir)
}

/// Wait until download `id` is not running or queued.
fn wait(dl: &Downloads, id: usize) -> super::super::Download {
    let end = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let d = dl.list()[id].clone();
        if !matches!(d.state, DownloadState::Running | DownloadState::Queued) || std::time::Instant::now() > end {
            return d;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Try `f` until the job has started (`try again`).
fn retry(f: impl Fn() -> Result<(), String>) {
    for _ in 0..200 {
        match f() {
            Err(e) if e.contains("try again") => std::thread::sleep(Duration::from_millis(5)),
            r => return r.unwrap(),
        }
    }
    panic!("the job did not start");
}

/// Download `url` as the session does, and wait for the end.
fn fetch(url: &str, tag: &str) -> (Downloads, super::super::Download) {
    let dl = folder(tag);
    let id = begin(&dl, url);
    let d = wait(&dl, id);
    (dl, d)
}

fn check(d: &super::super::Download, want: &[u8]) {
    assert_eq!(d.state, DownloadState::Done, "{d:?}");
    let got = std::fs::read(&d.path).unwrap();
    assert!(got == want, "the file differs: {} bytes, want {}", got.len(), want.len());
    assert!(!PathBuf::from(format!("{}.part", d.path.display())).exists());
    let _ = std::fs::remove_dir_all(d.path.parent().unwrap());
}

#[test]
fn a_small_file_is_one_request() {
    let data = pattern(16 << 10, 1);
    let srv = serve(data.clone(), Cfg { ranges: true, ..Cfg::default() });
    let (_, d) = fetch(&srv.url, "small");
    check(&d, &data);
    assert_eq!(*srv.log.lock().unwrap(), ["-"]);
}

#[test]
fn a_cut_reply_resumes_from_the_last_byte() {
    let data = pattern(1 << 20, 2);
    let srv = serve(data.clone(), Cfg { ranges: true, cut: vec![(0, 300_000)], ..Cfg::default() });
    let (_, d) = fetch(&srv.url, "resume");
    check(&d, &data);
    let log = srv.log.lock().unwrap().clone();
    assert_eq!(log[0], "-");
    assert!(log[1].starts_with("bytes=") && log[1] != "bytes=0-", "{log:?}");
    assert_eq!(log.len(), 2, "{log:?}");
}

#[test]
fn a_big_file_splits_over_several_connections() {
    let data = pattern(32 << 20, 3);
    let srv = serve(data.clone(), Cfg { ranges: true, rate: 8 << 20, ..Cfg::default() });
    let t = std::time::Instant::now();
    let (_, d) = fetch(&srv.url, "split");
    check(&d, &data);
    let log = srv.log.lock().unwrap().clone();
    assert!(log.len() >= 4, "{log:?}");
    assert!(srv.peak.load(Ordering::Relaxed) >= 4, "peak {}", srv.peak.load(Ordering::Relaxed));
    // One connection needs 4 s at 8 MiB/s.
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[test]
fn a_changed_file_starts_again_from_zero() {
    let (old, new) = (pattern(16 << 20, 4), pattern(16 << 20, 5));
    let srv = serve(old, Cfg { ranges: true, rate: 16 << 20, change: Some(new.clone()), ..Cfg::default() });
    let (_, d) = fetch(&srv.url, "changed");
    check(&d, &new);
}

#[test]
fn without_ranges_a_cut_reply_starts_again_once() {
    let data = pattern(1 << 20, 6);
    let srv = serve(data.clone(), Cfg { ranges: false, cut: vec![(0, 300_000)], ..Cfg::default() });
    let (_, d) = fetch(&srv.url, "plain");
    check(&d, &data);
    assert_eq!(*srv.log.lock().unwrap(), ["-", "-"]);
}

#[test]
fn a_refused_range_goes_back_to_the_first_connection() {
    let data = pattern(16 << 20, 7);
    let srv = serve(data.clone(), Cfg { ranges: true, rate: 16 << 20, refuse_ranges: true, ..Cfg::default() });
    let (_, d) = fetch(&srv.url, "refused");
    check(&d, &data);
}

#[test]
fn a_failed_download_leaves_no_file() {
    let data = pattern(1 << 20, 8);
    // Every reply is cut and nothing resumes: no ranges, and the one restart is cut too.
    let srv = serve(data, Cfg { ranges: false, cut: (0..10).map(|i| (i, 1000)).collect(), ..Cfg::default() });
    let (dl, d) = fetch(&srv.url, "fail");
    assert!(matches!(d.state, DownloadState::Failed(_)), "{d:?}");
    assert_eq!(std::fs::read_dir(dl.dir()).map_or(0, |r| r.count()), 0);
}

#[test]
fn a_reply_over_128_mib_downloads() {
    let data = pattern(130 << 20, 9);
    let srv = serve(data.clone(), Cfg::default());
    let (_, d) = fetch(&srv.url, "big");
    check(&d, &data);
}

#[test]
fn a_fourth_download_waits_for_a_place() {
    let data = pattern(2 << 20, 10);
    let srv = serve(data.clone(), Cfg { ranges: true, rate: 4 << 20, ..Cfg::default() });
    let dl = folder("queue");
    let ids: Vec<usize> = (0..4).map(|_| begin(&dl, &srv.url)).collect();
    assert_eq!(dl.list()[3].state, DownloadState::Queued);
    assert_eq!(dl.running(), 4);
    for &id in &ids {
        let d = wait(&dl, id);
        assert_eq!(d.state, DownloadState::Done, "{d:?}");
        assert!(std::fs::read(&d.path).unwrap() == data);
    }
    let _ = std::fs::remove_dir_all(dl.dir());
}

#[test]
fn a_paused_download_keeps_its_bytes_and_resumes() {
    let data = pattern(16 << 20, 11);
    let srv = serve(data.clone(), Cfg { ranges: true, rate: 4 << 20, ..Cfg::default() });
    let dl = folder("pause");
    let id = begin(&dl, &srv.url);
    std::thread::sleep(Duration::from_millis(300));
    retry(|| dl.pause(id));
    assert_eq!(dl.list()[id].state, DownloadState::Paused);
    assert_eq!(dl.running(), 0);
    // The threads stop after their next piece; then nothing moves.
    std::thread::sleep(Duration::from_millis(400));
    let a = dl.list()[id].bytes;
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(dl.list()[id].bytes, a);
    assert!(a > 0 && a < 16 << 20, "{a}");
    let requests = srv.log.lock().unwrap().len();
    dl.resume(id).unwrap();
    let d = wait(&dl, id);
    check(&d, &data);
    // The resumed ranges start where the bytes stopped, not at 0.
    let log = srv.log.lock().unwrap().clone();
    assert!(log[requests..].iter().all(|r| r.starts_with("bytes=") && r != "bytes=0-"), "{log:?}");
}

#[test]
fn a_cancelled_download_leaves_no_file() {
    let data = pattern(16 << 20, 12);
    let srv = serve(data, Cfg { ranges: true, rate: 2 << 20, ..Cfg::default() });
    let dl = folder("cancel");
    let id = begin(&dl, &srv.url);
    std::thread::sleep(Duration::from_millis(200));
    retry(|| dl.cancel(id));
    let d = wait(&dl, id);
    assert_eq!(d.state, DownloadState::Cancelled);
    assert_eq!(std::fs::read_dir(dl.dir()).map_or(0, |r| r.count()), 0);
}

#[test]
fn a_limit_slows_the_whole_job() {
    // 4 MiB at 16 MiB/s is 0.25 s without a limit (one connection: too small to split).
    let data = pattern(4 << 20, 13);
    let srv = serve(data.clone(), Cfg { ranges: true, rate: 16 << 20, ..Cfg::default() });
    let dl = folder("limit");
    let t = std::time::Instant::now();
    let id = begin(&dl, &srv.url);
    retry(|| dl.limit(id, Some(2 << 20)));
    let d = wait(&dl, id);
    // At 2 MiB/s it takes about 2 s; a few pieces pass before the limit is set.
    assert!(t.elapsed() > Duration::from_millis(1200), "{:?}", t.elapsed());
    check(&d, &data);
}

#[test]
fn a_download_without_ranges_cannot_pause() {
    let data = pattern(8 << 20, 14);
    let srv = serve(data.clone(), Cfg { ranges: false, rate: 8 << 20, ..Cfg::default() });
    let dl = folder("nopause");
    let id = begin(&dl, &srv.url);
    let mut r = Err(String::new());
    for _ in 0..200 {
        r = dl.pause(id);
        if !matches!(&r, Err(e) if e.contains("try again")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(r.unwrap_err().contains("cannot pause"));
    check(&wait(&dl, id), &data);
}

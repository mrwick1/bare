//! Downloads of a live session: a response that a person would save, not read, goes to a file and does not replace the page.
//!
//! A response is a download when it has `Content-Disposition: attachment`, or a type that the browser does not show (`text_mime`, images
//! and HTML are shown), or when a link with a `download` attribute asked for it. The link may point to a `blob:` or `data:` URL that a script
//! made. The body streams to a file in 64 KiB pieces, so the memory stays flat. A download stops at `MAX_DOWNLOAD` bytes and its file is removed.
//! The name comes from `Content-Disposition` (RFC 6266, `filename*` first), else the `download` attribute, else the last path segment.
//! It has no path separator and no leading dot, and it never replaces a file: the next one is `name (1).ext`.
//!
//! `Downloads` is a shared list: the session, its workers and the driver hold clones. A session made without it has no downloads, and a
//! response of a type that the page cannot show is a failed load, as before.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const MAX_DOWNLOAD: u64 = 1 << 30;
const PIECE: usize = 64 << 10;
const MAX_NAME: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Running,
    Done,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Download {
    /// Empty until the file exists.
    pub path: PathBuf,
    pub bytes: u64,
    pub mime: String,
    pub url: String,
    pub state: DownloadState,
}

/// A download that the page asked for with a `download` link (`a.click()` or a click): the session starts it.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub url: String,
    pub name: Option<String>,
    /// The document URL when the link was clicked.
    pub from: String,
    /// The bytes and type of a `blob:` or `data:` URL: no network.
    pub data: Option<(Vec<u8>, String)>,
    /// An iframe asked for it: save the reply only when it is a download, and drop any other reply unread.
    pub only: bool,
}

struct Inner {
    dir: PathBuf,
    list: Vec<Download>,
}

#[derive(Clone)]
pub struct Downloads(Arc<Mutex<Inner>>);

impl Downloads {
    pub fn new(dir: PathBuf) -> Downloads {
        Downloads(Arc::new(Mutex::new(Inner { dir, list: Vec::new() })))
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn dir(&self) -> PathBuf {
        self.inner().dir.clone()
    }

    pub fn list(&self) -> Vec<Download> {
        self.inner().list.clone()
    }

    pub fn len(&self) -> usize {
        self.inner().list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn running(&self) -> usize {
        self.inner().list.iter().filter(|d| d.state == DownloadState::Running).count()
    }

    /// Record a download that starts now. Gives its number.
    pub fn start(&self, url: &str) -> usize {
        let mut s = self.inner();
        s.list.push(Download { path: PathBuf::new(), bytes: 0, mime: String::new(), url: url.to_string(), state: DownloadState::Running });
        s.list.len() - 1
    }

    pub fn fail(&self, id: usize, why: String) {
        self.inner().list[id].state = DownloadState::Failed(why);
    }

    /// A new file in the folder: `name`, or `name (1).ext`, `name (2).ext` when it exists.
    fn create(&self, name: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => name.split_at(i),
            _ => (name, ""),
        };
        for n in 0u32.. {
            let path = dir.join(if n == 0 { name.to_string() } else { format!("{stem} ({n}){ext}") });
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => return Ok((f, path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        unreachable!("the counter does not end")
    }

    /// Stream `src` to a file called `name` and record the end of download `id`. A failure removes the file.
    pub fn save(&self, id: usize, name: &str, mime: &str, mut src: impl Read) {
        let (mut file, path) = match self.create(&sanitize(name)) {
            Ok(x) => x,
            Err(e) => return self.fail(id, format!("the file cannot be made: {}", e.kind())),
        };
        {
            let mut s = self.inner();
            s.list[id].path = path.clone();
            s.list[id].mime = mime.to_string();
        }
        let (mut piece, mut total) = (vec![0u8; PIECE], 0u64);
        let end = loop {
            match src.read(&mut piece) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    total += n as u64;
                    if total > MAX_DOWNLOAD {
                        break Err("over 1 GiB".to_string());
                    }
                    if let Err(e) = file.write_all(&piece[..n]) {
                        break Err(format!("write failed: {}", e.kind()));
                    }
                }
                Err(e) => break Err(format!("the transfer failed: {}", e.kind())),
            }
        };
        drop(file);
        let mut s = self.inner();
        s.list[id].bytes = total;
        s.list[id].state = match end {
            Ok(()) => DownloadState::Done,
            Err(why) => {
                let _ = std::fs::remove_file(&path);
                DownloadState::Failed(why)
            }
        };
    }
}

/// What a worker needs to save a response: where, and whether the response is a download whatever its headers say.
#[derive(Clone)]
pub(crate) struct Grab {
    pub dl: Downloads,
    /// A `download` link asked for it: save any reply.
    pub force: bool,
    /// The `download` attribute.
    pub name: Option<String>,
    /// The entry of a download that the session started (`Downloads::start`); else the load makes one.
    pub id: Option<usize>,
    /// Drop a reply that is not a download unread (an iframe `src`, not a document load).
    pub only: bool,
}

/// Is the response a download? `ct` is its `Content-Type`, `cd` its `Content-Disposition`.
pub(crate) fn is_download(ct: Option<&str>, cd: Option<&str>) -> bool {
    if cd.is_some_and(|c| disposition(c).0) {
        return true;
    }
    ct.is_some_and(|c| {
        let mime = c.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        !(mime.starts_with("image/") || mime == "application/xhtml+xml" || crate::browser_session::text_mime(&mime))
    })
}

/// The type of a response without parameters, or the generic one.
pub(crate) fn mime_of(ct: Option<&str>) -> String {
    ct.and_then(|c| c.split(';').next()).map(|m| m.trim().to_ascii_lowercase()).filter(|m| !m.is_empty()).unwrap_or_else(|| "application/octet-stream".into())
}

/// `Content-Disposition`: is it an attachment, and the file name (`filename*` before `filename`).
pub(crate) fn disposition(h: &str) -> (bool, Option<String>) {
    let mut parts = Vec::new();
    let (mut cur, mut quoted, mut esc) = (String::new(), false, false);
    for c in h.chars() {
        match c {
            _ if esc => {
                cur.push(c);
                esc = false;
            }
            '\\' if quoted => {
                cur.push(c);
                esc = true;
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let attachment = parts[0].trim().eq_ignore_ascii_case("attachment");
    let (mut plain, mut star) = (None, None);
    for p in &parts[1..] {
        let Some((k, v)) = p.split_once('=') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "filename*" {
            star = star.or_else(|| ext_value(v));
        } else if k == "filename" {
            plain = plain.or_else(|| Some(unquote(v)));
        }
    }
    (attachment, star.or(plain).filter(|n| !n.is_empty()))
}

fn unquote(v: &str) -> String {
    let Some(inner) = v.strip_prefix('"') else { return v.to_string() };
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '"' => break,
            c => out.push(c),
        }
    }
    out
}

/// RFC 8187: `charset'language'percent-encoded`, UTF-8 or ISO-8859-1.
fn ext_value(v: &str) -> Option<String> {
    let mut it = v.splitn(3, '\'');
    let (charset, _, text) = (it.next()?, it.next()?, it.next()?);
    let bytes = percent_decode(text);
    match charset.to_ascii_lowercase().as_str() {
        "utf-8" => Some(String::from_utf8_lossy(&bytes).into_owned()),
        "iso-8859-1" => Some(bytes.iter().map(|&b| b as char).collect()),
        _ => None,
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        let hex = |k: usize| b.get(k).and_then(|&c| (c as char).to_digit(16));
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The file name of a response: `Content-Disposition`, else the `download` attribute, else the last segment of the URL path.
pub(crate) fn file_name(cd: Option<&str>, attr: Option<&str>, url: &str) -> String {
    let from_url = || {
        let path = url.split(['?', '#']).next().unwrap_or("");
        let last = path.rsplit('/').next().unwrap_or("");
        String::from_utf8_lossy(&percent_decode(last)).into_owned()
    };
    let name = cd.and_then(|c| disposition(c).1).or_else(|| attr.filter(|a| !a.trim().is_empty()).map(str::to_string)).unwrap_or_else(from_url);
    sanitize(&name)
}

/// A name that is safe to join to the folder: no path separator, no control character, no leading dot, not too long, never empty.
pub(crate) fn sanitize(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let mut clean = clean.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if clean.len() > MAX_NAME {
        let mut end = MAX_NAME;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    if clean.is_empty() { "download".into() } else { clean }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_reads_the_type_and_the_name() {
        assert_eq!(disposition("attachment; filename=\"a b.csv\""), (true, Some("a b.csv".into())));
        assert_eq!(disposition("Attachment;filename=plain.pdf"), (true, Some("plain.pdf".into())));
        assert_eq!(disposition("inline; filename=x.txt"), (false, Some("x.txt".into())));
        assert_eq!(disposition("attachment; filename=\"a;b.txt\""), (true, Some("a;b.txt".into())));
        assert_eq!(disposition("attachment; filename=\"q\\\"x.txt\""), (true, Some("q\"x.txt".into())));
        assert_eq!(disposition("attachment; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt"), (true, Some("naïve file.txt".into())));
        assert_eq!(disposition("attachment; filename*=iso-8859-1''caf%E9.txt").1, Some("café.txt".into()));
        assert_eq!(disposition("attachment"), (true, None));
    }

    #[test]
    fn a_name_has_no_path_and_no_leading_dot() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("..\\..\\win.ini"), "win.ini");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("a\0b\nc.txt"), "abc.txt");
        assert_eq!(sanitize("..."), "download");
        assert_eq!(sanitize("").as_str(), "download");
        assert!(sanitize(&"é".repeat(300)).len() <= MAX_NAME);
    }

    #[test]
    fn the_name_follows_disposition_then_attribute_then_url() {
        assert_eq!(file_name(Some("attachment; filename=a.csv"), Some("b.csv"), "http://h/c.csv"), "a.csv");
        assert_eq!(file_name(None, Some("b.csv"), "http://h/c.csv"), "b.csv");
        assert_eq!(file_name(None, None, "http://h/dir/c%20d.csv?x=1#f"), "c d.csv");
        assert_eq!(file_name(None, None, "http://h/"), "download");
    }

    #[test]
    fn what_is_a_download() {
        assert!(is_download(Some("application/pdf"), None));
        assert!(is_download(Some("application/zip"), None));
        assert!(is_download(Some("text/html"), Some("attachment; filename=a.html")));
        assert!(!is_download(Some("text/html; charset=utf-8"), None));
        assert!(!is_download(Some("text/plain"), Some("inline")));
        assert!(!is_download(Some("image/svg+xml"), None));
        assert!(!is_download(Some("application/xhtml+xml"), None));
        assert!(!is_download(None, None));
    }

    #[test]
    fn a_file_never_replaces_another() {
        let dir = std::env::temp_dir().join(format!("bare-downloads-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dl = Downloads::new(dir.clone());
        for _ in 0..3 {
            let id = dl.start("u");
            dl.save(id, "r.csv", "text/csv", &b"abc"[..]);
        }
        let names: Vec<String> = dl.list().iter().map(|d| d.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["r.csv", "r (1).csv", "r (2).csv"]);
        assert!(dl.list().iter().all(|d| d.state == DownloadState::Done && d.bytes == 3));
        let _ = std::fs::remove_dir_all(dir);
    }
}

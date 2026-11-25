//! A persistent profile (`docs/internal/design-persistent-profile.md`, step 2): a directory that keeps the cookies and the
//! `localStorage` of a session, so that a new process opens with the same login. IndexedDB comes in step 3.
//!
//! ```text
//! <profile>/profile.lock   the process id of the holder; the holder keeps an OS lock on the file
//! <profile>/cookies.bin    all cookies, session cookies too (as Chrome keeps them for a session restore)
//! <profile>/local.bin      the `localStorage` items of all origins
//!
//! file    = 8 bytes magic, payload, u32 CRC-32 of the bytes before it
//! cookies = u32 count, (str name, str value, str domain, str path, u8 flags, u8 same site, f64 expires ms or NaN)*
//! local   = u32 origins, (str origin, u32 items, (str key, str value)*)*
//! str     = u32 length, UTF-8 bytes
//! ```
//!
//! Numbers are little endian. Flags: 1 host-only, 2 secure, 4 HttpOnly. Same site: 0 unset, 1 None, 2 Lax, 3 Strict.
//!
//! A write goes to a temporary file (mode 0600), `fsync`, then a rename over the old file: a crash leaves the old file or the new
//! one, never a part. The directory has mode 0700. The files hold live credentials: no message here holds a value.
//!
//! The lock is an OS lock (`File::try_lock`), so the OS frees it when the holder ends, also after a crash or a kill: a stale
//! `profile.lock` file is taken over. A second holder in the same process is refused too.
//!
//! A `Saver` (one per page) writes a file when its revision moved (`WebStorage::revisions`), at most once per second
//! (`Page::tick`), and at drop. sessionStorage is not saved, as in all browsers.

use crate::browser_navigation_storage::{OriginItems, StorageState, WebStorage};
use bare_net::cookie::{Cookie, SameSite};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The name of the profile that an entry point opens when it is given none.
pub const DEFAULT: &str = "default";
/// The shortest time between two writes of a profile.
const EVERY: Duration = Duration::from_secs(1);
const COOKIES: (&str, &[u8; 8]) = ("cookies.bin", b"bareck01");
const LOCAL: (&str, &[u8; 8]) = ("local.bin", b"barels01");

/// An open profile: its directory and its lock. Clones share the lock, which ends with the last clone.
#[derive(Clone)]
pub struct Profile {
    dir: PathBuf,
    _lock: Arc<std::fs::File>,
}

/// Why `Profile::open` failed.
#[derive(Debug)]
pub enum OpenError {
    /// Another holder has the lock: its process id, when the file names one.
    Locked(Option<u32>),
    Other(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            OpenError::Locked(Some(pid)) => write!(f, "the profile is in use by process {pid}"),
            OpenError::Locked(None) => write!(f, "the profile is in use by another process"),
            OpenError::Other(e) => f.write_str(e),
        }
    }
}

impl Profile {
    /// The directory of `name`: a value with a path separator is a path, a name is in `<data>/profiles`.
    pub fn dir_of(name: &str, data: Option<&Path>) -> Result<PathBuf, String> {
        if name.contains(['/', std::path::MAIN_SEPARATOR]) {
            return Ok(PathBuf::from(name));
        }
        if matches!(name, "" | "." | "..") {
            return Err(format!("profile {name:?}: not a profile name"));
        }
        let data = data.ok_or_else(|| format!("profile {name}: no data directory (BARE_DATA_DIR is empty); give a path"))?;
        Ok(data.join("profiles").join(name))
    }

    /// Open the profile in `dir`: make the directory (mode 0700) and take the lock.
    pub fn open(dir: &Path) -> Result<Profile, OpenError> {
        let err = |what: &str, e: io::Error| OpenError::Other(format!("{}: cannot {what} ({})", dir.display(), e.kind()));
        bare_os::fs::create_private_dir(dir).map_err(|e| err("make the profile directory", e))?;
        let path = dir.join("profile.lock");
        let mut f = bare_os::fs::open_private(&path).map_err(|e| err("open profile.lock", e))?;
        match f.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(OpenError::Locked(std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok())));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(err("lock profile.lock", e)),
        }
        f.set_len(0).and_then(|_| f.write_all(std::process::id().to_string().as_bytes())).map_err(|e| err("write profile.lock", e))?;
        Ok(Profile { dir: dir.to_path_buf(), _lock: Arc::new(f) })
    }

    /// What an entry point opens for `name` (`None`: `default`). The profile `default`, when it is locked or cannot open, gives
    /// `None` (memory) and says why, so that a second agent is not blocked. Another profile that cannot open is an error.
    /// The text names the profile for a receipt. With no name and no data directory (profiles are off) the text is empty.
    pub fn pick(name: Option<&str>, data: Option<&Path>) -> Result<(Option<Profile>, String), String> {
        if name.is_none() && data.is_none() {
            return Ok((None, String::new()));
        }
        let name = name.unwrap_or(DEFAULT);
        let opened = Profile::dir_of(name, data).map_err(OpenError::Other).and_then(|d| Profile::open(&d));
        match opened {
            Ok(p) => Ok((Some(p), format!("profile {name}"))),
            Err(e) if name == DEFAULT => Ok((None, format!("profile {name} not used ({e}): this session is in memory"))),
            Err(e) => Err(format!("profile {name}: {e}")),
        }
    }

    /// `pick` in the data directory of this machine (`bare_os::data_dir`).
    pub fn choose(name: Option<&str>) -> Result<(Option<Profile>, String), String> {
        Profile::pick(name, bare_os::data_dir().as_deref())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The saved cookies and `localStorage` (no file: nothing). A damaged file is an error that names it.
    pub fn load(&self) -> Result<StorageState, String> {
        let mut st = StorageState::default();
        if let Some(b) = self.read(COOKIES)? {
            st.cookies = decode_cookies(&b).ok_or_else(|| self.damaged(COOKIES.0))?;
        }
        if let Some(b) = self.read(LOCAL)? {
            st.local = decode_local(&b).ok_or_else(|| self.damaged(LOCAL.0))?;
        }
        Ok(st)
    }

    fn damaged(&self, name: &str) -> String {
        format!("{}: the profile file is damaged; delete it to start that part empty", self.dir.join(name).display())
    }

    /// The payload of a file whose magic and CRC are right. `None`: no file.
    fn read(&self, (name, magic): (&str, &[u8; 8])) -> Result<Option<Vec<u8>>, String> {
        let path = self.dir.join(name);
        let b = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: cannot read ({})", path.display(), e.kind())),
        };
        let ok = b.len() >= 12 && b.starts_with(magic) && {
            let (body, crc) = b.split_at(b.len() - 4);
            crc32fast::hash(body).to_le_bytes() == crc
        };
        ok.then(|| b[8..b.len() - 4].to_vec()).map(Some).ok_or_else(|| self.damaged(name))
    }

    /// Write a file: magic, payload and CRC to a temporary file (mode 0600), `fsync`, then a rename over the old one.
    fn write(&self, (name, magic): (&str, &[u8; 8]), payload: Vec<u8>) -> Result<(), String> {
        let (path, tmp) = (self.dir.join(name), self.dir.join(format!(".{name}.tmp")));
        let mut b = Vec::with_capacity(payload.len() + 12);
        b.extend_from_slice(magic);
        b.extend(payload);
        b.extend(crc32fast::hash(&b).to_le_bytes());
        // A crash during an earlier write can leave the temporary file; only this holder of the lock writes it.
        let _ = std::fs::remove_file(&tmp);
        let done = bare_os::fs::create_private(&tmp).and_then(|mut f| f.write_all(&b).and_then(|_| f.sync_all())).and_then(|_| std::fs::rename(&tmp, &path));
        done.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: cannot write ({})", path.display(), e.kind())
        })
    }
}

/// Saves the storage of one page to its profile.
pub(crate) struct Saver {
    profile: Profile,
    store: WebStorage,
    /// The revisions that are on disk.
    saved: (u64, u64),
    last: Option<Instant>,
    /// A write failed: the first failure goes to stderr, the next ones are quiet.
    failed: bool,
    /// The files written so far (for tests).
    pub(crate) writes: u32,
}

impl Saver {
    /// A saver for `store`, which holds what the profile holds now.
    pub(crate) fn new(profile: Profile, store: WebStorage) -> Saver {
        Saver { saved: store.revisions(), profile, store, last: None, failed: false, writes: 0 }
    }

    /// Write what changed, unless the last write was less than a second ago. Gives when to call again: a change waits for that time.
    pub(crate) fn tick(&mut self, now: Instant) -> Option<Instant> {
        let rev = self.store.revisions();
        if rev == self.saved {
            return None;
        }
        if let Some(due) = self.last.map(|t| t + EVERY).filter(|&due| now < due) {
            return Some(due);
        }
        self.last = Some(now);
        self.write(rev);
        None
    }

    /// Write what changed now.
    pub(crate) fn flush(&mut self) {
        let rev = self.store.revisions();
        if rev != self.saved {
            self.write(rev);
        }
    }

    fn write(&mut self, rev: (u64, u64)) {
        let mut done = Ok(());
        if rev.0 != self.saved.0 {
            done = self.profile.write(COOKIES, encode_cookies(&self.store.export_cookies()));
            self.writes += 1;
        }
        if rev.1 != self.saved.1 && done.is_ok() {
            done = self.profile.write(LOCAL, encode_local(&self.store.export_area(false)));
            self.writes += 1;
        }
        match done {
            Ok(()) => self.saved = rev,
            Err(e) if !self.failed => {
                self.failed = true;
                eprintln!("bare: {e}");
            }
            Err(_) => {}
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.flush();
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn encode_cookies(list: &[Cookie]) -> Vec<u8> {
    let mut out = (list.len() as u32).to_le_bytes().to_vec();
    for c in list {
        for s in [&c.name, &c.value, &c.domain, &c.path] {
            put_str(&mut out, s);
        }
        out.push(c.host_only as u8 | (c.secure as u8) << 1 | (c.http_only as u8) << 2);
        out.push(match c.same_site {
            SameSite::Unset => 0,
            SameSite::None => 1,
            SameSite::Lax => 2,
            SameSite::Strict => 3,
        });
        out.extend(c.expires.unwrap_or(f64::NAN).to_le_bytes());
    }
    out
}

fn encode_local(list: &OriginItems) -> Vec<u8> {
    let mut out = (list.len() as u32).to_le_bytes().to_vec();
    for (origin, items) in list {
        put_str(&mut out, origin);
        out.extend((items.len() as u32).to_le_bytes());
        for (k, v) in items {
            put_str(&mut out, k);
            put_str(&mut out, v);
        }
    }
    out
}

struct Reader<'b>(&'b [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn decode_cookies(b: &[u8]) -> Option<Vec<Cookie>> {
    let mut r = Reader(b);
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n.min(4096) as usize);
    for _ in 0..n {
        let (name, value, domain, path) = (r.str()?, r.str()?, r.str()?, r.str()?);
        let f = r.take(2)?;
        let mut c = Cookie::new(&name, &value, &domain, f[0] & 1 != 0, &path);
        (c.secure, c.http_only) = (f[0] & 2 != 0, f[0] & 4 != 0);
        c.same_site = match f[1] {
            0 => SameSite::Unset,
            1 => SameSite::None,
            2 => SameSite::Lax,
            3 => SameSite::Strict,
            _ => return None,
        };
        let e = f64::from_le_bytes(r.take(8)?.try_into().ok()?);
        c.expires = (!e.is_nan()).then_some(e);
        out.push(c);
    }
    r.0.is_empty().then_some(out)
}

fn decode_local(b: &[u8]) -> Option<OriginItems> {
    let mut r = Reader(b);
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n.min(1024) as usize);
    for _ in 0..n {
        let origin = r.str()?;
        let m = r.u32()?;
        let mut items = Vec::with_capacity(m.min(1024) as usize);
        for _ in 0..m {
            items.push((r.str()?, r.str()?));
        }
        out.push((origin, items));
    }
    r.0.is_empty().then_some(out)
}

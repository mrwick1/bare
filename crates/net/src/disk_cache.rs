//! The shared disk tier for public HTTP replies. I/O failure is a cache miss.

use super::{MAX_CACHE, MAX_HEAD, Response, disk_allowed};
use bare_tls::sha256::sha256;
use crc32fast::Hasher;
use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 8] = b"BAREHTTP";
const VERSION: u32 = 1;
const HEADER: usize = 24;
const MAX_META: usize = 2 * MAX_HEAD;
const CAP: u64 = 256 << 20;
type Id = [u8; 32];

pub(super) struct Disk {
    dir: PathBuf,
    cap: u64,
    pending: Arc<Mutex<Pending>>,
    worker: OnceLock<Option<(mpsc::Sender<Job>, JoinHandle<()>)>>,
}

#[derive(Default)]
struct Pending {
    keys: HashMap<Id, usize>,
    bytes: usize,
}

struct Job {
    id: Id,
    write: Option<(String, Arc<Response>, SystemTime, Duration)>,
    charge: usize,
}

impl Disk {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cap: CAP,
            pending: Arc::default(),
            worker: OnceLock::new(),
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<(Arc<Response>, Duration)> {
        let id = sha256(key.as_bytes());
        if self.pending.lock().unwrap().keys.contains_key(&id) {
            return None;
        }
        let path = self.dir.join(name(id));
        let result = read(&path, key);
        if result.as_ref().is_err_and(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
            )
        }) {
            // Retest under the writer lock. A writer could have replaced the file since we opened it.
            if let Ok(mut size) = open_size(&self.dir)
                && size.try_lock().is_ok()
                && read(&path, key).is_err_and(|e| {
                    matches!(
                        e.kind(),
                        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                    )
                })
            {
                let _ = remove(&self.dir, &mut size, &path);
            }
        }
        result.ok()
    }

    pub(super) fn put(&self, key: String, response: Arc<Response>, fresh_for: Duration) {
        // Bound the data retained after the memory cache evicts its own reference.
        let charge = response.body.len()
            + response
                .headers
                .iter()
                .map(|(n, v)| n.len() + v.len() + 64)
                .sum::<usize>()
            + key.len()
            + 512;
        self.queue(Job {
            id: sha256(key.as_bytes()),
            write: Some((key, response, SystemTime::now(), fresh_for)),
            charge,
        });
    }

    pub(super) fn forget(&self, key: &str) {
        let id = sha256(key.as_bytes());
        if self.pending.lock().unwrap().keys.contains_key(&id) || self.dir.join(name(id)).exists() {
            self.queue(Job {
                id,
                write: None,
                charge: 0,
            });
        }
    }

    fn queue(&self, job: Job) {
        let Some((tx, _)) = self.worker.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let (dir, cap, pending) = (self.dir.clone(), self.cap, self.pending.clone());
            let thread = thread::Builder::new()
                .name("http-cache".into())
                .spawn(move || {
                    for job in rx {
                        let _ = commit(&dir, cap, &job);
                        let mut pending = pending.lock().unwrap();
                        pending.bytes -= job.charge;
                        let n = pending.keys.get_mut(&job.id).unwrap();
                        *n -= 1;
                        if *n == 0 {
                            pending.keys.remove(&job.id);
                        }
                    }
                })
                .ok()?;
            Some((tx, thread))
        }) else {
            return;
        };
        let mut pending = self.pending.lock().unwrap();
        if pending.bytes + job.charge > MAX_CACHE {
            return;
        }
        let (id, charge) = (job.id, job.charge);
        if tx.send(job).is_ok() {
            *pending.keys.entry(id).or_default() += 1;
            pending.bytes += charge;
        }
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        if let Some(Some((tx, thread))) = self.worker.take() {
            drop(tx);
            let _ = thread.join();
        }
    }
}

fn name(id: Id) -> String {
    let mut out = String::with_capacity(64);
    for b in id {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

fn read(path: &Path, key: &str) -> io::Result<(Arc<Response>, Duration)> {
    let mut options = OpenOptions::new();
    options.read(true);
    // On Windows, updating the last-use time requires a handle with write access.
    #[cfg(windows)]
    options.write(true);
    let mut file = options.open(path)?;
    let mut head = [0; HEADER];
    file.read_exact(&mut head)?;
    let number = |n| u32::from_le_bytes(head[n..n + 4].try_into().unwrap());
    let (meta_len, body_len, crc) = (number(12) as usize, number(16) as usize, number(20));
    if &head[..8] != MAGIC
        || number(8) != VERSION
        || meta_len > MAX_META
        || body_len > MAX_CACHE
        || file.metadata()?.len() != (HEADER + meta_len + body_len) as u64
    {
        return Err(invalid());
    }
    let mut meta = vec![0; meta_len];
    file.read_exact(&mut meta)?;
    let mut rest = meta.as_slice();
    let stamp = u64::from_le_bytes(rest.split_off(..8).ok_or_else(invalid)?.try_into().unwrap());
    let lifetime = u64::from_le_bytes(rest.split_off(..8).ok_or_else(invalid)?.try_into().unwrap());
    if field(&mut rest)? != key {
        return Err(invalid());
    }
    let mut headers = Vec::new();
    while !rest.is_empty() {
        let (name, value) = (field(&mut rest)?, field(&mut rest)?);
        if name.is_empty()
            || !name.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
            })
            || value.bytes().any(|b| (b < b' ' && b != b'\t') || b == 127)
        {
            return Err(invalid());
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    let mut response = Response {
        status: 200,
        headers,
        ..Response::default()
    };
    if !disk_allowed(&response) {
        return Err(invalid());
    }
    response.body = vec![0; body_len];
    file.read_exact(&mut response.body)?;
    if checksum(&meta, &response.body) != crc {
        return Err(invalid());
    }
    let now = SystemTime::now();
    let remaining = now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| now.checked_sub(Duration::from_millis(stamp)))
        .map_or(Duration::ZERO, |age| {
            Duration::from_secs(lifetime).saturating_sub(age)
        });
    let _ = file.set_modified(now);
    Ok((Arc::new(response), remaining))
}

fn field<'a>(rest: &mut &'a [u8]) -> io::Result<&'a str> {
    let n = u32::from_le_bytes(rest.split_off(..4).ok_or_else(invalid)?.try_into().unwrap());
    let bytes = rest.split_off(..n as usize).ok_or_else(invalid)?;
    std::str::from_utf8(bytes).map_err(|_| invalid())
}

fn add_field(meta: &mut Vec<u8>, text: &str) {
    meta.extend_from_slice(&(text.len() as u32).to_le_bytes());
    meta.extend_from_slice(text.as_bytes());
}

fn checksum(meta: &[u8], body: &[u8]) -> u32 {
    let mut crc = Hasher::new();
    crc.update(meta);
    crc.update(body);
    crc.finalize()
}

fn open_size(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".size"))
}

fn commit(dir: &Path, cap: u64, job: &Job) -> io::Result<()> {
    let path = dir.join(name(job.id));
    let Some((key, response, stored, freshness)) = &job.write else {
        let mut size = open_size(dir)?;
        size.lock()?;
        return remove(dir, &mut size, &path);
    };

    let _span = bare_observe::process::span("network", "http_cache_write");
    let stamp = stored
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis() as u64;
    let mut meta = Vec::new();
    meta.extend_from_slice(&stamp.to_le_bytes());
    meta.extend_from_slice(&freshness.as_secs().to_le_bytes());
    add_field(&mut meta, key);
    for (name, value) in &response.headers {
        add_field(&mut meta, name);
        add_field(&mut meta, value);
    }
    let body = &response.body;
    let len = (HEADER + meta.len() + body.len()) as u64;
    if len > cap || meta.len() > MAX_META {
        return Ok(());
    }
    let mut head = [0; HEADER];
    head[..8].copy_from_slice(MAGIC);
    for (part, n) in head[8..].chunks_exact_mut(4).zip([
        VERSION,
        meta.len() as u32,
        body.len() as u32,
        checksum(&meta, body),
    ]) {
        part.copy_from_slice(&n.to_le_bytes());
    }

    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;

    let mut size = open_size(dir)?;
    size.lock()?;
    let (mut total, old) = (count(dir, &mut size)?, length(&path)?);
    if total.saturating_sub(old) + len > cap {
        size.set_len(0)?;
        let (actual, mut entries) = scan(dir)?;
        total = actual;
        entries.sort_unstable();
        // Evict down to the low watermark in one batch.
        let goal = (cap * 7 / 8).max(len);
        for (_, victim, bytes) in entries {
            if total.saturating_sub(old) + len <= goal {
                break;
            }
            if victim != path && fs::remove_file(victim).is_ok() {
                total -= bytes;
            }
        }
        if total.saturating_sub(old) + len > cap {
            return Ok(());
        }
    }
    size.set_len(0)?;
    // The lock gives all writers exclusive use of this temporary name, including after a crash.
    let temp = dir.join(".pending");
    let result = (|| {
        let _ = fs::remove_file(&temp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&head)?;
        file.write_all(&meta)?;
        file.write_all(body)?;
        drop(file);
        replace(&temp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result?;
    save_count(&mut size, total.saturating_sub(old) + len)
}

fn replace(temp: &Path, path: &Path) -> io::Result<()> {
    match fs::rename(temp, path) {
        // A Windows reader can keep the destination open while a new reply arrives.
        #[cfg(windows)]
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::AlreadyExists
            ) =>
        {
            fs::remove_file(path)?;
            fs::rename(temp, path)
        }
        result => result,
    }
}

fn length(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

fn remove(dir: &Path, size: &mut File, path: &Path) -> io::Result<()> {
    let len = length(path)?;
    if len == 0 {
        return fs::remove_file(path);
    }
    let total = count(dir, size)?;
    size.set_len(0)?;
    fs::remove_file(path)?;
    save_count(size, total.saturating_sub(len))
}

fn count(dir: &Path, size: &mut File) -> io::Result<u64> {
    let mut bytes = [0; 16];
    if size.read_exact(&mut bytes).is_ok() && size.metadata()?.len() == 16 {
        let total = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let inverse = u64::from_le_bytes(bytes[8..].try_into().unwrap());
        if total <= CAP && total == !inverse {
            return Ok(total);
        }
    }
    scan(dir).map(|(total, _)| total)
}

fn save_count(size: &mut File, count: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&count.to_le_bytes());
    bytes[8..].copy_from_slice(&(!count).to_le_bytes());
    size.seek(SeekFrom::Start(0))?;
    size.write_all(&bytes)
}

fn scan(dir: &Path) -> io::Result<(u64, Vec<(SystemTime, PathBuf, u64)>)> {
    let mut total = 0;
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let bytes = name.as_encoded_bytes();
        if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
            entries.push((
                meta.modified().unwrap_or(UNIX_EPOCH),
                entry.path(),
                meta.len(),
            ));
        }
    }
    Ok((total, entries))
}

#[cfg(test)]
#[path = "disk_cache_http_tests.rs"]
mod tests;

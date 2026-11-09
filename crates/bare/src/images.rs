//! `<img>`: find the pictures of a loaded document, download and decode them on worker threads, and give each one to its element.
//!
//! The page does not wait for a picture. `Page::tick` starts the downloads after `load` and takes the finished ones: it calls
//! `Document::set_image`, which logs one `Mark::Image`. The frame then lays out that box and what depends on it, and paints it. A
//! picture that fails (a status, a type that is not supported, a limit) leaves the element as it is.
//!
//! Source of an element: the first `<source srcset>` of its `<picture>` that has no `media` and a supported `type`, else `src`, else the
//! first URL of its own `srcset`. A `srcset` gives its first URL (the density and width descriptors are not read). A `data:` URL is
//! decoded here, and any other URL is resolved against the document and downloaded with the client of the session (cache, TLS, limits).
//!
//! Limits (`bare_image`): 32 MiB of file, 16 M pixels. `BUDGET` bounds the decoded pixels of the pictures of one page: a worker reserves
//! `4 * width * height` bytes from the header size before it allocates, and a picture over the budget fails.

use crate::page::{Page, Source};
use bare_html::dom::{Bitmap, Document, NodeData, NodeId};
use bare_net::http::{Client, Request};
use bare_net::parallel::Task;
use bare_url::Url;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Decoded bytes of all pictures of one page.
pub const BUDGET: usize = 64 << 20;
/// Pictures that one page asks for. More are ignored.
pub const MAX_PICTURES: usize = 512;
/// Downloads at once for one page. The workers are shared with the scripts and the `fetch` calls of the page.
const INFLIGHT: usize = 4;

#[derive(Default)]
pub(crate) struct Images {
    /// The document that the pictures are for (`Page::doc_id` + 1; 0 is none yet), and whether its tree was searched for `<img>`.
    doc: usize,
    scanned: Option<std::time::Instant>,
    /// Did the second look, at `load`, run? Scripts that ran after `interactive` may have added pictures.
    rescanned: bool,
    /// The elements that were asked for already.
    known: std::collections::HashSet<NodeId>,
    queue: VecDeque<(String, Vec<NodeId>)>,
    jobs: Vec<(Vec<NodeId>, Task<Option<Bitmap>>)>,
    used: Arc<AtomicUsize>,
    /// Pictures given to their elements, and pictures that failed.
    pub(crate) done: usize,
    pub(crate) failed: usize,
}

impl Images {
    /// Does the page still wait for a picture?
    pub(crate) fn pending(&self) -> bool {
        !self.queue.is_empty() || !self.jobs.is_empty()
    }
}

fn supported(kind: &str) -> bool {
    matches!(kind.trim().to_ascii_lowercase().as_str(), "image/png" | "image/jpeg" | "image/webp")
}

/// The first URL of a `srcset`.
fn first(srcset: &str) -> Option<&str> {
    srcset.split_whitespace().next().map(|u| u.trim_end_matches(',')).filter(|u| !u.is_empty())
}

/// The text that an `<img>` loads from, see the module comment.
fn source_of(doc: &Document, img: NodeId) -> Option<String> {
    let attr = |n: NodeId, name: &str| doc.find_atom(name).and_then(|a| doc.attr(n, a));
    if let Some(p) = doc.parent(img).filter(|&p| doc.element_name(p) == "picture") {
        for s in doc.children(p).take_while(|&c| c != img) {
            if matches!(doc.data(s), NodeData::Element { .. })
                && doc.element_name(s) == "source"
                && attr(s, "media").is_none()
                && attr(s, "type").is_none_or(supported)
                && let Some(u) = attr(s, "srcset").and_then(first)
            {
                return Some(u.to_string());
            }
        }
    }
    attr(img, "src").map(str::trim).filter(|s| !s.is_empty()).or_else(|| attr(img, "srcset").and_then(first)).map(str::to_string)
}

/// The bytes of a `data:` URL (base64 or percent-encoded).
fn data_url(url: &str) -> Option<Vec<u8>> {
    let (head, body) = url.strip_prefix("data:")?.split_once(',')?;
    if head.rsplit(';').next().is_some_and(|f| f.eq_ignore_ascii_case("base64")) {
        let mut out = Vec::with_capacity(body.len() / 4 * 3);
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in body.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                _ => return None,
            };
            acc = acc << 6 | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    } else {
        let b = body.as_bytes();
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
        Some(out)
    }
}

/// The body of `url`, with the cache of the client. At most `bare_image::MAX_BYTES`.
fn download(client: &mut Client, url: &str) -> Option<Vec<u8>> {
    let mut url = Url::parse(url).ok()?;
    for _ in 0..5 {
        let mut s = client.send_stream_cached(&Request::get(&url)).ok()?;
        if matches!(s.response.status, 301 | 302 | 303 | 307 | 308) {
            let next = url.join(s.response.header("location")?).ok()?;
            if url.scheme == bare_url::Scheme::Https && next.scheme == bare_url::Scheme::Http {
                return None;
            }
            url = next;
            continue;
        }
        if !(200..300).contains(&s.response.status) {
            return None;
        }
        let declared = s.response.header("content-length").and_then(|v| v.trim().parse::<usize>().ok());
        if declared.is_some_and(|d| d > bare_image::MAX_BYTES) {
            return None;
        }
        let mut bytes = Vec::with_capacity(declared.unwrap_or(0).min(bare_image::MAX_BYTES));
        (&mut s.body).take(bare_image::MAX_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() > bare_image::MAX_BYTES {
            return None;
        }
        client.keep(&url, &s.response, &bytes);
        return Some(bytes);
    }
    None
}

/// Download (or read) and decode one picture. Runs on a worker thread.
fn load(client: &mut Client, url: &str, used: &AtomicUsize) -> Option<Bitmap> {
    let bytes = if url.starts_with("data:") { data_url(url)? } else { download(client, url)? };
    // An SVG file is drawn by the painter of an inline `<svg>`, at its own size.
    if bare_image::kind(&bytes).is_none() {
        let text = std::str::from_utf8(&bytes).ok().filter(|t| t.contains("<svg"))?;
        let (w, h, px) = bare_paint::svg::bitmap(text)?;
        let need = px.len() * 4;
        if used.fetch_add(need, Ordering::Relaxed) + need > BUDGET {
            used.fetch_sub(need, Ordering::Relaxed);
            return None;
        }
        return Some((w, h, px.into()));
    }
    let (w, h) = bare_image::size(&bytes).ok()?;
    let need = w as usize * h as usize * 4;
    if used.fetch_add(need, Ordering::Relaxed) + need > BUDGET {
        used.fetch_sub(need, Ordering::Relaxed);
        return None;
    }
    let i = bare_image::decode(&bytes);
    drop(bytes);
    match i {
        Ok(i) => Some((i.width, i.height, i.pixels.into())),
        Err(_) => {
            used.fetch_sub(need, Ordering::Relaxed);
            None
        }
    }
}

/// A driver that takes a capture waits this long for the pictures that are still on their way.
const CAPTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

impl Page {
    /// How far the document is (`Browser::ready_level`); 0 while it loads. Pictures start at `interactive` and are looked for again at `load`.
    fn images_level(&mut self) -> u8 {
        match &mut self.source {
            // A page that stopped (a fatal error of a script) keeps its tree: the pictures in it still load.
            Source::Session(s) if s.loading().is_none() => {
                let stopped = s.ended().is_some();
                s.browser().map_or(0, |b| b.ready_level().max(u8::from(stopped)))
            }
            Source::Session(_) => 0,
            Source::Browser(b) => b.ready_level(),
        }
    }

    /// Do the pictures still load (or the document, which starts them), for less than `CAPTURE_WAIT`? A driver waits for them before a
    /// capture, and not before an `expect`.
    pub fn images_pending(&mut self) -> bool {
        // Look now: a loaded page with no picture needs no tick to say so.
        if self.images_step().0 {
            self.touched();
        }
        let since = self.images.scanned.unwrap_or(self.started);
        if since.elapsed() >= CAPTURE_WAIT {
            return false;
        }
        // Before the first look, wait only if the tree has an `<img>` now: a page with none needs no wait for `interactive`.
        match self.images.scanned {
            Some(_) => self.images.pending(),
            None => self.source.browser().is_some_and(|b| {
                let doc = b.document();
                let doc = doc.borrow();
                let mut stack = vec![doc.root()];
                while let Some(n) = stack.pop() {
                    if matches!(doc.data(n), NodeData::Element { .. }) && doc.element_name(n) == "img" {
                        return true;
                    }
                    stack.extend(doc.children(n));
                }
                false
            }),
        }
    }

    /// Start the downloads of a document that has loaded, and give the finished pictures to their elements. Gives whether one arrived, and
    /// whether a download ended (a failed one too: the caller must look at the page again).
    /// A page with no `<img>` pays one walk of the tree, once.
    pub(crate) fn images_step(&mut self) -> (bool, bool) {
        let id = self.doc_id() + 1;
        if self.images.doc != id {
            self.images = Images { doc: id, ..Images::default() };
        }
        let level = self.images_level();
        if level == 0 {
            return (false, false);
        }
        // The first look is a change for a caller that waits for it (`images_pending`), also when the page has no picture.
        let first = self.images.scanned.is_none();
        // The first look at the loaded document: the pictures in tree order, so that the first ones of the page download first.
        if self.images.scanned.is_none() || (level == 2 && !self.images.rescanned) {
            self.images.scanned.get_or_insert_with(std::time::Instant::now);
            self.images.rescanned = level == 2;
            let Some(b) = self.source.browser() else { return (false, false) };
            let doc = b.document();
            let doc = doc.borrow();
            let mut by_url: HashMap<String, usize> = HashMap::new();
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if matches!(doc.data(n), NodeData::Element { .. }) && doc.element_name(n) == "img" && self.images.known.insert(n) {
                    let Some(src) = source_of(&doc, n) else { continue };
                    let url = if src.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) { Ok(src) } else { b.resolve_url(&src) };
                    if let Ok(url) = url {
                        match by_url.get(&url) {
                            Some(&k) => self.images.queue[k].1.push(n),
                            None if by_url.len() < MAX_PICTURES => {
                                by_url.insert(url.clone(), self.images.queue.len());
                                self.images.queue.push_back((url, vec![n]));
                            }
                            None => {}
                        }
                    }
                }
                let at = stack.len();
                stack.extend(doc.children(n));
                stack[at..].reverse();
            }
        }
        while self.images.jobs.len() < INFLIGHT {
            let Some((url, nodes)) = self.images.queue.pop_front() else { break };
            let used = self.images.used.clone();
            let client = match &mut self.source {
                Source::Session(s) => s.fork(),
                Source::Browser(_) => Client::new(),
            };
            self.images.jobs.push((nodes, client.spawn(move |c| load(c, &url, &used))));
        }
        let (mut arrived, mut finished) = (false, first);
        let mut k = 0;
        while k < self.images.jobs.len() {
            if !self.images.jobs[k].1.done() {
                k += 1;
                continue;
            }
            let (nodes, task) = self.images.jobs.swap_remove(k);
            finished = true;
            match task.wait().flatten() {
                Some(bitmap) => {
                    if let Some(b) = self.source.browser() {
                        let doc = b.document();
                        let mut doc = doc.borrow_mut();
                        for n in nodes {
                            doc.set_image(n, Some(bitmap.clone()));
                        }
                    }
                    self.images.done += 1;
                    arrived = true;
                }
                None => self.images.failed += 1,
            }
        }
        (arrived, finished)
    }
}

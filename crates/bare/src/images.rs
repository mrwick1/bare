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
//! CSS images: `background-image` and `mask-image` name a picture by the number of its address (`bare_css::urls`). After each new set of
//! styles the loader reads the numbers that the unique styles of the frame use, loads the ones it has not seen (the same way, the same limits,
//! the same client) and gives each to the document (`Document::set_css_image`). The display list is then made again. A `url()` in a linked
//! style sheet was made absolute against the address of that sheet when the sheet was read (`browser_loader/sheets.rs`).
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
    /// The pictures on their way: the elements, the job, and the URL and the start (for `Page::pending`).
    jobs: Vec<(Vec<NodeId>, Task<Option<Bitmap>>, String, std::time::Instant)>,
    /// The CSS pictures: the numbers that were asked for, the ones that wait for a worker, and the ones on their way.
    css_seen: std::collections::HashSet<u32>,
    css_queue: VecDeque<(u32, String)>,
    css_jobs: Vec<(u32, Task<Option<Bitmap>>, String, std::time::Instant)>,
    /// The styles that were read last (the address of the `Styles` and its number of styles).
    css_read: (usize, usize, u64),
    used: Arc<AtomicUsize>,
    /// Pictures given to their elements, and pictures that failed.
    pub(crate) done: usize,
    pub(crate) failed: usize,
}

impl Images {
    /// Add the pictures on their way to `out` (`Page::pending`). A picture never holds the page (`Page::idle`).
    pub(crate) fn waiting(&self, out: &mut Vec<crate::page::Waiting>) {
        for (url, t) in self.jobs.iter().map(|j| (&j.2, j.3)).chain(self.css_jobs.iter().map(|j| (&j.2, j.3))) {
            out.push(crate::page::Waiting { kind: "image", method: "GET", url: url.clone(), age: t.elapsed(), holds: false, state: "" });
        }
    }

    /// Does the page still wait for a picture?
    pub(crate) fn pending(&self) -> bool {
        !self.queue.is_empty() || !self.jobs.is_empty() || !self.css_queue.is_empty() || !self.css_jobs.is_empty()
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
pub(crate) fn data_url(url: &str) -> Option<Vec<u8>> {
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
        Ok(i) => Some((i.width, i.height, i.pixels)),
        Err(_) => {
            used.fetch_sub(need, Ordering::Relaxed);
            None
        }
    }
}

/// A driver that takes a capture waits this long for the pictures that are still on their way.
const CAPTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Queue the picture of the `<img>` `n` (`by_url` joins elements with one URL). Gives false if it cannot be queued.
fn want(images: &mut Images, b: &crate::browser::Browser, doc: &Document, n: NodeId, by_url: &mut HashMap<String, usize>) -> bool {
    let Some(src) = source_of(doc, n) else { return false };
    let url = if src.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) { Ok(src) } else { b.resolve_url(&src) };
    let Ok(url) = url else { return false };
    match by_url.get(&url) {
        Some(&k) => images.queue[k].1.push(n),
        None if by_url.len() < MAX_PICTURES => {
            by_url.insert(url.clone(), images.queue.len());
            images.queue.push_back((url, vec![n]));
        }
        None => return false,
    }
    b.img_started(n);
    true
}

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
        // The styles of the frame say which CSS pictures the page needs. Make the frame when there is none, and when no picture is on its way
        // (a capture needs it anyway): not at each arrival, that would lay the page out again for each picture.
        if self.images_level() > 0 && (self.frame.is_none() || !self.images.pending()) {
            self.frame_now();
        }
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
                    want(&mut self.images, b, &doc, n, &mut by_url);
                }
                let at = stack.len();
                stack.extend(doc.children(n));
                stack[at..].reverse();
            }
        }
        // The pictures that scripts asked for (`new Image()` and `src`): the same loader, and the elements hear about the end.
        if self.source.browser().is_some_and(|b| b.img_requested()) {
            let Some(b) = self.source.browser() else { return (false, false) };
            let doc = b.document();
            let doc = doc.borrow();
            let mut by_url: HashMap<String, usize> = HashMap::new();
            for n in b.img_requests() {
                self.images.known.insert(n);
                // A request that cannot start has ended already.
                if !want(&mut self.images, b, &doc, n, &mut by_url) {
                    b.img_settled(n, false);
                }
            }
        }
        self.css_scan();
        while self.images.jobs.len() + self.images.css_jobs.len() < INFLIGHT {
            // The pictures of the tree first, then the ones of the styles (`id` is 0 for the first kind).
            let Some((url, nodes, id)) = self.images.queue.pop_front().map(|(u, n)| (u, n, 0)).or_else(|| self.images.css_queue.pop_front().map(|(i, u)| (u, Vec::new(), i))) else { break };
            let used = self.images.used.clone();
            let client = match &mut self.source {
                Source::Session(s) => s.fork(),
                Source::Browser(_) => Client::new(),
            };
            let (shown, now) = (url.clone(), std::time::Instant::now());
            let task = client.spawn(move |c| load(c, &url, &used));
            if id == 0 {
                self.images.jobs.push((nodes, task, shown, now));
            } else {
                self.images.css_jobs.push((id, task, shown, now));
            }
        }
        let (mut arrived, mut finished) = (false, first);
        let mut k = 0;
        while k < self.images.jobs.len() {
            if !self.images.jobs[k].1.done() {
                k += 1;
                continue;
            }
            let (nodes, task, ..) = self.images.jobs.swap_remove(k);
            finished = true;
            let bitmap = task.wait().flatten();
            if let Some(b) = self.source.browser() {
                let doc = b.document();
                for &n in &nodes {
                    let mut doc = doc.borrow_mut();
                    match &bitmap {
                        Some(bitmap) => doc.set_image(n, Some(bitmap.clone())),
                        // The new `src` failed: the old picture goes.
                        None if doc.image(n).is_some() => doc.set_image(n, None),
                        None => {}
                    }
                }
                for n in nodes {
                    b.img_settled(n, bitmap.is_some());
                }
            }
            if bitmap.is_some() {
                self.images.done += 1;
                arrived = true;
            } else {
                self.images.failed += 1;
            }
        }
        let mut k = 0;
        let mut css_arrived = false;
        while k < self.images.css_jobs.len() {
            if !self.images.css_jobs[k].1.done() {
                k += 1;
                continue;
            }
            let (id, task, ..) = self.images.css_jobs.swap_remove(k);
            finished = true;
            match (task.wait().flatten(), self.source.browser()) {
                (Some(bitmap), Some(b)) => {
                    b.document().borrow_mut().set_css_image(id, bitmap);
                    self.images.done += 1;
                    css_arrived = true;
                }
                _ => self.images.failed += 1,
            }
        }
        if css_arrived {
            // Layout does not read a CSS picture: only the display list is made again.
            self.relist();
        }
        (arrived || css_arrived, finished)
    }

    /// Ask for the CSS pictures that the styles of the frame use and that were not asked for yet. The unique styles are read when the
    /// styles of the frame are new, so a page with no such picture pays one pass over a few hundred styles for each new frame.
    fn css_scan(&mut self) {
        let Some(f) = &self.frame else { return };
        let read = (std::rc::Rc::as_ptr(&f.styles) as usize, f.styles.unique(), self.epoch);
        if self.images.css_read == read {
            return;
        }
        self.images.css_read = read;
        let mut ids = Vec::new();
        for style in f.styles.each().filter(|s| s.picture_set() != 0) {
            let set = style.pictures();
            for p in [bare_css::values::Property::BackgroundImage, bare_css::values::Property::MaskImage] {
                if let bare_css::values::Value::Number(x) = set[p as usize - bare_css::values::Property::CORE] {
                    ids.push(x as u32);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let Some(b) = self.source.browser() else { return };
        for id in ids {
            if self.images.css_seen.len() >= MAX_PICTURES || !self.images.css_seen.insert(id) {
                continue;
            }
            let Some(raw) = bare_css::urls::get(id) else { continue };
            let url = if raw.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) { Ok(raw.to_string()) } else { b.resolve_url(&raw) };
            if let Ok(url) = url {
                self.images.css_queue.push_back((id, url));
            }
        }
    }
}

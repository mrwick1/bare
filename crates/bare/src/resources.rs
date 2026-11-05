//! Resource loading (S1 topic 6): find the external scripts and style sheets of a page, and download them.
//!
//! This is preparation. Nothing here runs JavaScript or parses CSS. `discover` reads the DOM and gives the
//! references in the order of the page, with the `async`/`defer`/`module` facts that the script scheduler (S3) needs.
//! `load` downloads each distinct URL once, one after the other, with hard limits.
//!
//! Rules:
//! - URLs are resolved against the first `<base href>` that resolves, else against the page URL. (The HTML standard
//!   uses the first `<base href>` only, even if it is bad. We skip a bad one, as the task asked.)
//! - Scripts: `type` absent, empty or a JavaScript MIME type (exact, no parameters) is a classic script. `module` is a
//!   module script: the facts are kept, nothing runs it. Any other type (`application/json`, `importmap`, a template)
//!   is data, not code: it is listed as skipped. A classic script with `nomodule` is skipped: `bare` is a browser
//!   that knows modules, and a browser like that does not run it.
//! - Timing: a classic external script is `Async` with `async`, else `Defer` with `defer`, else `Blocking`. A classic
//!   inline script is always `Blocking` (`async` and `defer` do nothing there). A module is `Async` with `async`, else
//!   `Defer`.
//! - Stylesheets: `<link rel=stylesheet>` without `alternate`. `media` is not read: the node is kept, so a later step can.
//!   Preloads, icons and `<style>` are not resources of this step. Elements inside `<template>` are ignored.
//! - A response is good when the status is 2xx, the `Content-Type` is a JavaScript type (scripts) or `text/css`
//!   (styles), and `Content-Encoding` is absent or `identity`. We send no `Accept-Encoding`, so the server sends
//!   plain bytes. A compressed body is an error: we never treat compressed bytes as source.
//! - The text is UTF-8 unless `charset` names another encoding that `bare_html::encoding` knows.
//!
//! Memory: the body is read into one buffer that becomes the `String` (no second copy for UTF-8). The client cache is
//! not used, because `get_stream` does not put replies in it. An inline script is not copied: `inline_source` reads
//! the DOM text.
//!
//! Time: `Limits::max_time` is checked before each request and between reads. A blocking call is not interrupted:
//! one DNS lookup, connect (10 s) or read (30 s) in `bare_net` can pass the limit by that much before we notice.
//!
//! This file uses no `crate::` path, so the load harness (`examples/resources_load.rs`) can include it with `#[path]`.

use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_html::encoding;
use bare_net::http;
use bare_url::Url;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::io::{ErrorKind, Read};
use std::time::{Duration, Instant};

/// Elements (scripts and style sheet links) in one page. More is an error: a page cannot make us hold a million nodes.
pub const MAX_REFERENCES: usize = 4096;
/// Distinct URLs to download. The staging login page needs 18, and its home page 41.
pub const MAX_FETCHES: usize = 128;
/// One resource, after decoding. The biggest staging file is 391 KB, so this is 20 times more.
pub const MAX_RESOURCE_BYTES: usize = 8 * 1024 * 1024;
/// All the text that we keep. The 52 staging files have 1.86 MB.
pub const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
/// All downloads together. See the module comment about blocking calls.
pub const MAX_TIME: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_references: usize,
    pub max_fetches: usize,
    pub max_resource_bytes: usize,
    pub max_total_bytes: usize,
    pub max_time: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_references: MAX_REFERENCES,
            max_fetches: MAX_FETCHES,
            max_resource_bytes: MAX_RESOURCE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_time: MAX_TIME,
        }
    }
}

/// When a script runs, for the scheduler of S3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// The parser waits for it and runs it at once.
    Blocking,
    /// It runs after the page is parsed, in the order of the page.
    Defer,
    /// It runs when it has arrived, in any order.
    Async,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The text is in the DOM: see `inline_source`.
    Inline,
    /// An index into `Discovery::targets`.
    External(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A `type` that is not JavaScript or `module`: data, not code.
    UnsupportedType,
    /// A classic script with `nomodule`.
    NoModule,
    /// `src` or `href` is empty.
    EmptyUrl,
    /// The URL does not resolve to an `http` or `https` URL.
    BadUrl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `module` only says what the script is. Nothing runs a module yet.
    Script { module: bool, timing: Timing, source: Source },
    /// An index into `Discovery::targets`.
    Stylesheet { target: usize },
    Skipped(Skip),
}

/// One `<script>` or `<link rel=stylesheet>` of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub node: NodeId,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Expect {
    Script,
    Style,
    /// Any content type and encoding (a font, an image, a `fetch` preload). The body is read and dropped, never kept: the reply
    /// is `Resource` with empty text. Status, redirects, the time limit and the size cap of one resource still apply.
    Opaque,
}

/// A URL to download, and what it must be.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub expect: Expect,
    pub url: Url,
}

#[derive(Debug)]
pub struct Discovery {
    /// In the order of the page. A URL that two items use is one target.
    pub items: Vec<Item>,
    pub targets: Vec<Target>,
    /// The document base URL (the first good `<base href>`, else the page). A script that the page adds later resolves against it.
    pub base: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiscoverError {
    TooManyReferences(usize),
    TooManyFetches(usize),
}

impl fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DiscoverError::TooManyReferences(n) => write!(f, "more than {n} scripts and style sheets in the page"),
            DiscoverError::TooManyFetches(n) => write!(f, "more than {n} different resource URLs in the page"),
        }
    }
}

/// The JavaScript MIME types of the HTML standard. `type` and `Content-Type` are compared with them, ignoring ASCII case.
pub const JS_TYPES: [&str; 16] = [
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
];

pub const WS: [char; 5] = [' ', '\t', '\n', '\x0c', '\r'];

struct Atoms {
    script: Option<Atom>,
    link: Option<Atom>,
    base: Option<Atom>,
    template: Option<Atom>,
    src: Option<Atom>,
    ty: Option<Atom>,
    async_: Option<Atom>,
    defer: Option<Atom>,
    nomodule: Option<Atom>,
    rel: Option<Atom>,
    href: Option<Atom>,
}

/// Find the scripts and style sheets of the page. `page` is the URL of the page.
pub fn discover(doc: &Document, page: &Url, limits: &Limits) -> Result<Discovery, DiscoverError> {
    // A name that no node has cannot be on any element, so `None` means "never present".
    let a = |n: &str| doc.find_atom(n);
    let at = Atoms {
        script: a("script"),
        link: a("link"),
        base: a("base"),
        template: a("template"),
        src: a("src"),
        ty: a("type"),
        async_: a("async"),
        defer: a("defer"),
        nomodule: a("nomodule"),
        rel: a("rel"),
        href: a("href"),
    };
    let attr = |el: NodeId, name: Option<Atom>| name.and_then(|n| doc.attr(el, n));

    // One walk, in the order of the page. Collect the candidates. The base can come after a script, so URLs wait.
    let mut candidates: Vec<NodeId> = Vec::new();
    let mut base: Option<Url> = None;
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        // An SVG `script`, `link` or `base` is not an HTML resource (the loader would be wrong to run or fetch it).
        if let NodeData::Element { name, .. } = *doc.data(node)
            && !doc.is_foreign(node)
        {
            let tag = Some(name);
            if tag == at.template {
                continue;
            } else if tag == at.script {
                candidates.push(node);
            } else if tag == at.link {
                if let Some(rel) = attr(node, at.rel) {
                    let has = |word: &str| rel.split_ascii_whitespace().any(|w| w.eq_ignore_ascii_case(word));
                    if has("stylesheet") && !has("alternate") {
                        candidates.push(node);
                    }
                }
            } else if tag == at.base && base.is_none() {
                base = attr(node, at.href).and_then(|h| page.join(h.trim_matches(WS)).ok());
            }
            if candidates.len() > limits.max_references {
                return Err(DiscoverError::TooManyReferences(limits.max_references));
            }
        }
        let mut child = doc.last_child(node);
        while let Some(c) = child {
            stack.push(c);
            child = doc.prev_sibling(c);
        }
    }
    let base = base.as_ref().unwrap_or(page);

    let mut items = Vec::with_capacity(candidates.len());
    let mut targets: Vec<Target> = Vec::new();
    let mut seen: HashMap<(Expect, String), usize> = HashMap::new();
    let mut by_raw: HashMap<(Expect, &str), usize> = HashMap::new();
    for node in candidates {
        let NodeData::Element { name, .. } = *doc.data(node) else { continue };
        let is_script = Some(name) == at.script;
        let (expect, url_attr) = if is_script { (Expect::Script, attr(node, at.src)) } else { (Expect::Style, attr(node, at.href)) };

        // A script with a type that is not code is skipped before anything else.
        let mut module = false;
        if is_script {
            match attr(node, at.ty).map(|t| t.trim_matches(WS)) {
                None | Some("") => {}
                Some(t) if t.eq_ignore_ascii_case("module") => module = true,
                Some(t) if JS_TYPES.iter().any(|j| t.eq_ignore_ascii_case(j)) => {}
                Some(_) => {
                    items.push(Item { node, kind: Kind::Skipped(Skip::UnsupportedType) });
                    continue;
                }
            }
            if !module && attr(node, at.nomodule).is_some() {
                items.push(Item { node, kind: Kind::Skipped(Skip::NoModule) });
                continue;
            }
        }
        let timing = |external: bool| {
            let has = |n: Option<Atom>| attr(node, n).is_some();
            if module {
                if has(at.async_) { Timing::Async } else { Timing::Defer }
            } else if !external {
                Timing::Blocking
            } else if has(at.async_) {
                Timing::Async
            } else if has(at.defer) {
                Timing::Defer
            } else {
                Timing::Blocking
            }
        };

        let Some(raw) = url_attr else {
            // A script with no `src` is inline. A link with no `href` has nothing to load.
            let kind = if is_script { Kind::Script { module, timing: timing(false), source: Source::Inline } } else { Kind::Skipped(Skip::EmptyUrl) };
            items.push(Item { node, kind });
            continue;
        };
        let raw = raw.trim_matches(WS);
        if raw.is_empty() {
            items.push(Item { node, kind: Kind::Skipped(Skip::EmptyUrl) });
            continue;
        }
        // The same text always resolves to the same URL (one base), so a repeat skips the resolve and the key text.
        let index = if let Some(&i) = by_raw.get(&(expect, raw)) {
            i
        } else {
            let Ok(url) = base.join(raw) else {
                items.push(Item { node, kind: Kind::Skipped(Skip::BadUrl) });
                continue;
            };
            let key = (expect, url.to_string());
            let i = match seen.get(&key) {
                Some(&i) => i,
                None => {
                    if targets.len() >= limits.max_fetches {
                        return Err(DiscoverError::TooManyFetches(limits.max_fetches));
                    }
                    targets.push(Target { expect, url });
                    seen.insert(key, targets.len() - 1);
                    targets.len() - 1
                }
            };
            by_raw.insert((expect, raw), i);
            i
        };
        let kind = if is_script { Kind::Script { module, timing: timing(true), source: Source::External(index) } } else { Kind::Stylesheet { target: index } };
        items.push(Item { node, kind });
    }
    Ok(Discovery { items, targets, base: base.to_string() })
}

/// The document base URL now: the first `<base href>` that resolves against `page`, else `None` (the base is `page`; the rule of `discover`).
/// The driver calls it again after scripts ran, because a script can add or change a `<base>`.
pub fn document_base(doc: &Document, page: &Url) -> Option<Url> {
    let (base, template, href) = (doc.find_atom("base"), doc.find_atom("template"), doc.find_atom("href"));
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { name, .. } = *doc.data(node)
            && !doc.is_foreign(node)
        {
            if Some(name) == template {
                continue;
            }
            if Some(name) == base
                && let Some(u) = href.and_then(|h| doc.attr(node, h)).and_then(|h| page.join(h.trim_matches(WS)).ok())
            {
                return Some(u);
            }
        }
        let mut child = doc.last_child(node);
        while let Some(c) = child {
            stack.push(c);
            child = doc.prev_sibling(c);
        }
    }
    None
}

/// The text of an inline script: the text children of the element. One child (the usual case) is borrowed, not copied.
pub fn inline_source(doc: &Document, node: NodeId) -> Cow<'_, str> {
    let mut texts = doc.children(node).filter_map(|c| if let NodeData::Text(s) = *doc.data(c) { Some(doc.text(s)) } else { None });
    let Some(first) = texts.next() else { return Cow::Borrowed("") };
    match texts.next() {
        None => Cow::Borrowed(first),
        Some(second) => Cow::Owned([first, second].into_iter().chain(texts).collect()),
    }
}

/// A downloaded resource. The server bytes are decoded to text, and the bytes are not kept.
#[derive(Debug)]
pub struct Resource {
    /// Final response URL, used as the base of relative CSS imports.
    pub url: Url,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The request failed: DNS, connect, TLS validation, or a bad reply.
    Network(String),
    /// A redirect is invalid, too deep, or would downgrade HTTPS.
    Redirect,
    Status(u16),
    /// The `Content-Type` is not allowed. Holds the type (up to 64 chars), or `None` when it is missing.
    Mime(Option<String>),
    /// A `Content-Encoding` other than `identity`.
    Encoding(String),
    /// The resource is over `max_resource_bytes`, by `Content-Length` or by the bytes that came.
    TooLarge,
    /// This resource would pass `max_total_bytes`.
    TotalTooLarge,
    /// The connection closed before the whole body came.
    Truncated,
    /// A read failed, or the limit of time passed.
    Read(ErrorKind),
    TimedOut,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LoadError::Network(e) => write!(f, "request failed: {e}"),
            LoadError::Redirect => write!(f, "resource redirect refused"),
            LoadError::Status(s) => write!(f, "status {s}"),
            LoadError::Mime(Some(m)) => write!(f, "content type {m:?} is not allowed here"),
            LoadError::Mime(None) => write!(f, "no content type"),
            LoadError::Encoding(e) => write!(f, "content encoding {e:?} is not supported"),
            LoadError::TooLarge => write!(f, "the resource is too big"),
            LoadError::TotalTooLarge => write!(f, "the resources together are too big"),
            LoadError::Truncated => write!(f, "the body is cut short"),
            LoadError::Read(k) => write!(f, "read failed: {k}"),
            LoadError::TimedOut => write!(f, "the time limit passed"),
        }
    }
}

/// The results, one for each target in the same order.
#[derive(Debug)]
pub struct Loaded {
    pub results: Vec<Result<Resource, LoadError>>,
    /// The bytes of text that the good results hold.
    pub retained: usize,
}

/// The part of a `Content-Type` before the first `;`, without spaces. The case is not changed.
fn essence(value: &str) -> &str {
    value.split(';').next().unwrap_or("").trim_matches(WS)
}

/// The `charset` parameter, without quotes.
fn charset(value: &str) -> Option<&str> {
    value.split(';').skip(1).find_map(|p| {
        let (name, v) = p.split_once('=')?;
        name.trim_matches(WS).eq_ignore_ascii_case("charset").then(|| v.trim_matches(WS).trim_matches('"'))
    })
}

fn short(text: &str) -> String {
    text.chars().take(64).collect()
}

/// Download every target, one after the other. A failure of one target does not stop the others. A limit that is
/// global (time, total size) fails the targets that it reaches.
pub fn load(targets: &[Target], limits: &Limits) -> Loaded {
    let deadline = Instant::now() + limits.max_time;
    let mut client = http::Client::new();
    let mut retained = 0;
    let mut results = Vec::with_capacity(targets.len());
    for target in targets {
        let result = fetch(&mut client, target, limits, deadline, retained);
        if let Ok(r) = &result {
            retained += r.text.len();
        }
        results.push(result);
    }
    Loaded { results, retained }
}

fn redirect_target(current: &Url, location: &str) -> Result<Url, LoadError> {
    let next = current.join(location).map_err(|_| LoadError::Redirect)?;
    if current.scheme == bare_url::Scheme::Https && next.scheme == bare_url::Scheme::Http { return Err(LoadError::Redirect); }
    Ok(next)
}

/// Download one target with the caps of `limits`. `retained` is the text that the caller already keeps (the total cap),
/// and `deadline` ends the work. The caller counts fetches.
pub fn fetch(client: &mut http::Client, target: &Target, limits: &Limits, deadline: Instant, retained: usize) -> Result<Resource, LoadError> {
    if Instant::now() >= deadline {
        return Err(LoadError::TimedOut);
    }
    let mut current = Url { scheme: target.url.scheme, host: target.url.host.clone(), port: target.url.port, path: target.url.path.clone() };
    let mut redirects = 0;
    let mut streamed = loop {
        if Instant::now() >= deadline { return Err(LoadError::TimedOut); }
        let streamed = client.send_stream_once(&http::Request::get(&current)).map_err(|e| LoadError::Network(e.to_string()))?;
        if !matches!(streamed.response.status, 301 | 302 | 303 | 307 | 308) { break streamed; }
        if redirects == 20 { return Err(LoadError::Redirect); }
        let Some(location) = streamed.response.header("location") else { break streamed };
        let next = redirect_target(&current, location)?;
        current = next;
        redirects += 1;
    };
    let response = &streamed.response;
    if !(200..300).contains(&response.status) {
        return Err(LoadError::Status(response.status));
    }
    let content_type = response.header("content-type");
    let good = target.expect == Expect::Opaque
        || content_type.map(essence).is_some_and(|m| match target.expect {
            Expect::Script => JS_TYPES.iter().any(|j| m.eq_ignore_ascii_case(j)),
            _ => m.eq_ignore_ascii_case("text/css"),
        });
    if !good {
        return Err(LoadError::Mime(content_type.map(|c| short(essence(c)))));
    }
    if target.expect != Expect::Opaque
        && let Some(e) = response.header("content-encoding")
        && !e.trim_matches(WS).eq_ignore_ascii_case("identity")
    {
        return Err(LoadError::Encoding(short(e)));
    }
    if target.expect == Expect::Opaque {
        // Count the bytes against the size cap of one resource, in one small buffer. Nothing is kept.
        let declared = response.header("content-length").and_then(|v| v.trim_matches(WS).parse::<usize>().ok());
        if declared.is_some_and(|d| d > limits.max_resource_bytes) {
            return Err(LoadError::TooLarge);
        }
        let (mut buf, mut total) = ([0u8; 8 * 1024], 0usize);
        loop {
            if Instant::now() >= deadline {
                return Err(LoadError::TimedOut);
            }
            match streamed.body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(LoadError::Truncated),
                Err(e) => return Err(LoadError::Read(e.kind())),
            }
            if total > limits.max_resource_bytes {
                return Err(LoadError::TooLarge);
            }
        }
        return Ok(Resource { url: current, text: String::new() });
    }
    let (cap, room) = (limits.max_resource_bytes, limits.max_total_bytes.saturating_sub(retained));
    // Refuse a size that the server declares, before any buffer: a lie of 1 TB costs nothing.
    let declared = response.header("content-length").and_then(|v| v.trim_matches(WS).parse::<usize>().ok());
    if declared.is_some_and(|d| d > cap) {
        return Err(LoadError::TooLarge);
    }
    if declared.is_some_and(|d| d > room) {
        return Err(LoadError::TotalTooLarge);
    }
    // One byte more than the declared size, so that the read that sees the end of the body has room.
    let mut buf = vec![0u8; declared.map_or(16 * 1024, |d| d.saturating_add(1)).min(cap.saturating_add(1))];
    let mut filled = 0;
    loop {
        if Instant::now() >= deadline {
            return Err(LoadError::TimedOut);
        }
        if filled == buf.len() {
            buf.resize(buf.len().saturating_mul(2).min(cap.saturating_add(1)), 0);
        }
        let n = match streamed.body.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(LoadError::Truncated),
            Err(e) => return Err(LoadError::Read(e.kind())),
        };
        filled += n;
        if filled > cap {
            return Err(LoadError::TooLarge);
        }
        if filled > room {
            return Err(LoadError::TotalTooLarge);
        }
    }
    buf.truncate(filled);
    if buf.capacity() - filled > filled / 8 {
        // Chunked bodies grow by doubling. Give back the unused room: the cap is on the memory that we keep.
        buf.shrink_to_fit();
    }
    let text = decode(buf, content_type.and_then(charset));
    // Decoding can make the text longer than the bytes (a bad byte becomes 3 bytes). The caps are on the text.
    if text.len() > cap {
        return Err(LoadError::TooLarge);
    }
    if text.len() > room {
        return Err(LoadError::TotalTooLarge);
    }
    Ok(Resource { url: current, text })
}

/// UTF-8 is the default for scripts and styles. A UTF-8 body with no BOM becomes the `String` without a copy.
fn decode(bytes: Vec<u8>, charset: Option<&str>) -> String {
    let utf8 = charset.is_none_or(|c| c.eq_ignore_ascii_case("utf-8") || c.eq_ignore_ascii_case("utf8"));
    if !utf8 {
        return encoding::decode(&bytes, Some(&format!("text/plain; charset={}", charset.unwrap_or(""))));
    }
    let mut text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    if text.starts_with('\u{feff}') {
        text.drain(..3);
    }
    text
}

/// What `bare --resources` prints: one line for each item, then a summary. The second value is `false` when a
/// download failed. Only the host and the path (no query) of a URL are printed, and never a body.
pub fn report(doc: &Document, page: &Url) -> (String, bool) {
    use fmt::Write;
    let limits = Limits::default();
    let discovery = match discover(doc, page, &limits) {
        Ok(d) => d,
        Err(e) => return (format!("error: {e}\n"), false),
    };
    let loaded = load(&discovery.targets, &limits);
    let label = |i: usize| {
        let u = &discovery.targets[i].url;
        format!("{}://{}:{}{}", u.scheme, u.host, u.port, u.path.split('?').next().unwrap_or(""))
    };
    let result = |i: usize| match &loaded.results[i] {
        Ok(r) => format!("{} bytes", r.text.len()),
        Err(e) => format!("FAILED {e}"),
    };
    let mut out = String::new();
    for (n, item) in discovery.items.iter().enumerate() {
        let _ = match &item.kind {
            Kind::Script { module, timing, source } => {
                let what = if *module { "module" } else { "classic" };
                match source {
                    Source::Inline => writeln!(out, "{n} script {what} {timing:?} inline {} bytes", inline_source(doc, item.node).len()),
                    Source::External(i) => writeln!(out, "{n} script {what} {timing:?} {} {}", label(*i), result(*i)),
                }
            }
            Kind::Stylesheet { target } => writeln!(out, "{n} stylesheet {} {}", label(*target), result(*target)),
            Kind::Skipped(why) => writeln!(out, "{n} skipped {why:?}"),
        };
    }
    let failed = loaded.results.iter().filter(|r| r.is_err()).count();
    let _ = writeln!(out, "{} items, {} downloads, {failed} failed, {} bytes kept", discovery.items.len(), discovery.targets.len(), loaded.retained);
    (out, failed == 0)
}

#[cfg(test)]
#[path = "test_server.rs"]
pub(crate) mod test_server;

#[cfg(test)]
mod tests {
    use super::test_server::{Payload, Reply, Shape, serve};
    use super::*;
    use bare_html::tree::parse;

    fn page(port: u16) -> Url {
        Url::parse(&format!("http://127.0.0.1:{port}/dir/page.html")).unwrap()
    }

    fn found(html: &str, page: &Url) -> Discovery {
        discover(&parse(html), page, &Limits::default()).unwrap()
    }

    fn paths(d: &Discovery) -> Vec<&str> {
        d.targets.iter().map(|t| t.url.path.as_str()).collect()
    }

    fn limits(resource: usize, total: usize) -> Limits {
        Limits { max_resource_bytes: resource, max_total_bytes: total, ..Limits::default() }
    }

    // ---- discovery ----

    #[test]
    fn relative_and_absolute_urls() {
        let p = page(1);
        let d = found(r#"<script src="a.js"></script><script src="/b.js"></script><script src="../c.js"></script><script src="//other.test/d.js"></script><link rel=stylesheet href="s.css?v=1#x">"#, &p);
        assert_eq!(paths(&d), ["/dir/a.js", "/b.js", "/c.js", "/d.js", "/dir/s.css?v=1"]);
        assert_eq!(d.targets[3].url.host, "other.test");
    }

    #[test]
    fn first_valid_base_wins() {
        let p = page(1);
        let d = found(r#"<head><base href="ftp://bad/"><base href="/root/"><base href="/late/"><script src="a.js"></script><link rel=stylesheet href="s.css"></head>"#, &p);
        assert_eq!(paths(&d), ["/root/a.js", "/root/s.css"]);
        // A base after the script still applies to it.
        let d = found(r#"<script src="a.js"></script><base href="/after/">"#, &p);
        assert_eq!(paths(&d), ["/after/a.js"]);
        // A base with no href is no base.
        assert_eq!(paths(&found(r#"<base target=x><script src="a.js"></script>"#, &p)), ["/dir/a.js"]);
    }

    #[test]
    fn timing_and_module_metadata() {
        let p = page(1);
        let d = found(
            r#"<script src=a.js></script><script src=b.js async></script><script src=c.js defer></script><script src=d.js async defer></script>
               <script type=module src=e.js></script><script type=MODULE src=f.js async></script><script type=module src=g.js defer></script>
               <script>1</script><script async defer>2</script><script type=module>3</script><script type=module async>4</script>
               <script type="Text/JavaScript" src=h.js></script><script type="" src=i.js></script>"#,
            &p,
        );
        let got: Vec<(bool, Timing, bool)> = d
            .items
            .iter()
            .map(|i| match i.kind {
                Kind::Script { module, timing, source } => (module, timing, matches!(source, Source::External(_))),
                ref k => panic!("{k:?}"),
            })
            .collect();
        use Timing::*;
        assert_eq!(
            got,
            [
                (false, Blocking, true),
                (false, Async, true),
                (false, Defer, true),
                (false, Async, true),
                (true, Defer, true),
                (true, Async, true),
                (true, Defer, true),
                (false, Blocking, false),
                (false, Blocking, false),
                (true, Defer, false),
                (true, Async, false),
                (false, Blocking, true),
                (false, Blocking, true),
            ]
        );
    }

    #[test]
    fn inline_scripts_are_not_copied() {
        let doc = parse("<script>let a = 1;</script><script src=x.js>ignored</script><script></script>");
        let d = discover(&doc, &page(1), &Limits::default()).unwrap();
        assert_eq!(d.items.len(), 3);
        let text = inline_source(&doc, d.items[0].node);
        assert!(matches!(text, Cow::Borrowed("let a = 1;")));
        // The source of the text is the buffer of the DOM: same address, no copy.
        assert!(std::ptr::eq(doc.text(match *doc.data(doc.children(d.items[0].node).next().unwrap()) {
            NodeData::Text(s) => s,
            _ => unreachable!(),
        }), &*text));
        assert!(matches!(inline_source(&doc, d.items[2].node), Cow::Borrowed("")));
        // With a `src`, the inline text is not the source: the item is external.
        assert!(matches!(d.items[1].kind, Kind::Script { source: Source::External(0), .. }));
    }

    #[test]
    fn duplicates_are_one_fetch_but_stay_in_the_items() {
        let p = page(1);
        let d = found(r#"<script src=a.js async></script><script src=./a.js defer></script><link rel=stylesheet href=a.js><script src="a.js#frag"></script><script src=b.js></script>"#, &p);
        assert_eq!(d.items.len(), 5);
        assert_eq!(d.targets.len(), 3);
        // The same URL as a script and as a style sheet needs two checks of the type, so two targets.
        assert_eq!((d.targets[0].expect, d.targets[1].expect), (Expect::Script, Expect::Style));
        let ext = |i: usize| match d.items[i].kind {
            Kind::Script { source: Source::External(t), timing, .. } => (t, timing),
            _ => panic!(),
        };
        assert_eq!((ext(0), ext(1), ext(3)), ((0, Timing::Async), (0, Timing::Defer), (0, Timing::Blocking)));
        assert_eq!(ext(4).0, 2);
    }

    #[test]
    fn non_code_types_and_other_skips_are_explicit() {
        let p = page(1);
        let d = found(
            r#"<script type="application/json">{}</script><script type="importmap">{}</script><script type="text/template" src=t.js></script>
               <script type="text/javascript; charset=utf-8" src=p.js></script>
               <script nomodule src=n.js></script><script type=module nomodule src=m.js></script>
               <script src=""></script><script src="  "></script><script src="data:text/javascript,1"></script><script src="ftp://h/x.js"></script>
               <link rel="stylesheet alternate" href=alt.css><link rel=preload href=p.css><link rel=icon href=i.png><link rel=stylesheet><link rel=STYLESHEET href=ok.css>
               <template><script src=in-template.js></script></template>"#,
            &p,
        );
        let kinds: Vec<_> = d.items.iter().map(|i| i.kind.clone()).collect();
        assert!(kinds[..4].iter().all(|k| *k == Kind::Skipped(Skip::UnsupportedType)));
        assert_eq!(kinds[4], Kind::Skipped(Skip::NoModule));
        assert!(matches!(kinds[5], Kind::Script { module: true, .. }));
        assert!(kinds[6..8].iter().all(|k| *k == Kind::Skipped(Skip::EmptyUrl)));
        assert!(kinds[8..10].iter().all(|k| *k == Kind::Skipped(Skip::BadUrl)));
        assert_eq!(kinds[10], Kind::Skipped(Skip::EmptyUrl)); // link with no href
        assert_eq!(kinds.len(), 12);
        assert_eq!(paths(&d), ["/dir/m.js", "/dir/ok.css"]);
    }

    #[test]
    fn reference_and_fetch_caps_are_exact() {
        let p = page(1);
        let many = |n: usize, distinct: usize| (0..n).map(|i| format!("<script src=s{}.js></script>", i % distinct)).collect::<String>();
        let l = Limits { max_references: 10, max_fetches: 4, ..Limits::default() };
        assert!(discover(&parse(&many(10, 4)), &p, &l).is_ok());
        assert_eq!(discover(&parse(&many(11, 4)), &p, &l).unwrap_err(), DiscoverError::TooManyReferences(10));
        assert_eq!(discover(&parse(&many(10, 5)), &p, &l).unwrap_err(), DiscoverError::TooManyFetches(4));
        // Inline scripts count as references too.
        assert!(discover(&parse(&"<script>1</script>".repeat(11)), &p, &l).is_err());
    }

    /// The saved staging login page: the references that the HTML names. Runtime chunks are not here (see the test below).
    #[test]
    fn staging_login_fixture_discovery() {
        let html = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/staging/login.html")).unwrap();
        let p = Url::parse("https://staging.app.8vox.ai/login").unwrap();
        let d = discover(&parse(&html), &p, &Limits::default()).unwrap();
        let scripts = d.items.iter().filter(|i| matches!(i.kind, Kind::Script { .. })).count();
        let inline = d.items.iter().filter(|i| matches!(i.kind, Kind::Script { source: Source::Inline, .. })).count();
        let nomodule = d.items.iter().filter(|i| i.kind == Kind::Skipped(Skip::NoModule)).count();
        let styles = d.items.iter().filter(|i| matches!(i.kind, Kind::Stylesheet { .. })).count();
        let asyncs = d.items.iter().filter(|i| matches!(i.kind, Kind::Script { timing: Timing::Async, .. })).count();
        // staging-network.md: 16 scripts with src (15 async + 1 nomodule polyfill) and 6 inline scripts, 2 style sheets.
        assert_eq!((scripts, inline, nomodule, styles, asyncs), (21, 6, 1, 2, 15));
        assert_eq!(d.items.len(), 24);
        assert_eq!(d.targets.len(), 15 + 2);
        assert!(d.targets.iter().all(|t| t.url.host == "staging.app.8vox.ai" && t.url.scheme == bare_url::Scheme::Https));
        assert!(d.targets.iter().any(|t| t.url.path == "/_next/static/chunks/webpack-4af263207fca128f.js"));
        assert!(d.targets.iter().any(|t| t.url.path == "/_next/static/css/f550c1958bfdbf7f.css"));
        // The preloaded webpack runtime is also a script element: it is one target, not two.
        assert_eq!(d.targets.iter().filter(|t| t.url.path.contains("webpack-")).count(), 1);
    }

    // ---- loading ----

    fn load_html(server: &super::test_server::Server, html: &str, l: &Limits) -> (Document, Discovery, Loaded) {
        let doc = parse(html);
        let d = discover(&doc, &page(server.port), l).unwrap();
        let loaded = load(&d.targets, l);
        (doc, d, loaded)
    }

    #[test]
    fn loads_scripts_and_styles_once_each() {
        let server = serve(|path| match path {
            "/dir/a.js" => Reply::js("var a=1;"),
            "/dir/s.css" => Reply::css("p{}"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let html = "<link rel=stylesheet href=s.css><script src=a.js async></script><script src=a.js defer></script><script>inline</script><script src=a.js></script>";
        let (_, d, loaded) = load_html(&server, html, &Limits::default());
        assert_eq!((d.items.len(), d.targets.len()), (5, 2));
        assert_eq!(loaded.results[0].as_ref().unwrap().text, "p{}");
        assert_eq!(loaded.results[1].as_ref().unwrap().text, "var a=1;");
        assert_eq!(loaded.retained, 3 + 8);
        assert_eq!((server.hits("/dir/a.js"), server.hits("/dir/s.css")), (1, 1));
        // We never ask for compression, and never send a body or a cookie.
        let heads = server.heads.lock().unwrap();
        assert!(heads.iter().all(|h| !h.contains("accept-encoding") && !h.contains("cookie") && !h.contains("authorization")));
    }

    #[test]
    fn base_href_decides_the_request_path() {
        let server = serve(|path| if path == "/assets/a.js" { Reply::js("ok") } else { Reply::new(404, Some("text/plain"), "") });
        let (_, _, loaded) = load_html(&server, "<base href=/assets/><script src=a.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap().text, "ok");
    }

    #[test]
    fn bad_status_is_an_error_even_with_good_looking_body() {
        // The staging chunk 3299 case: 404, HTML, "no-store".
        let server = serve(|path| match path {
            "/dir/a.js" => Reply::new(404, Some("text/html; charset=utf-8"), "<html>not found</html>").header("Cache-Control", "no-store"),
            "/dir/b.js" => Reply::new(200, Some("application/javascript"), "x").shaped(Shape::Length),
            "/dir/c.js" => Reply::new(500, Some("application/javascript"), "var a"),
            "/dir/d.js" => Reply::new(204, Some("application/javascript"), ""),
            _ => Reply::new(301, None, ""),
        });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script><script src=c.js></script><script src=d.js></script><script src=e.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap_err(), &LoadError::Status(404));
        assert!(loaded.results[1].is_ok());
        assert_eq!(loaded.results[2].as_ref().unwrap_err(), &LoadError::Status(500));
        assert_eq!(loaded.results[3].as_ref().unwrap().text, ""); // 2xx is accepted: an empty script is a script
        assert_eq!(loaded.results[4].as_ref().unwrap_err(), &LoadError::Status(301)); // a redirect with no Location
        assert_eq!(loaded.retained, 1);
    }

    #[test]
    fn mime_is_checked_with_parameters_and_case() {
        let types = [
            ("application/javascript; charset=UTF-8", true),
            ("APPLICATION/JavaScript", true),
            ("text/javascript ; charset=\"utf-8\"", true),
            ("text/ecmascript", true),
            ("text/html; charset=utf-8", false),
            ("text/css", false),
            ("application/json", false),
            ("application/javascript-evil", false),
            ("text/javascript;", true),
        ];
        let server = serve(move |path| {
            let n: usize = path[1..].parse().unwrap();
            if n == types.len() { Reply::new(200, None, "no type") } else { Reply::new(200, Some(types[n].0), "x") }
        });
        let html: String = (0..=types.len()).map(|n| format!("<script src=/{n}></script>")).collect();
        let (_, _, loaded) = load_html(&server, &html, &Limits::default());
        for (n, (t, ok)) in types.iter().enumerate() {
            assert_eq!(loaded.results[n].is_ok(), *ok, "{t}: {:?}", loaded.results[n]);
        }
        assert_eq!(loaded.results[types.len()].as_ref().unwrap_err(), &LoadError::Mime(None));
        assert_eq!(loaded.results[4].as_ref().unwrap_err(), &LoadError::Mime(Some("text/html".into())));
    }

    #[test]
    fn stylesheets_need_text_css() {
        let server = serve(|path| match path {
            "/dir/a.css" => Reply::new(200, Some("Text/CSS;charset=utf-8"), "a{}"),
            "/dir/b.css" => Reply::new(200, Some("application/javascript"), "a{}"),
            "/dir/c.css" => Reply::new(200, Some("text/html"), "<html>"),
            _ => Reply::new(200, None, "a{}"),
        });
        let (_, _, loaded) = load_html(&server, "<link rel=stylesheet href=a.css><link rel=stylesheet href=b.css><link rel=stylesheet href=c.css><link rel=stylesheet href=d.css>", &Limits::default());
        assert!(loaded.results[0].is_ok());
        assert!(loaded.results[1..].iter().all(|r| matches!(r, Err(LoadError::Mime(_)))));
    }

    #[test]
    fn compressed_bodies_are_errors() {
        let server = serve(|path| match path {
            "/dir/a.js" => Reply::js(b"\x1f\x8b gz".to_vec()).header("Content-Encoding", "gzip"),
            "/dir/b.js" => Reply::js("x").header("Content-Encoding", "Identity"),
            "/dir/c.js" => Reply::js("x").header("Content-Encoding", "br"),
            _ => Reply::js("x").header("Content-Encoding", "deflate, gzip"),
        });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script><script src=c.js></script><script src=d.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap_err(), &LoadError::Encoding("gzip".into()));
        assert!(loaded.results[1].is_ok());
        assert_eq!(loaded.results[2].as_ref().unwrap_err(), &LoadError::Encoding("br".into()));
        assert!(matches!(loaded.results[3], Err(LoadError::Encoding(_))));
    }

    #[test]
    fn per_resource_cap_is_exact_for_every_framing() {
        let cap = 1000;
        let shapes = |n: usize| match n % 3 {
            0 => Shape::Length,
            1 => Shape::Chunked(77),
            _ => Shape::Short(10),
        };
        let server = serve(move |path| {
            let n: usize = path[1..].parse().unwrap();
            let size = if n < 3 { cap } else { cap + 1 };
            Reply::js(vec![b'x'; size]).shaped(shapes(n))
        });
        let html: String = (0..6).map(|n| format!("<script src=/{n}></script>")).collect();
        let (_, _, loaded) = load_html(&server, &html, &limits(cap, 1 << 20));
        for n in 0..3 {
            assert_eq!(loaded.results[n].as_ref().unwrap().text.len(), cap, "exact cap, shape {n}");
        }
        for n in 3..6 {
            assert_eq!(loaded.results[n].as_ref().unwrap_err(), &LoadError::TooLarge, "cap + 1, shape {n}");
        }
        assert_eq!(loaded.retained, 3 * cap);
    }

    #[test]
    fn huge_declared_length_is_refused_before_any_body_is_read() {
        let server = serve(|_| Reply::js("small").shaped(Shape::Declared(100 << 20)));
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap_err(), &LoadError::TooLarge);
        // The HTTP client refuses a length over its own 128 MB limit first. It is still an error, and no buffer is made.
        let server = serve(|_| Reply::js("small").shaped(Shape::Declared(1 << 40)));
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script>", &Limits::default());
        assert!(loaded.results[0].is_err());
        // Under the cap but over the room that is left for all resources.
        let server = serve(|_| Reply::js("small").shaped(Shape::Declared(900)));
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script>", &limits(1000, 800));
        assert_eq!(loaded.results[0].as_ref().unwrap_err(), &LoadError::TotalTooLarge);
    }

    #[test]
    fn aggregate_cap_is_exact() {
        let server = serve(|path| Reply::js(vec![b'x'; if path == "/dir/c.js" { 1 } else { 400 }]));
        // 400 + 400 = 800 is exactly the cap. The third (1 byte) is over.
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script><script src=c.js></script>", &limits(500, 800));
        assert!(loaded.results[0].is_ok() && loaded.results[1].is_ok());
        assert_eq!(loaded.results[2].as_ref().unwrap_err(), &LoadError::TotalTooLarge);
        assert_eq!(loaded.retained, 800);
        // The aggregate also holds for a body of unknown length (chunked): no declared size to refuse early.
        let server = serve(|_| Reply::js(vec![b'x'; 400]).shaped(Shape::Chunked(50)));
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script><script src=c.js></script>", &limits(500, 800));
        assert!(loaded.results[0].is_ok() && loaded.results[1].is_ok());
        assert_eq!(loaded.results[2].as_ref().unwrap_err(), &LoadError::TotalTooLarge);
    }

    #[test]
    fn truncated_body_is_an_error_not_a_short_script() {
        let server = serve(|path| match path {
            "/dir/a.js" => Reply::js("var a = 'cut';").shaped(Shape::Truncated(10)),
            _ => Reply::js("ok"),
        });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap_err(), &LoadError::Truncated);
        assert!(loaded.results[1].is_ok(), "one failure does not stop the next resource");
        assert_eq!(loaded.retained, 2);
    }

    #[test]
    fn short_reads_give_the_same_bytes() {
        let body: Vec<u8> = (0..5000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let b = body.clone();
        let server = serve(move |path| if path == "/dir/a.js" { Reply::js(b.clone()).shaped(Shape::Short(1)) } else { Reply::js(b.clone()).shaped(Shape::Chunked(3)) });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script>", &Limits::default());
        for r in &loaded.results {
            assert_eq!(r.as_ref().unwrap().text.as_bytes(), &body[..]);
        }
    }

    #[test]
    fn time_limit_stops_further_requests() {
        let server = serve(|_| Reply::js("x"));
        let l = Limits { max_time: Duration::ZERO, ..Limits::default() };
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script>", &l);
        assert!(loaded.results.iter().all(|r| r.as_ref().unwrap_err() == &LoadError::TimedOut));
        assert_eq!(server.total(), 0, "no request after the deadline");
    }

    #[test]
    fn charset_bom_and_bad_bytes() {
        let server = serve(|path| match path {
            "/dir/a.js" => Reply::js("\u{feff}var é;"),
            "/dir/b.js" => Reply::new(200, Some("text/javascript; charset=iso-8859-1"), vec![b'"', 0xe9, b'"']),
            "/dir/c.js" => Reply::js(vec![b'a', 0xff, b'b']),
            _ => Reply::new(200, Some("text/javascript"), "x"),
        });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script><script src=b.js></script><script src=c.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap().text, "var é;");
        assert_eq!(loaded.results[1].as_ref().unwrap().text, "\"é\"");
        assert_eq!(loaded.results[2].as_ref().unwrap().text, "a\u{fffd}b");
    }

    #[test]
    fn https_network_failure_is_reported() {
        // An unavailable HTTPS endpoint must surface a download error. This does not establish socket behavior.
        let p = Url::parse("https://127.0.0.1:1/login").unwrap();
        let doc = parse("<script src=/a.js></script>");
        let d = discover(&doc, &p, &Limits::default()).unwrap();
        let loaded = load(&d.targets, &Limits::default());
        assert!(matches!(loaded.results[0], Err(LoadError::Network(_))));
    }

    #[test]
    fn errors_name_no_url_and_no_body() {
        let secret = "token=SECRET123";
        let server = serve(|_| Reply::new(404, Some("text/html"), "<html>BODYTEXT</html>"));
        let doc = parse(&format!("<script src='a.js?{secret}'></script>"));
        let d = discover(&doc, &page(server.port), &Limits::default()).unwrap();
        let loaded = load(&d.targets, &Limits::default());
        let text = format!("{}", loaded.results[0].as_ref().unwrap_err());
        assert!(!text.contains("SECRET") && !text.contains("BODYTEXT"));
        let (out, ok) = report(&doc, &page(server.port));
        assert!(!ok && !out.contains("SECRET") && !out.contains("BODYTEXT"), "{out}");
        assert!(out.contains("FAILED status 404"));
    }

    #[test]
    fn report_lists_items_in_page_order() {
        let server = serve(|path| if path.ends_with(".css") { Reply::css("p{}") } else { Reply::js("var a;") });
        let doc = parse("<link rel=stylesheet href=s.css><script src=a.js async></script><script>inline!</script><script type=module src=m.js></script><script type=text/x-t>t</script>");
        let (out, ok) = report(&doc, &page(server.port));
        assert!(ok, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("0 stylesheet ") && lines[0].ends_with("/dir/s.css 3 bytes"));
        assert!(lines[1].starts_with("1 script classic Async ") && lines[1].ends_with("/dir/a.js 6 bytes"));
        assert_eq!(lines[2], "2 script classic Blocking inline 7 bytes");
        assert!(lines[3].starts_with("3 script module Defer "));
        assert_eq!(lines[4], "4 skipped UnsupportedType");
        assert_eq!(lines[5], "5 items, 3 downloads, 0 failed, 15 bytes kept");
    }

    #[test]
    fn filler_payload_is_not_all_in_memory_on_the_server() {
        // Guards the harness server: a Filler of 3 MB arrives as 3 MB.
        let server = serve(|_| Reply { status: 200, headers: vec![("Content-Type".into(), "text/javascript".into())], body: Payload::Filler(3_000_000), shape: Shape::Length });
        let (_, _, loaded) = load_html(&server, "<script src=a.js></script>", &Limits::default());
        assert_eq!(loaded.results[0].as_ref().unwrap().text.len(), 3_000_000);
    }
    #[test]
    fn maximum_resource_caps_do_not_overflow() {
        let server = serve(|_| Reply::js("x"));
        let target = Target { expect: Expect::Script, url: Url::parse(&server.url("/x")).unwrap() };
        let limits = Limits { max_resource_bytes: usize::MAX, max_total_bytes: usize::MAX, ..Limits::default() };
        let got = fetch(&mut http::Client::new(), &target, &limits, Instant::now() + Duration::from_secs(2), 0).unwrap();
        assert_eq!(got.text, "x");
    }

    #[test]
    fn resource_redirects_are_checked_before_following() {
        let from = Url::parse("https://secure.test/start").unwrap();
        assert!(matches!(redirect_target(&from, "http://plain.test/a"), Err(LoadError::Redirect)));
        assert!(matches!(redirect_target(&from, "ftp://other.test/a"), Err(LoadError::Redirect)));
        assert_eq!(redirect_target(&from, "/next").unwrap().scheme, bare_url::Scheme::Https);
        let server = serve(|p| if p == "/a" { Reply::new(302, None, "").header("Location", "/b") } else { Reply::js("x") });
        let target = Target { expect: Expect::Script, url: Url::parse(&server.url("/a")).unwrap() };
        let got = fetch(&mut http::Client::new(), &target, &Limits::default(), Instant::now() + Duration::from_secs(2), 0).unwrap();
        assert_eq!(got.text, "x");
        assert_eq!((server.hits("/a"),server.hits("/b")),(1,1));
    }

}

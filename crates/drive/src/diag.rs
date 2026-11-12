//! What went wrong on a page, for an agent: a bounded history of the console and the network, and a report that names the likely cause.
//! The history lives as long as the session. The step reports (`Session::drain`) hand out the same entries once and forget them.

pub use bare::control::Log;
use bare_net::trace::Rec;
use std::collections::VecDeque;

const KEEP: usize = 500;

/// The last 500 console entries and 500 network records. Each entry has a number; a reader keeps the last number that it saw.
#[derive(Default)]
pub struct History {
    logs: VecDeque<(u64, Log)>,
    net: VecDeque<(u64, Rec)>,
    n: u64,
    seen_logs: u64,
    seen_net: u64,
}

impl History {
    pub fn feed(&mut self, logs: impl IntoIterator<Item = Log>, net: impl IntoIterator<Item = Rec>) {
        for l in logs {
            self.n += 1;
            self.logs.push_back((self.n, l));
        }
        for r in net {
            self.n += 1;
            self.net.push_back((self.n, r));
        }
        while self.logs.len() > KEEP {
            self.logs.pop_front();
        }
        while self.net.len() > KEEP {
            self.net.pop_front();
        }
    }

    /// The console entries (not older than the last call when `since_last`), at most `max`, newest last. `level` is `error`, `warn` (and
    /// errors) or `all`.
    pub fn console(&mut self, level: &str, since_last: bool, max: usize) -> Vec<String> {
        let from = if since_last { self.seen_logs } else { 0 };
        self.seen_logs = self.n;
        let mut out: Vec<String> = self.logs.iter().filter(|(i, l)| *i > from && wanted(level, l.level)).map(|(_, l)| format!("{} {}", l.level, short(&l.text, 300))).collect();
        cut(&mut out, max);
        out
    }

    /// The network records (not older than the last call when `since_last`), at most `max`.
    pub fn network(&mut self, failed_only: bool, contains: Option<&str>, since_last: bool, max: usize) -> Vec<String> {
        let from = if since_last { self.seen_net } else { 0 };
        self.seen_net = self.n;
        let mut out: Vec<String> = self.net.iter().filter(|(i, r)| *i > from && (!failed_only || failed(r)) && contains.is_none_or(|c| r.url.contains(c))).map(|(_, r)| line(r)).collect();
        cut(&mut out, max);
        out
    }

    /// The number of the newest entry. `logs_since` gives the entries after it.
    pub fn mark(&self) -> u64 {
        self.n
    }

    /// The console entries after `mark`.
    pub fn logs_since(&self, mark: u64) -> impl Iterator<Item = &Log> {
        self.logs.iter().filter(move |(i, _)| *i > mark).map(|(_, l)| l)
    }

    pub fn logs(&self) -> impl Iterator<Item = &Log> {
        self.logs.iter().map(|(_, l)| l)
    }

    pub fn recs(&self) -> impl DoubleEndedIterator<Item = &Rec> {
        self.net.iter().map(|(_, r)| r)
    }
}

fn wanted(want: &str, level: &str) -> bool {
    match want {
        "all" => true,
        "warn" => matches!(level, "error" | "warn" | "exception" | "page"),
        _ => matches!(level, "error" | "exception" | "page"),
    }
}

fn cut(v: &mut Vec<String>, max: usize) {
    if v.len() > max {
        v.drain(..v.len() - max);
    }
}

fn short(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n { s } else { s.chars().take(n).collect::<String>() + "…" }
}

pub fn failed(r: &Rec) -> bool {
    r.error.is_some() || r.status >= 400
}

fn line(r: &Rec) -> String {
    let what = match (&r.error, r.status) {
        (Some(e), _) => short(e, 120),
        (None, s) => s.to_string(),
    };
    format!("{what} {} {} {}B {:.0}ms", kind(&r.url), crate::run::safe_url(&r.url), r.bytes, (r.ttfb + r.body) as f64 / 1000.0)
}

/// The kind of a request, from its URL: a record does not keep the kind of its caller.
fn kind(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    match path.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "script",
        "css" => "stylesheet",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "avif" => "image",
        "woff" | "woff2" | "ttf" | "otf" => "font",
        _ => "fetch",
    }
}

/// One thing that the page waits for: `fetch GET https://x/api (19.4 s)`, or the kind of work that is not a request. Then its state
/// (`Waiting::state`): `not sent` or `reply in` say that it waits for the page thread, not for the network.
pub fn waiting(w: &bare::page::Waiting) -> String {
    if w.url.is_empty() {
        return w.kind.to_string();
    }
    format!("{} {} {} ({:.1} s{})", w.kind, w.method, crate::run::safe_url(&w.url), w.age.as_secs_f64(), if w.state.is_empty() { String::new() } else { format!(", {}", w.state) })
}

/// What `Session::diagnose` read from the page.
pub struct Facts {
    pub url: String,
    pub load_error: Option<String>,
    pub stopped: Option<String>,
    pub nodes: usize,
    /// Snapshot lines of a walk that shows text. 0: nothing shows.
    pub shown: usize,
    pub cpu_ms: f64,
    /// What the page waits for (`Page::pending`).
    pub pending: Vec<bare::page::Waiting>,
}

fn base(u: &str) -> &str {
    u.split('#').next().unwrap_or(u)
}

/// The report: one line for each finding, then the verdict. `clean` removes the secrets.
pub fn report(h: &History, f: &Facts, clean: &dyn Fn(&str) -> String) -> String {
    let mut out: Vec<String> = Vec::new();
    // The document: the last request for the URL of the page.
    let doc = if f.url.is_empty() { h.recs().next() } else { h.recs().rfind(|r| base(&r.url) == base(&f.url)) };
    let mut load = format!("document: {}", crate::run::safe_url(if f.url.is_empty() { doc.map_or("", |r| &r.url) } else { &f.url }));
    if let Some(r) = doc {
        load += &match &r.error {
            Some(e) => format!(", failed: {}", short(e, 120)),
            None => format!(", HTTP {}", r.status),
        };
    }
    if let Some(e) = &f.load_error {
        load += &format!(", load error: {}", short(e, 160));
    }
    if let Some(e) = &f.stopped {
        load += &format!(", the page stopped: {}", short(e, 160));
    }
    out.push(load);
    let (mut thrown, mut gaps) = (Vec::new(), Vec::new());
    for l in h.logs() {
        let t = short(&l.text, 240);
        if uncaught(l) && !thrown.contains(&t) && f.load_error.as_deref() != Some(t.as_str()) {
            thrown.push(t.clone());
        }
        if matches!(l.level, "exception" | "page" | "error" | "warn") && gap(&t) && !gaps.contains(&t) {
            gaps.push(t);
        }
    }
    for t in thrown.iter().take(5) {
        out.push(format!("uncaught: {t}"));
    }
    for t in gaps.iter().take(5) {
        out.push(format!("missing feature: {t}"));
    }
    let bad: Vec<&Rec> = h.recs().filter(|r| failed(r) && base(&r.url) != base(&f.url)).collect();
    for k in ["script", "stylesheet", "image", "font", "fetch"] {
        let of: Vec<&&Rec> = bad.iter().filter(|r| kind(&r.url) == k).collect();
        if !of.is_empty() {
            let first = of[0];
            out.push(format!("failed {k}: {} ({} in all, first {})", crate::run::safe_url(&first.url), of.len(), first.error.clone().unwrap_or_else(|| first.status.to_string())));
        }
    }
    // A request that has not ended after 5 s: it can hold the page for good (a long poll, a reply that never comes).
    for w in f.pending.iter().filter(|w| w.age.as_secs() >= 5).take(3) {
        out.push(format!("pending {}{}", waiting(w), if w.holds { ", holds the page" } else { "" }));
    }
    let empty = f.shown == 0;
    if empty {
        out.push("render: no visible text and no control".into());
    }
    out.push(format!("size: {} DOM nodes, page thread CPU {:.0} ms", f.nodes, f.cpu_ms));
    let net_only = !bad.is_empty() && bad.iter().all(|r| r.error.is_some());
    let verdict = if let Some(n) = f.load_error.as_deref().and_then(|e| e.strip_prefix("status ")) {
        format!("site problem: the document answered HTTP {n}")
    } else if f.load_error.is_some() || doc.is_some_and(|r| r.error.is_some()) {
        "network: the document did not load".to_string()
    } else if f.stopped.is_some() {
        "bare gap: the page stopped (a fatal error or a script budget)".to_string()
    } else if let Some(g) = gaps.first() {
        format!("bare gap: {g}")
    } else if let Some(t) = thrown.first() {
        format!("site problem: the script threw: {t}")
    } else if doc.is_some_and(|r| r.status >= 400) {
        format!("site problem: the document answered HTTP {}", doc.map_or(0, |r| r.status))
    } else if net_only && empty {
        "network: subresources failed and the page shows nothing".to_string()
    } else if let Some(r) = bad.iter().find(|r| r.status >= 400) {
        format!("site problem: {} answered HTTP {}", crate::run::safe_url(&r.url), r.status)
    } else if net_only {
        "network: a request failed".to_string()
    } else if empty {
        "unclear: the page shows nothing and no error was logged".to_string()
    } else {
        "unclear: no failure seen".to_string()
    };
    out.push(format!("verdict: {verdict}"));
    clean(&out.join("\n"))
}

/// Is the entry an uncaught exception or a page error?
pub fn uncaught(l: &Log) -> bool {
    matches!(l.level, "exception" | "page") || (l.level == "error" && l.text.contains("Uncaught"))
}

/// Does the text say that the engine lacks an API or a feature?
fn gap(t: &str) -> bool {
    let t = t.to_ascii_lowercase();
    ["is not a function", "is not defined", "not supported", "unsupported", "not implemented", "is not a constructor"].iter().any(|k| t.contains(k))
}

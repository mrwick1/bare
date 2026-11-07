//! Style sheet loading for a page session: the driver fetches the active style sheets of the document and supplies them to
//! `getComputedStyle` (`Browser::set_external_stylesheet`). Child of `browser_loader`: it is part of `Run`.
//!
//! When it runs (a driver boundary, never inside `getComputedStyle`, a getter or user JS)
//! - Before the first blocking script (`go_inner`), and in `resume` before the inputs.
//! - Before each parsed script, before each dynamic script and at the start of each round, if a task ran since the last
//!   pass (`feed_if_ran`). So a link that a script inserted, or whose `href`/`media`/`rel` it changed, or a `<base>` it changed,
//!   is loaded before the next script or timer. The engine does no network call in a script.
//!
//! A read before the pass: pending sheets (as Chrome shows, `docs/resources.md#pending-style-sheets`)
//! - The first pass that ends `Ready` has every sheet of the parsed document. The driver then calls `Browser::set_pending_sheets(true)`.
//!   Chrome blocks a script on a parser-inserted sheet, and a pre-parsed document has them all before the first script, so that case needs
//!   no rule. Before that call a missing sheet still stops the page (`Fatal::Unsupported`).
//! - After it, a sheet that a script added (a `<link>`, an `@import` of a new `<style>`) and that is not supplied yet is "loading": it adds
//!   no rules, and a read in the same task, in a microtask or in a timer before the next pass sees the page without it. The `load` of
//!   the link runs after the supply, so a read in it sees the sheet.
//! - A link whose `href` changed keeps the sheet that it had until the new one is supplied. `style_availability` binds each link to
//!   its sheet at each pass for this. If that sheet is gone, or the link has a `media` text (it may have been off and just turned on, and
//!   Chrome had loaded it), the read still stops the page: bare does not fetch a sheet that its `media` turns off.
//! - A style preload (`preload.rs`) that loaded supplies its sheet under its `href` text before its `load` runs. A
//!   `<link rel=stylesheet>` for it, or the same link with `rel` changed, has the sheet at once, as in Chrome (the memory cache). The key is
//!   the exact `href` text: the same URL in another text is a new sheet that loads at the next pass.
//! - A pass asks the document (`Browser::style_availability`) which sheets it needs and the driver does not have: this reads
//!   the current DOM, so a node that was removed is never fetched, and a `media` that does not match is not fetched.
//!
//! What a pass does with a missing key (the exact `href` text of a link, or of an `@import` of a `<style>`)
//! - Resolves it against the document base (first good `<base href>`, else the page URL: `resources::document_base`).
//! - Fetches it with `resources::fetch` (the verified pipeline): 2xx, `text/css`, no content encoding, the caps of the
//!   page limits (one body at most 4 MiB, all text `max_total_bytes`, `max_fetches`, the page deadline), every redirect checked
//!   (http(s) only, never https to http). A page on https never asks for an `http:` sheet (mixed content): it fails the sheet.
//! - Reads its `@import`s (`cascade::imports`) and resolves each against the URL of the sheet that holds it. Imports come first,
//!   depth first, in the same layers as `main.rs` (`join_layer`). The flat parts go to the computed module under the raw key.
//! - A URL is fetched once per document: the text and the failures are cached by resolved URL. Each reference still gets its place
//!   in the cascade (parts are made for each key and each `@import`). A base change drops the keys whose URL changed.
//! - A sheet that genuinely failed (network, status, type, encoding, cut short, redirect, mixed content) is supplied as no
//!   rules, counted in `CssStats::failed`, named in a diagnostic (`style sheet failed: scheme://host:port/path: reason`) and the
//!   `<link>` gets an `error` event. A good one gets `load`. A good empty sheet is `CssStats::blank`, never a failure.
//!   An `@import` that failed adds no part. A cycle is cut (and counted), as the CSS standard says.
//! - Nothing that is a budget is an empty sheet: a time limit is `Stop::TimeBudget`, and a size, fetch, depth, parts or
//!   references cap is `Fatal::Limit("css-...")`. Both end the run for good: no JS and no I/O after them (the sticky fatal).
//!
//! Not done: `<link disabled>` (Chrome loads nothing and adds no rules; bare fetches and applies it), `crossorigin`, `integrity`, `Link:` headers, `@import supports()`, a changed server text is not
//! fetched again in the same document (the cache is by URL), a sheet that a script reads before the first pass ends `Ready` (it stops the page).

use super::{EvTarget, Fatal, Flow, Run, Stop, WS, fatal_of, label};
use crate::browser_computed::{SheetPart, StyleAvailability, join_layer};
use crate::resources::{self, Expect, LoadError};
use bare_css::cascade;
use bare_html::dom::{NodeData, NodeId};
use bare_url::{Scheme, Url};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// `@import` goes this deep (`main.rs` too). A deeper import is `Fatal::Limit("css-import-depth")`, not a silent cut.
const MAX_IMPORT_DEPTH: usize = 8;
/// `@import` rules that one supplied sheet reads, and the flat parts of it. Checked before the thing grows.
const MAX_IMPORT_REFS: usize = 256;
const MAX_PARTS: usize = 256;
/// One body, and the flat text of one supplied sheet. The computed module takes 4 MiB for a sheet and its key.
pub(super) const MAX_BODY: usize = 4 << 20;
const MAX_FLAT: usize = MAX_BODY - (64 << 10);
/// Passes of one boundary. Each supplies at least one new key, so this is a bound on keys, not a loop guard against a bug.
const MAX_PASSES: usize = 64;

/// What the driver did for style sheets in this document (all runs and resumes). `fetches` and `bytes` are also counted in
/// `PageResult::fetches` and `bytes`: one budget. A failed sheet and a good empty one are different counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CssStats {
    /// Requests sent for style sheets (a failure counts, a cache hit does not).
    pub fetches: usize,
    /// Bytes of decoded sheet text kept.
    pub bytes: usize,
    /// Sheets that came back good (blank ones too).
    pub loaded: usize,
    /// Of `loaded`: no rules, only space and comments. This is a real sheet, not a failure.
    pub blank: usize,
    /// Sheets that genuinely failed (supplied as no rules).
    pub failed: usize,
    /// `@import` cycles that were cut.
    pub cycles: usize,
    /// Keys supplied to `getComputedStyle` (a failed sheet is one too: no rules).
    pub supplied: usize,
    /// Boundary passes that read the document.
    pub feeds: usize,
}

type Fetched = Result<(Rc<Url>, Rc<String>), String>;

#[derive(Default)]
pub(crate) struct Sheets {
    pub(super) stats: CssStats,
    /// Resolved URL to the text, or why it failed. A failure is not tried again.
    fetched: HashMap<String, Fetched>,
    /// Bytes of text in `fetched`: part of the total cap, with the scripts that are kept.
    pub(super) retained: usize,
    /// Raw key to the URL that it was supplied for.
    supplied: HashMap<String, String>,
    base: Option<String>,
    /// `tasks` of the run at the last pass: a pass is skipped when no task ran since.
    fed_tasks: usize,
    /// `(link, url)` pairs that got their `load` or `error`.
    notified: HashSet<(NodeId, String)>,
}

pub(super) fn limit(what: &str) -> Stop {
    Stop::Fatal(Fatal::Limit(what.to_string()))
}

/// Only space and comments (`/* ... */`): a good sheet with no rules.
fn no_rules(text: &str) -> bool {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        match rest.strip_prefix("/*") {
            None => return rest.is_empty(),
            Some(after) => match after.split_once("*/") {
                Some((_, tail)) => rest = tail,
                None => return true,
            },
        }
    }
}

/// A failure, as text for a diagnostic. A network error is a fixed label (its text can name a host or an address).
fn why(e: &LoadError) -> String {
    match e {
        LoadError::Network(_) => "network error".into(),
        LoadError::Mime(_) => "content type is not text/css".into(),
        LoadError::Encoding(_) => "content encoding is not supported".into(),
        e => e.to_string(),
    }
}

impl Run<'_> {
    /// A pass if a task ran since the last one.
    pub(super) fn feed_if_ran(&mut self) -> Flow {
        if self.res.tasks != self.dom.scripts.sheets.borrow().fed_tasks {
            self.feed_styles()?;
        }
        Ok(())
    }

    /// One pass: supply every active sheet that the document needs. See the module comment.
    pub(super) fn feed_styles(&mut self) -> Flow {
        self.budget()?;
        let page = self.b.document_url().and_then(|u| Url::parse(u.split('#').next().unwrap_or("")).ok());
        let Some(page) = page else { return Ok(()) };
        let base = resources::document_base(&self.dom.doc.borrow(), &page).unwrap_or(page);
        {
            let mut s = self.dom.scripts.sheets.borrow_mut();
            s.stats.feeds += 1;
            s.fed_tasks = self.res.tasks;
            let now = base.to_string();
            if s.base.as_deref() != Some(&now) {
                // A key whose text now means another URL must be fetched again (from the cache, if the URL is known).
                let stale: Vec<String> = s.supplied.iter().filter(|(raw, url)| base.join(raw.trim_matches(WS)).map(|u| u.to_string()).ok().as_ref() != Some(*url)).map(|(raw, _)| raw.clone()).collect();
                for raw in stale {
                    s.supplied.remove(&raw);
                    self.b.remove_external_stylesheet(&raw);
                }
                s.base = Some(now);
            }
        }
        for _ in 0..MAX_PASSES {
            let fired = self.feed_preloads(&base)?;
            let missing = match self.b.style_availability() {
                Ok(StyleAvailability::Ready) => {
                    // Every sheet of the parsed document is in. A sheet that a script adds from now on loads after its task.
                    self.b.set_pending_sheets(true);
                    let (before, known) = {
                        let s = self.dom.scripts.sheets.borrow();
                        (s.notified.len(), s.supplied.iter().map(|(raw, url)| (raw.clone(), url.clone(), s.fetched.get(url).is_some_and(Result::is_ok))).collect::<Vec<_>>())
                    };
                    for (raw, url, ok) in known { self.tell_links(&raw, &url, ok)?; }
                    if fired || self.dom.scripts.sheets.borrow().notified.len() != before { continue; }
                    return Ok(());
                },
                Ok(StyleAvailability::MissingExternal { hrefs, .. }) => hrefs,
                // Discovery errors stop the driver before any later JavaScript or I/O.
                Err(e) => return Err(fatal_of(e)),
            };
            for raw in missing {
                self.supply(&raw, &base)?;
            }
        }
        Err(limit("css-passes"))
    }

    /// Fetch and supply the sheet that `raw` names, then tell the `<link>` elements that wait for it.
    pub(super) fn supply(&mut self, raw: &str, base: &Url) -> Flow {
        let (parts, url, top_ok) = match base.join(raw.trim_matches(WS)) {
            Err(_) => {
                self.sheet_failed(None, "the URL is not an http(s) URL".into());
                (Vec::new(), String::new(), false)
            }
            Ok(url) => {
                let key = url.to_string();
                match self.fetch_sheet(&url)? {
                    Err(_) => (Vec::new(), key, false),
                    Ok((final_url, text)) => {
                        let mut parts = Vec::new();
                        let mut state = Flat { path: vec![key.clone(), final_url.to_string()], refs: 0, bytes: 0 };
                        self.flatten(&final_url, &text, None, 0, &mut state, &mut parts)?;
                        (parts, key, true)
                    }
                }
            }
        };
        self.budget()?;
        if let Err(e) = self.b.set_external_stylesheet(raw, parts) {
            return Err(fatal_of(e));
        }
        {
            let mut s = self.dom.scripts.sheets.borrow_mut();
            s.stats.supplied += 1;
            self.dom.forget_quiet();
            s.supplied.insert(raw.to_string(), url.clone());
        }
        self.tell_links(raw, &url, top_ok)
    }

    /// The text of `url`: from the cache, or from the network (counted, capped). `Ok(Err)` is a failed sheet, `Err` a stop.
    pub(super) fn fetch_sheet(&mut self, url: &Url) -> Result<Fetched, Stop> {
        let key = url.to_string();
        if let Some(c) = self.dom.scripts.sheets.borrow().fetched.get(&key) {
            return Ok(c.clone());
        }
        self.budget()?;
        let fail = |run: &mut Self, reason: String| {
            run.sheet_failed(Some(url), reason.clone());
            run.dom.scripts.sheets.borrow_mut().fetched.insert(key.clone(), Err(reason.clone()));
            Ok(Err(reason))
        };
        if self.https_only && url.scheme == Scheme::Http {
            return fail(self, "insecure style sheet URL refused".into());
        }
        if self.res.fetches >= self.lim.resources.max_fetches {
            return Err(limit("css-fetches"));
        }
        self.res.fetches += 1;
        self.dom.forget_quiet();
        self.dom.scripts.sheets.borrow_mut().stats.fetches += 1;
        let mut lim = self.lim.resources.clone();
        lim.max_resource_bytes = lim.max_resource_bytes.min(MAX_BODY);
        let retained = self.kept();
        let target = resources::Target { expect: Expect::Style, url: Url { scheme: url.scheme, host: url.host.clone(), port: url.port, path: url.path.clone() } };
        let fetched = self.net.fetch(&target, &lim, self.deadline, retained);
        self.prefetch();
        match fetched {
            Ok(r) => {
                let n = r.text.len();
                self.res.bytes += n;
                let mut s = self.dom.scripts.sheets.borrow_mut();
                s.stats.bytes += n;
                s.stats.loaded += 1;
                s.stats.blank += no_rules(&r.text) as usize;
                s.retained += n;
                let fetched = (Rc::new(r.url), Rc::new(r.text));
                s.fetched.insert(key, Ok(fetched.clone()));
                Ok(Ok(fetched))
            }
            Err(LoadError::TimedOut) => Err(Stop::TimeBudget),
            Err(LoadError::TooLarge) => Err(limit("css-resource-bytes")),
            Err(LoadError::TotalTooLarge) => Err(limit("css-total-bytes")),
            Err(e) => fail(self, why(&e)),
        }
    }

    /// Count a failed sheet and name it (origin and path only, never a query).
    fn sheet_failed(&mut self, url: Option<&Url>, reason: String) {
        self.dom.scripts.sheets.borrow_mut().stats.failed += 1;
        let what = url.map_or_else(|| "bad url".to_string(), label);
        self.diag(format!("style sheet failed: {what}: {reason}"));
    }

    /// The flat parts of the sheet `text` at `url`: its imports first, depth first, then the text. `layer` is the layer that
    /// this sheet is in, relative to the key that is supplied.
    fn flatten(&mut self, url: &Url, text: &str, layer: Option<String>, depth: usize, st: &mut Flat, parts: &mut Vec<SheetPart>) -> Flow {
        let has_import = text.as_bytes().windows(7).any(|w| w.eq_ignore_ascii_case(b"@import"));
        if has_import {
            let vp = self.dom.plat.s.borrow().viewport;
            let imports = cascade::imports(&mut self.dom.doc.borrow_mut(), text, vp);
            if !imports.is_empty() && depth >= MAX_IMPORT_DEPTH {
                return Err(limit("css-import-depth"));
            }
            for imp in imports {
                st.refs += 1;
                if st.refs > MAX_IMPORT_REFS {
                    return Err(limit("css-import-references"));
                }
                let Ok(child) = url.join(imp.href.trim_matches(WS)) else {
                    self.sheet_failed(None, "the import URL is not an http(s) URL".into());
                    continue;
                };
                let key = child.to_string();
                if st.path.contains(&key) {
                    self.dom.scripts.sheets.borrow_mut().stats.cycles += 1;
                    self.diag(format!("style sheet import cycle cut: {}", label(&child)));
                    continue;
                }
                let Ok((final_url, body)) = self.fetch_sheet(&child)? else { continue };
                st.path.push(key);
                st.path.push(final_url.to_string());
                self.flatten(&final_url, &body, join_layer(layer.as_deref(), imp.layer.as_deref()), depth + 1, st, parts)?;
                st.path.pop();
                st.path.pop();
            }
        }
        st.bytes = st.bytes.saturating_add(text.len());
        if parts.len() >= MAX_PARTS {
            return Err(limit("css-parts"));
        }
        if st.bytes > MAX_FLAT {
            return Err(limit("css-flat-bytes"));
        }
        parts.push(SheetPart { text: text.to_string(), layer });
        Ok(())
    }

    /// `load` or `error` for each `<link rel=stylesheet href=raw>` in the tree that has not had one for this URL.
    fn tell_links(&mut self, raw: &str, url: &str, ok: bool) -> Flow {
        let nodes = {
            let doc = self.dom.doc.borrow();
            let (link, template, rel, href, media) = (doc.find_atom("link"), doc.find_atom("template"), doc.find_atom("rel"), doc.find_atom("href"), doc.find_atom("media"));
            let vp = self.dom.plat.s.borrow().viewport;
            let mut out = Vec::new();
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if let NodeData::Element { name, .. } = *doc.data(n)
                    && !doc.is_foreign(n)
                {
                    if Some(name) == template {
                        continue;
                    }
                    let attr = |a: Option<bare_html::dom::Atom>| a.and_then(|a| doc.attr(n, a));
                    if Some(name) == link && attr(href) == Some(raw) {
                        let is = |w: &str| attr(rel).is_some_and(|r| r.split_ascii_whitespace().any(|x| x.eq_ignore_ascii_case(w)));
                        if is("stylesheet") && !is("alternate") && attr(media).is_none_or(|m| bare_css::at_rules::media_matches(m, vp)) {
                            out.push(n);
                        }
                    }
                }
                let mut c = doc.last_child(n);
                while let Some(k) = c {
                    stack.push(k);
                    c = doc.prev_sibling(k);
                }
            }
            out
        };
        for n in nodes {
            if !self.dom.scripts.sheets.borrow_mut().notified.insert((n, url.to_string())) {
                continue;
            }
            self.fire(EvTarget::Node(n), if ok { "load" } else { "error" }, false)?;
            self.after_js()?;
        }
        Ok(())
    }
}

/// The state of one flatten: the URLs on the path (for cycles), the `@import` rules read and the bytes kept.
struct Flat {
    path: Vec<String>,
    refs: usize,
    bytes: usize,
}

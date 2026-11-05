//! `<link rel=preload>` loading for a page session. Child of `browser_loader`: it is part of `Run`.
//!
//! Why: React 19 and the Next.js App Router insert `<link rel=preload as=style>` with `appendChild` and wait for its `load` (or
//! `error`) event before they commit a route. Before this module nothing fetched such a link and nothing fired an event, so a
//! client-side navigation never finished.
//!
//! When it runs: in each style pass (`feed_styles`), so at the same driver boundaries as style sheets (before a script, at the
//! start of a round, and again after any event it fires). It never runs inside `appendChild`: the event is a task of the driver.
//! A pass reads the document only when `Document::revision` moved since the last pass.
//!
//! Which links: HTML `link` elements in the tree (not in a `template`) whose `rel` has the token `preload`, with a non-empty `href`
//! that resolves to an http(s) URL and a `media` that matches. The `as` value decides, as the HTML Standard says:
//! - `style`: fetched through `fetch_sheet` (the cache of `sheets.rs`, keyed by URL). A later `<link rel=stylesheet>` with the same
//!   URL reads that text and does not fetch. A good one is also supplied to `getComputedStyle` under its `href` text, before its `load`
//!   runs (`Run::supply`): in Chrome the sheet of that URL is then in place at once for a stylesheet link (or this link, with `rel`
//!   changed), even in the task that inserts it.
//! - `script`: fetched with `Expect::Script`. The text is kept by URL until the first script with that URL takes it
//!   (`take_script`, used by parsed and dynamic scripts). Kept text counts in the total byte cap (`Run::kept`).
//! - any other destination of the Fetch Standard (`font`, `image`, `fetch`, `audio`, ...): fetched with `Expect::Opaque`. Status and
//!   redirects are checked, the body is counted against the size cap and dropped. Nothing is decoded.
//! - a missing or unknown `as`: no fetch and no event. An empty `href` or an unresolvable URL: the same.
//!
//! Then `load` is dispatched at the link as a task (`res.tasks` grows by one), or `error` when the fetch failed (network, status,
//! type, size). A failure is counted (`PreloadStats::failed`) and named in a diagnostic (`preload failed: origin/path: reason`).
//! A budget is not an `error`: as in `sheets.rs`, a time limit is `Stop::TimeBudget` and a fetch, byte or link cap ends the run.
//!
//! Each `(link, href text)` gets one event. The same URL is fetched once per document and kind: later links to it reuse the
//! result (a failure is not tried again). A link that is detached or changed before its turn is skipped: no fetch, no event.
//!
//! Skipped on purpose: the links that were in the document when the page was parsed. A browser fires their events before any
//! script can listen, and bare has no HTTP cache, so a fetch would only cost time and memory (the staging login has four font
//! preloads). A parsed `<script>` or `<link rel=stylesheet>` still loads at its turn.
//!
//! Not done: `modulepreload`, `type`, `imagesrcset`, `integrity`, `crossorigin` (no CORS mode), `fetchpriority`, `Link:` headers,
//! `<link rel=prefetch>`, `disabled`. A script text that was taken is not kept: a second script with the same URL fetches again.

use super::sheets::limit;
use super::{EvTarget, Run, Stop, WS, download, label};
use crate::browser_dom::LINK_AS;
use crate::resources::{Expect, Target};
use bare_css::at_rules::media_matches;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_url::Url;
use std::collections::HashMap;

/// What the driver did for preload links in this document (all runs and resumes). `fetches` counts requests sent for them, of any
/// kind (a style sheet that was already cached costs none). They are also counted in `PageResult::fetches`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreloadStats {
    pub fetches: usize,
    /// Links that got `load`.
    pub loaded: usize,
    /// Links that got `error`.
    pub failed: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Style,
    Script,
    Opaque,
}

#[derive(Default)]
pub(crate) struct Preloads {
    pub(super) stats: PreloadStats,
    /// False until the first pass: that one only marks the links of the parsed document.
    started: bool,
    /// Link to the `href` text that it was handled for.
    done: HashMap<NodeId, String>,
    /// `Document::revision` at the last scan.
    rev: Option<u64>,
    /// Resolved URL to the result of its script preload: the text until a script takes it (`Ok(None)`: taken), or the reason.
    scripts: HashMap<String, Result<Option<String>, String>>,
    /// Resolved URL to the result of its opaque preload.
    opaque: HashMap<String, Result<(), String>>,
    /// Bytes of text in `scripts`: part of the total cap.
    pub(super) retained: usize,
}

impl Preloads {
    /// The preloaded text for a script at `url`, once. `Err` is the reason that the preload failed.
    pub(super) fn take_script(&mut self, url: &Url) -> Option<Result<String, String>> {
        if self.scripts.is_empty() {
            return None;
        }
        match self.scripts.get_mut(&url.to_string())? {
            Ok(slot) => {
                let text = slot.take()?;
                self.retained -= text.len();
                Some(Ok(text))
            }
            Err(why) => Some(Err(why.clone())),
        }
    }
}

/// Is `n` a preload link that the driver should handle now? Gives its `href` text and the kind.
fn probe(doc: &Document, n: NodeId, vp: (f32, f32), at: &[Option<Atom>; 4]) -> Option<(String, Kind)> {
    let [rel, href, as_, media] = *at;
    let a = |x: Option<Atom>| x.and_then(|x| doc.attr(n, x));
    if !a(rel)?.split_ascii_whitespace().any(|w| w.eq_ignore_ascii_case("preload")) || !a(media).is_none_or(|m| media_matches(m, vp)) {
        return None;
    }
    let as_ = a(as_)?;
    let known = LINK_AS.iter().find(|k| k.eq_ignore_ascii_case(as_))?;
    let kind = match *known {
        "style" => Kind::Style,
        "script" => Kind::Script,
        _ => Kind::Opaque,
    };
    Some((a(href)?.to_string(), kind))
}

fn connected(doc: &Document, mut n: NodeId) -> bool {
    while let Some(p) = doc.parent(n) {
        n = p;
    }
    n == doc.root()
}

impl Run<'_> {
    /// Text kept by the loader: parsed scripts that wait for a repeat, style sheets and preloaded scripts. The total cap counts it.
    pub(super) fn kept(&self) -> usize {
        self.retained + self.dom.scripts.sheets.borrow().retained + self.dom.scripts.preloads.borrow().retained
    }

    /// Handle every preload link that is new. Gives true when an event fired (its listeners may have changed the document).
    pub(super) fn feed_preloads(&mut self, base: &Url) -> Result<bool, Stop> {
        let (links, atoms, vp) = {
            let doc = self.dom.doc.borrow();
            let mut p = self.dom.scripts.preloads.borrow_mut();
            if p.rev == Some(doc.revision()) {
                return Ok(false);
            }
            p.rev = Some(doc.revision());
            let atoms = [doc.find_atom("rel"), doc.find_atom("href"), doc.find_atom("as"), doc.find_atom("media")];
            let vp = self.dom.plat.s.borrow().viewport;
            let (link, template) = (doc.find_atom("link"), doc.find_atom("template"));
            let mut out = Vec::new();
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if let NodeData::Element { name, .. } = *doc.data(n)
                    && !doc.is_foreign(n)
                {
                    if Some(name) == template {
                        continue;
                    }
                    if Some(name) == link
                        && let Some((raw, kind)) = probe(&doc, n, vp, &atoms)
                        && p.done.get(&n) != Some(&raw)
                    {
                        out.push((n, raw, kind));
                    }
                }
                let mut c = doc.last_child(n);
                while let Some(k) = c {
                    stack.push(k);
                    c = doc.prev_sibling(k);
                }
            }
            // The links of the parsed document are only marked (see the module comment).
            if !std::mem::replace(&mut p.started, true) {
                p.done.extend(out.into_iter().map(|(n, raw, _)| (n, raw)));
                return Ok(false);
            }
            (out, atoms, vp)
        };
        let mut fired = false;
        for (n, raw, kind) in links {
            self.budget()?;
            // An earlier event may have removed or changed this link.
            let now = {
                let doc = self.dom.doc.borrow();
                connected(&doc, n).then(|| probe(&doc, n, vp, &atoms)).flatten()
            };
            if now.as_ref().map(|(r, k)| (r, *k)) != Some((&raw, kind)) {
                continue;
            }
            {
                let mut p = self.dom.scripts.preloads.borrow_mut();
                if p.done.len() >= self.lim.resources.max_references {
                    return Err(limit("preload-links"));
                }
                p.done.insert(n, raw.clone());
            }
            let text = raw.trim_matches(WS);
            let Some(url) = Some(text).filter(|t| !t.is_empty()).and_then(|t| base.join(t).ok()) else { continue };
            let ok = match kind {
                Kind::Style => {
                    let before = self.dom.scripts.sheets.borrow().stats.fetches;
                    let ok = self.fetch_sheet(&url)?.is_ok();
                    // Chrome keeps a loaded style preload: a `<link rel=stylesheet>` for it (or this link with `rel` changed) has its sheet
                    // at once, before its `load` runs. So the sheet is supplied under this `href` text.
                    if ok {
                        self.supply(&raw, base)?;
                    }
                    let sent = self.dom.scripts.sheets.borrow().stats.fetches != before;
                    self.dom.scripts.preloads.borrow_mut().stats.fetches += sent as usize;
                    ok
                }
                Kind::Script | Kind::Opaque => self.fetch_preload(&url, kind),
            };
            {
                let mut p = self.dom.scripts.preloads.borrow_mut();
                if ok { p.stats.loaded += 1 } else { p.stats.failed += 1 }
            }
            self.res.tasks += 1;
            self.fire(EvTarget::Node(n), if ok { "load" } else { "error" }, false)?;
            self.after_js()?;
            fired = true;
        }
        Ok(fired)
    }

    /// Fetch a script or opaque preload once per URL. True when it is good. A failure is named in a diagnostic.
    fn fetch_preload(&mut self, url: &Url, kind: Kind) -> bool {
        let key = url.to_string();
        {
            let p = self.dom.scripts.preloads.borrow();
            match kind {
                Kind::Script => if let Some(r) = p.scripts.get(&key) { return r.is_ok() },
                _ => if let Some(r) = p.opaque.get(&key) { return r.is_ok() },
            }
        }
        let expect = if kind == Kind::Script { Expect::Script } else { Expect::Opaque };
        let target = Target { expect, url: Url { scheme: url.scheme, host: url.host.clone(), port: url.port, path: url.path.clone() } };
        let kept = self.kept();
        let sent = self.res.fetches;
        let r = download(&mut self.client, self.https_only, &self.lim, self.deadline, kept, &mut self.res, &target);
        let mut p = self.dom.scripts.preloads.borrow_mut();
        p.stats.fetches += (self.res.fetches != sent) as usize;
        let ok = r.is_ok();
        if let Err(why) = &r {
            let line = format!("preload failed: {}: {why}", label(url));
            drop(p);
            self.diag(line);
            p = self.dom.scripts.preloads.borrow_mut();
        }
        match kind {
            Kind::Script => {
                if let Ok(t) = &r {
                    p.retained += t.len();
                }
                p.scripts.insert(key, r.map(Some));
            }
            _ => {
                p.opaque.insert(key, r.map(drop));
            }
        }
        ok
    }
}

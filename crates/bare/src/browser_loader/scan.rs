//! Live: the scan of a document that the session still parses in slices, as the preload scanner of a browser. It reads the HTML ahead of
//! the parser with the plain tokenizer and starts the downloads of the classic scripts and the style sheets on workers. `run_page` takes
//! the started downloads (`Browser::adopt_scan`), so the parse in slices does not delay them.
//!
//! The scan finds what `resources::discover` finds, by the same rules: no `template`, SVG or MathML content, a script with a JavaScript
//! type and no `nomodule`, a `stylesheet` link that is not `alternate`. A link with a `media` other than `all` or `screen` is left to the
//! style pass. A `<link rel=preload as=script|style>` without `crossorigin` is a hint: its download starts too, and waits for the element that
//! the page code adds later with the same URL (`Net::fetch` finds it by kind and URL). A hint that no element takes holds no page work
//! (`Net::hints`). A `<base href>` counts from where it is: a script before it resolves against the page, so its early download is not used.

use crate::eng::Eng;
use super::{Net, sheets};
use crate::browser::Browser;
use crate::resources::{Expect, JS_TYPES, Limits, WS};
use bare_html::tokenizer::{Mark, Token, Tokens};
use bare_net::http::Client;
use bare_url::Url;
use std::time::Instant;

pub(crate) struct Scan {
    net: Net,
    mark: Mark,
    done: bool,
    page: Url,
    base: Option<Url>,
    lim: Limits,
    deadline: Instant,
    /// Open `template`, `svg` and `math` elements. Nothing in them is loaded.
    skip: usize,
    /// Do not read again before `html` is this long (see `step`).
    retry_at: usize,
}

impl Scan {
    pub(crate) fn new(client: Client, page: Url, lim: Limits, deadline: Instant) -> Scan {
        Scan { net: Net::new(client, &page), mark: Mark::default(), done: false, page, base: None, lim, deadline, skip: 0, retry_at: 0 }
    }

    /// Read `html` from where the last call stopped, until `end`. `more`: the document is still on its way, so `html` may stop in the
    /// middle of a tag, and the scan is not done when it has read it all.
    pub(crate) fn step(&mut self, html: &str, end: Instant, more: bool) {
        // A token that is cut and very long (an inline script of 1 MB) would be read again for each piece: wait until its text has grown
        // by its own size, as `tree::Parser` does.
        if self.done || (more && html.len() < self.retry_at) {
            return;
        }
        let mut tokens = Tokens::new(html, self.mark, more);
        let mut n = 0u32;
        while let Some(t) = tokens.next() {
            match t {
                Token::StartTag { name, attrs, self_closing } => self.start(&name, &attrs, self_closing),
                Token::EndTag { name } if matches!(&*name, "template" | "svg" | "math") => self.skip = self.skip.saturating_sub(1),
                _ => {}
            }
            n += 1;
            if n.is_multiple_of(64) && tokens.between() && bare_net::vclock::instant() >= end {
                self.mark = tokens.mark();
                return;
            }
        }
        self.mark = tokens.mark();
        self.done = !more;
        self.retry_at = html.len() + (html.len() - self.mark.done());
    }

    fn start(&mut self, name: &str, attrs: &[bare_html::tokenizer::Attr<'_>], self_closing: bool) {
        let attr = |n: &str| attrs.iter().find(|a| a.0 == n).map(|a| a.1.trim_matches(WS));
        match name {
            "template" | "svg" | "math" => self.skip += usize::from(!self_closing || name == "template"),
            _ if self.skip > 0 => {}
            "base" if self.base.is_none() => self.base = attr("href").and_then(|h| self.page.join(h).ok()),
            "script" => {
                let classic = attr("type").is_none_or(|t| t.is_empty() || JS_TYPES.iter().any(|j| t.eq_ignore_ascii_case(j)));
                if let Some(src) = attr("src").filter(|s| classic && !s.is_empty() && attr("nomodule").is_none()) {
                    self.fetch(Expect::Script, src, false);
                }
            }
            "link" => {
                let rel = attr("rel").unwrap_or("");
                let has = |w: &str| rel.split_ascii_whitespace().any(|r| r.eq_ignore_ascii_case(w));
                let media = attr("media").is_none_or(|m| m.eq_ignore_ascii_case("all") || m.eq_ignore_ascii_case("screen"));
                let Some(href) = attr("href").filter(|h| media && attr("disabled").is_none() && !h.is_empty()) else { return };
                if has("stylesheet") && !has("alternate") {
                    self.fetch(Expect::Style, href, false);
                } else if has("preload") && attr("crossorigin").is_none() {
                    // A hint: the page names a file that its code adds later (the loader of Gmail). It has no element yet, so the download
                    // waits in `started` for the element that asks for the same URL, and it holds nothing until then (`Net::hints`).
                    // `crossorigin` is another credentials mode than a script without it. bare has one mode, so such a link starts nothing.
                    match attr("as") {
                        Some(a) if a.eq_ignore_ascii_case("script") => self.fetch(Expect::Script, href, true),
                        Some(a) if a.eq_ignore_ascii_case("style") => self.fetch(Expect::Style, href, true),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// `hint`: the URL comes from a preload link, not from an element of the document.
    fn fetch(&mut self, expect: Expect, raw: &str, hint: bool) {
        let Ok(url) = self.base.as_ref().unwrap_or(&self.page).join(raw) else { return };
        if (self.page.scheme == bare_url::Scheme::Https && url.scheme == bare_url::Scheme::Http) || self.net.started.len() >= self.lim.max_fetches {
            return;
        }
        let mut lim = self.lim.clone();
        if expect == Expect::Style {
            lim.max_resource_bytes = lim.max_resource_bytes.min(sheets::MAX_BODY);
        }
        let key = Net::key(expect, &url);
        let new = !self.net.started.contains_key(&key);
        self.net.start_with(expect, &url, &lim, self.deadline, 0);
        // An element of the document with the URL of a hint is a real reference: from then on it holds the page like any other.
        if !hint {
            self.net.hints.remove(&key);
        } else if new && self.net.started.contains_key(&key) {
            self.net.hints.insert(key);
        }
    }
}

impl<E: Eng> Browser<E> {
    /// Live: `run_page` goes on with the downloads that the scan of the document started.
    pub(crate) fn adopt_scan(&self, scan: Scan) {
        *self.dom.scripts.net.borrow_mut() = Some(Box::new(scan.net));
    }
}

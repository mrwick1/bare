//! Live: the scan of a document that the session still parses in slices, as the preload scanner of a browser. It reads the HTML ahead of
//! the parser with the plain tokenizer and starts the downloads of the classic scripts and the style sheets on workers. `run_page` takes
//! the started downloads (`Browser::adopt_scan`), so the parse in slices does not delay them.
//!
//! The scan finds what `resources::discover` finds, by the same rules: no `template`, SVG or MathML content, a script with a JavaScript
//! type and no `nomodule`, a `stylesheet` link that is not `alternate`. A link with a `media` other than `all` or `screen` is left to the
//! style pass. A `<base href>` counts from where it is: a script before it resolves against the page, so its early download is not used.

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
}

impl Scan {
    pub(crate) fn new(client: Client, page: Url, lim: Limits, deadline: Instant) -> Scan {
        Scan { net: Net::new(client), mark: Mark::default(), done: false, page, base: None, lim, deadline, skip: 0 }
    }

    /// Read `html` from where the last call stopped, until `end`.
    pub(crate) fn step(&mut self, html: &str, end: Instant) {
        if self.done {
            return;
        }
        let mut tokens = Tokens::new(html, self.mark, false);
        let mut n = 0u32;
        while let Some(t) = tokens.next() {
            match t {
                Token::StartTag { name, attrs, self_closing } => self.start(&name, &attrs, self_closing),
                Token::EndTag { name } if matches!(&*name, "template" | "svg" | "math") => self.skip = self.skip.saturating_sub(1),
                _ => {}
            }
            n += 1;
            if n.is_multiple_of(64) && tokens.between() && Instant::now() >= end {
                self.mark = tokens.mark();
                return;
            }
        }
        self.done = true;
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
                    self.fetch(Expect::Script, src);
                }
            }
            "link" => {
                let rel = attr("rel").unwrap_or("");
                let has = |w: &str| rel.split_ascii_whitespace().any(|r| r.eq_ignore_ascii_case(w));
                let media = attr("media").is_none_or(|m| m.eq_ignore_ascii_case("all") || m.eq_ignore_ascii_case("screen"));
                if let Some(href) = attr("href").filter(|h| has("stylesheet") && !has("alternate") && media && attr("disabled").is_none() && !h.is_empty()) {
                    self.fetch(Expect::Style, href);
                }
            }
            _ => {}
        }
    }

    fn fetch(&mut self, expect: Expect, raw: &str) {
        let Ok(url) = self.base.as_ref().unwrap_or(&self.page).join(raw) else { return };
        if (self.page.scheme == bare_url::Scheme::Https && url.scheme == bare_url::Scheme::Http) || self.net.started.len() >= self.lim.max_fetches {
            return;
        }
        let mut lim = self.lim.clone();
        if expect == Expect::Style {
            lim.max_resource_bytes = lim.max_resource_bytes.min(sheets::MAX_BODY);
        }
        self.net.start(expect, &url, &lim, self.deadline, 0);
    }
}

impl Browser {
    /// Live: `run_page` goes on with the downloads that the scan of the document started.
    pub(crate) fn adopt_scan(&self, scan: Scan) {
        *self.dom.scripts.net.borrow_mut() = Some(Box::new(scan.net));
    }
}

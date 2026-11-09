//! `<meta http-equiv="refresh">`: the HTML "shared declarative refresh steps". The session calls `Browser::start_refresh` once for a
//! document that it has built and not yet run. The first valid `<meta http-equiv=refresh content=...>` in the document, outside `<body>`,
//! queues a timer that calls `location.replace` (history "replace") after the delay. The timer is queued work: a live page waits for it.
//! Loops end at `SessionLimits::max_documents`.
//!
//! Not done: a `meta` that a script inserts later, a `meta` inside `<body>`, the `Refresh` response header, `<base>` (the URL is
//! resolved against the document URL, as `location.replace` does). `Session::run` settles when a timer is more than `PageLimits::quiet`
//! away, so a refresh later than that does not hold a run to the settle; a live page does wait.

use crate::browser::Browser;
use bare_html::dom::NodeData;
use bare_url::Url;

/// The longest delay that a timer can hold (2^31 - 1 ms). A longer one fires at once in a browser, so the refresh is dropped instead.
const MAX_SECONDS: u64 = (i32::MAX / 1000) as u64;

const WS: &[char] = &['\t', '\n', '\x0c', '\r', ' '];

/// The steps of the standard for the content of `meta`: the delay in seconds and the URL text (`None`: the document URL).
fn parse(input: &str) -> Option<(u64, Option<&str>)> {
    let mut rest = input.trim_start_matches(WS);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 && !rest.starts_with('.') {
        return None;
    }
    let seconds = rest[..digits].bytes().fold(0u64, |t, b| t.saturating_mul(10).saturating_add((b - b'0') as u64));
    rest = rest[digits..].trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if rest.is_empty() {
        return Some((seconds, None));
    }
    if !rest.starts_with([';', ',']) && !rest.starts_with(WS) {
        return None;
    }
    rest = rest.trim_start_matches(WS);
    rest = rest.strip_prefix([';', ',']).unwrap_or(rest).trim_start_matches(WS);
    // `url=` is optional and its letters are not case sensitive. Where it breaks off, the text from that place on is the URL.
    let mut at = rest;
    let mut after_eq = None;
    if let Some(t) = at.strip_prefix(['u', 'U'])
        && let Some(t) = t.strip_prefix(['r', 'R'])
        && let Some(t) = t.strip_prefix(['l', 'L'])
        && let Some(t) = t.trim_start_matches(WS).strip_prefix('=')
    {
        after_eq = Some(t.trim_start_matches(WS));
    } else if let Some(t) = at.strip_prefix(['u', 'U']) {
        at = t.strip_prefix(['r', 'R']).map_or(t, |t| t.strip_prefix(['l', 'L']).map_or(t, |t| t.trim_start_matches(WS)));
    }
    let mut url = after_eq.unwrap_or(at);
    if let Some(q) = url.chars().next().filter(|c| matches!(c, '\'' | '"')) {
        url = &url[1..];
        if let Some(end) = url.find(q) {
            url = &url[..end];
        }
    }
    Some((seconds, Some(url)))
}

impl Browser {
    /// Queue the refresh of the first `meta` that asks for one. Reads the `head` only (a walk that skips `body`).
    pub(crate) fn start_refresh(&mut self) {
        let Some((seconds, target)) = self.find_refresh() else { return };
        // The URL is resolved here: a `javascript:` or other non-http URL does not start a timer.
        let Some(base) = self.document_url() else { return };
        let Ok(base) = Url::parse(base.split('#').next().unwrap_or("")) else { return };
        let to = match target {
            None => base.to_string(),
            Some(t) => match base.join(t.trim_matches(WS)) {
                Ok(u) => u.to_string(),
                Err(_) => return,
            },
        };
        // A serialized URL has no quote, backslash or line break that is not percent-encoded, except `'` and `\`.
        let to = to.replace('\\', "\\\\").replace('\'', "\\'");
        let _ = self.run_script(&format!("setTimeout(function () {{ location.replace('{to}') }}, {})", seconds * 1000));
    }

    fn find_refresh(&self) -> Option<(u64, Option<String>)> {
        let doc = self.dom.doc.borrow();
        let (meta, body, template) = (doc.find_atom("meta")?, doc.find_atom("body"), doc.find_atom("template"));
        let (equiv, content) = (doc.find_atom("http-equiv")?, doc.find_atom("content")?);
        let mut stack = vec![doc.root()];
        while let Some(node) = stack.pop() {
            if let NodeData::Element { name, .. } = *doc.data(node) {
                if Some(name) == body || Some(name) == template {
                    continue;
                }
                if name == meta && !doc.is_foreign(node) && doc.attr(node, equiv).is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
                    && let Some((s, t)) = doc.attr(node, content).and_then(parse)
                    && s <= MAX_SECONDS
                {
                    return Some((s, t.map(str::to_owned)));
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
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn the_content_follows_the_standard() {
        let cases: [(&str, Option<(u64, Option<&str>)>); 20] = [
            ("5", Some((5, None))),
            ("0", Some((0, None))),
            (" 3 ", Some((3, Some("")))),
            ("5; url=https://a.test/", Some((5, Some("https://a.test/")))),
            ("5;URL=https://a.test/", Some((5, Some("https://a.test/")))),
            ("5, url = /x", Some((5, Some("/x")))),
            ("5 url=/x", Some((5, Some("/x")))),
            ("5; url='/x y' tail", Some((5, Some("/x y")))),
            ("5; url=\"/x\"", Some((5, Some("/x")))),
            ("5; url='/x", Some((5, Some("/x")))),
            ("5; /x", Some((5, Some("/x")))),
            ("5;", Some((5, Some("")))),
            ("1.9; url=/x", Some((1, Some("/x")))),
            (".5; url=/x", Some((0, Some("/x")))),
            ("2.5.1", Some((2, None))),
            ("-1; url=/x", None),
            ("soon; url=/x", None),
            ("", None),
            ("5x", None),
            ("5; ur=/x", Some((5, Some("=/x")))),
        ];
        for (input, want) in cases {
            assert_eq!(parse(input), want, "{input:?}");
        }
    }
}

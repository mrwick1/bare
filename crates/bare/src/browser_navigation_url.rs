//! URL records for the navigation module: `bare_url::Url` plus a fragment, with the WHATWG operations that the
//! browser APIs need (parse with a base, getters, setters). No JS here.
//!
//! `bare_url` is the one parser. It has these limits, and each one is gated here with an explicit error, never
//! silently turned into another URL (checked against Node's `URL` by `tools/js/navigation_check.sh`):
//! - it drops the fragment, so `Rec` keeps it (`frag`);
//! - `parse` does not remove dot segments (`join` does): the path always goes through `join`;
//! - no user info (`u:p@host`), no `\` as `/`, no `http:host` or `http:///host` forms, no `%2e` dot segments;
//! - hosts are ASCII only: a non-ASCII, `%`-escaped or `xn--` host is gated (IDNA is not claimed);
//! - only `http` and `https`. Any other scheme is `UErr::Scheme` (a script may still see it as a cross-origin
//!   refusal, as `pushState` does).

use bare_url::{Scheme, Url};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UErr {
    /// Not a URL (a browser throws a `TypeError` or `SyntaxError`).
    Invalid,
    /// A scheme other than `http` and `https`.
    Scheme,
    /// A form that a browser accepts and that this code does not model.
    Gate(&'static str),
}

pub(crate) struct Rec {
    pub u: Url,
    pub frag: Option<String>,
}

fn dup(u: &Url) -> Url {
    Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: u.path.clone() }
}

impl Clone for Rec {
    fn clone(&self) -> Rec {
        Rec { u: dup(&self.u), frag: self.frag.clone() }
    }
}

fn frag_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b <= b' ' || b >= 0x7f || matches!(b, b'"' | b'<' | b'>' | b'`') {
            o.push_str(&format!("%{b:02X}"));
        } else {
            o.push(b as char);
        }
    }
    o
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect()
}

/// `(scheme, text after the colon)` when `s` starts with a URL scheme.
fn scheme_of(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| !(c.is_ascii_alphanumeric() || "+-.".contains(c)))?;
    (end > 0 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[end] == b':').then(|| (&s[..end], &s[end + 1..]))
}

fn dot_escape(path: &str) -> bool {
    let p = path.split('?').next().unwrap_or("").to_ascii_lowercase();
    p.contains("%2e")
}

fn host_gate(host: &str) -> Result<(), UErr> {
    if !host.is_ascii() || host.contains('%') || host.to_ascii_lowercase().contains("xn--") {
        return Err(UErr::Gate("non-ASCII, escaped or punycode host (IDNA is gated)"));
    }
    Ok(())
}

fn absolute(scheme: &str, after: &str) -> Result<Rec, UErr> {
    if after.starts_with('/') {
        return Err(UErr::Gate("extra slashes after the scheme"));
    }
    let end = after.find(['/', '?']).unwrap_or(after.len());
    let (auth, tail) = after.split_at(end);
    if auth.contains('@') {
        return Err(UErr::Gate("user info in a URL"));
    }
    if auth.is_empty() {
        return Err(UErr::Invalid);
    }
    if !auth.starts_with('[') {
        host_gate(auth.split(':').next().unwrap_or(""))?;
    }
    let root = Url::parse(&format!("{scheme}://{auth}/")).map_err(|_| UErr::Invalid)?;
    if tail.starts_with("//") {
        return Err(UErr::Gate("empty first path segment"));
    }
    if dot_escape(tail) {
        return Err(UErr::Gate("percent-encoded dot segment"));
    }
    let u = if tail.is_empty() { root } else { root.join(tail).map_err(|_| UErr::Invalid)? };
    Ok(Rec { u, frag: None })
}

/// Parse `input`, relative to `base` when it has no scheme.
pub(crate) fn parse(input: &str, base: Option<&Rec>) -> Result<Rec, UErr> {
    let s = clean(input);
    let s = s.trim_matches(|c: char| c <= ' ');
    if s.contains('\\') {
        return Err(UErr::Gate("backslash in a URL"));
    }
    let (rest, frag) = match s.split_once('#') {
        Some((r, f)) => (r, Some(frag_encode(f))),
        None => (s, None),
    };
    let mut rec = if let Some((sch, after)) = scheme_of(rest) {
        let low = sch.to_ascii_lowercase();
        if low != "http" && low != "https" {
            return Err(UErr::Scheme);
        }
        match after.strip_prefix("//") {
            Some(a) => absolute(&low, a)?,
            None => return Err(UErr::Gate("http(s) URL without //")),
        }
    } else if let Some(a) = rest.strip_prefix("//") {
        absolute(&base.ok_or(UErr::Invalid)?.u.scheme.to_string(), a)?
    } else {
        let b = base.ok_or(UErr::Invalid)?;
        if rest.split(['/', '?']).next().unwrap_or("").contains(':') {
            return Err(UErr::Gate("colon in the first relative path segment"));
        }
        if dot_escape(rest) {
            return Err(UErr::Gate("percent-encoded dot segment"));
        }
        Rec { u: b.u.join(rest).map_err(|_| UErr::Invalid)?, frag: None }
    };
    rec.frag = frag;
    Ok(rec)
}

/// Percent-encode a query with the URL encoder (`#` is escaped first: it would end the query).
fn query_encode(v: &str) -> Result<String, UErr> {
    let u = Url::parse(&format!("http://h/?{}", clean(v).replace('#', "%23"))).map_err(|_| UErr::Invalid)?;
    Ok(u.path.split_once('?').map(|x| x.1.to_string()).unwrap_or_default())
}

impl Rec {
    pub fn href(&self) -> String {
        let mut s = self.u.to_string();
        if let Some(f) = &self.frag {
            s.push('#');
            s.push_str(f);
        }
        s
    }
    pub fn protocol(&self) -> String {
        format!("{}:", self.u.scheme)
    }
    pub fn hostname(&self) -> String {
        if self.u.host.contains(':') { format!("[{}]", self.u.host) } else { self.u.host.clone() }
    }
    pub fn port(&self) -> String {
        if self.u.port == self.u.scheme.default_port() { String::new() } else { self.u.port.to_string() }
    }
    pub fn host(&self) -> String {
        let p = self.port();
        if p.is_empty() { self.hostname() } else { format!("{}:{p}", self.hostname()) }
    }
    pub fn origin(&self) -> String {
        format!("{}//{}", self.protocol(), self.host())
    }
    pub fn pathname(&self) -> &str {
        self.u.path.split('?').next().unwrap_or("/")
    }
    /// The query without `?`. `Some("")` for a bare `?`.
    pub fn query(&self) -> Option<&str> {
        self.u.path.split_once('?').map(|x| x.1)
    }
    pub fn search(&self) -> String {
        match self.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }
    }
    pub fn hash(&self) -> String {
        match &self.frag {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }
    }
    /// Set the already encoded query (`None` removes it, with its `?`).
    pub fn set_query(&mut self, q: Option<&str>) {
        let p = self.pathname().to_string();
        self.u.path = match q {
            Some(q) => format!("{p}?{q}"),
            None => p,
        };
    }
    pub fn set_search(&mut self, v: &str) -> Result<(), UErr> {
        let q = if v.is_empty() { None } else { Some(query_encode(v.strip_prefix('?').unwrap_or(v))?) };
        self.set_query(q.as_deref());
        Ok(())
    }
    /// The `Location.hash` setter: a fragment is always set, even an empty one.
    pub fn set_fragment(&mut self, v: &str) {
        let v = clean(v);
        self.frag = Some(frag_encode(v.strip_prefix('#').unwrap_or(&v)));
    }
    pub fn set_hash(&mut self, v: &str) {
        let v = clean(v);
        self.frag = if v.is_empty() { None } else { Some(frag_encode(v.strip_prefix('#').unwrap_or(&v))) };
    }
    pub fn set_pathname(&mut self, v: &str) -> Result<(), UErr> {
        let v = clean(v);
        if v.contains('\\') {
            return Err(UErr::Gate("backslash in a pathname"));
        }
        let p = if v.starts_with('/') { v } else { format!("/{v}") };
        if p.starts_with("//") {
            return Err(UErr::Gate("empty first path segment"));
        }
        let p = p.replace('?', "%3F").replace('#', "%23");
        if dot_escape(&p) {
            return Err(UErr::Gate("percent-encoded dot segment"));
        }
        let q = self.query().map(str::to_string);
        self.u = self.u.join(&p).map_err(|_| UErr::Invalid)?;
        self.set_query(q.as_deref());
        Ok(())
    }
    /// Set the host (and the port when `v` has one). An invalid value leaves the URL as it was.
    pub fn set_host(&mut self, v: &str, with_port: bool) -> Result<(), UErr> {
        let v = clean(v);
        if v.is_empty() || v.contains(['/', '?', '#', '\\', '@', ' ']) {
            return Ok(());
        }
        let (h, p) = match v.rfind(':') {
            Some(i) if !v.ends_with(']') && (!v.starts_with('[') || v[..i].ends_with(']')) => (&v[..i], Some(&v[i + 1..])),
            _ => (&v[..], None),
        };
        if !h.starts_with('[') {
            host_gate(h)?;
            if h.contains(':') {
                return Ok(());
            }
        }
        if p.is_some() && !with_port {
            return Ok(());
        }
        let Ok(u) = Url::parse(&format!("{}://{h}/", self.u.scheme)) else { return Ok(()) };
        self.u.host = u.host;
        if let Some(p) = p.filter(|p| !p.is_empty()) {
            self.set_port(p);
        }
        Ok(())
    }
    pub fn set_port(&mut self, v: &str) {
        let v = clean(v);
        if v.is_empty() {
            self.u.port = self.u.scheme.default_port();
            return;
        }
        let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(p) = digits.parse::<u16>() {
            self.u.port = p;
        }
    }
    pub fn set_protocol(&mut self, v: &str) -> Result<(), UErr> {
        let name = clean(v);
        let name = name.split(':').next().unwrap_or("").to_ascii_lowercase();
        let new = match name.as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            "ftp" | "ws" | "wss" | "file" => return Err(UErr::Scheme),
            _ => return Ok(()),
        };
        // The port is "null" in a URL that shows the default port: it follows the new scheme.
        if self.u.port == self.u.scheme.default_port() {
            self.u.port = new.default_port();
        }
        self.u.scheme = new;
        Ok(())
    }
}

/// A URL component that `URL` and `Location` read and write the same way.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Href,
    Protocol,
    Host,
    Hostname,
    Port,
    Pathname,
    Search,
    Hash,
}

impl Part {
    pub fn get(self, r: &Rec) -> String {
        match self {
            Part::Href => r.href(),
            Part::Protocol => r.protocol(),
            Part::Host => r.host(),
            Part::Hostname => r.hostname(),
            Part::Port => r.port(),
            Part::Pathname => r.pathname().to_string(),
            Part::Search => r.search(),
            Part::Hash => r.hash(),
        }
    }

    /// Set a component. `Href` is not handled here (it needs a fresh parse).
    pub fn set(self, r: &mut Rec, v: &str) -> Result<(), UErr> {
        match self {
            Part::Href => unreachable!("href is parsed, not set"),
            Part::Protocol => r.set_protocol(v),
            Part::Host => r.set_host(v, true),
            Part::Hostname => r.set_host(v, false),
            Part::Port => {
                r.set_port(v);
                Ok(())
            }
            Part::Pathname => r.set_pathname(v),
            Part::Search => r.set_search(v),
            Part::Hash => {
                r.set_hash(v);
                Ok(())
            }
        }
    }
}

// ------------------------------------------------------------------ application/x-www-form-urlencoded

pub(crate) fn form_parse(s: &str) -> Vec<(String, String)> {
    fn dec(s: &str) -> String {
        let b: Vec<u8> = s.bytes().map(|c| if c == b'+' { b' ' } else { c }).collect();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
            match (b[i], hex) {
                (b'%', Some(v)) if b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                    out.push(v);
                    i += 3;
                }
                (c, _) => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (n, v) = p.split_once('=').unwrap_or((p, ""));
            (dec(n), dec(v))
        })
        .collect()
}

fn form_enc(s: &str, out: &mut String) {
    for b in s.bytes() {
        match b {
            b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            b' ' => out.push('+'),
            _ if b.is_ascii_alphanumeric() => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
}

pub(crate) fn form_serialize(list: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (n, v)) in list.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        form_enc(n, &mut out);
        out.push('=');
        form_enc(v, &mut out);
    }
    out
}

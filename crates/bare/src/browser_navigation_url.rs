//! URL records for the navigation module: `bare_url::Url` plus a fragment, with the WHATWG operations that the
//! browser APIs need (parse with a base, getters, setters). No JS here.
//!
//! `bare_url` is the one parser. It has these limits, and each one is gated here with an explicit error, never
//! silently turned into another URL (checked against Node's `URL` by `tools/js/navigation_check.sh`):
//! - it drops the fragment, so `Rec` keeps it (`frag`);
//! - `parse` does not remove dot segments (`join` does): the path always goes through `join`;
//! - HTTP user info is retained separately; no backslash normalization, shorthand HTTP forms or escaped dot segments;
//! - HTTP host names use the IDNA conversion of `bare_url`; other schemes keep their host gates;
//! - `parse` accepts only `http` and `https`. Any other scheme is `UErr::Scheme` (a script may still see it as a cross-origin
//!   refusal, as `pushState` does). `parse_any` is for the `URL` class: it also reads other schemes into an `Alt` (see there).

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
    pub userinfo: Option<Box<(String, String)>>,
}

/// A URL of another scheme (`data:`, `mailto:`, `blob:`, `about:`, `ws:`, `sveltekit-internal://`): the parts that `URL` shows. The host is kept as
/// written (lower case for `ws`, `wss`, `ftp`), with no IDNA and no user info. `host` is `None` for a URL with no `//` (`mailto:a@b`, `about:blank`).
#[derive(Clone)]
struct Alt {
    scheme: String,
    host: Option<String>,
    port: Option<u16>,
    /// The path, then `?` and the query when there is one.
    pq: String,
}

fn special_port(scheme: &str) -> Option<u16> {
    match scheme {
        "ftp" => Some(21),
        "ws" => Some(80),
        "wss" => Some(443),
        _ => None,
    }
}

fn dup(u: &Url) -> Url {
    Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: u.path.clone() }
}

impl Clone for Rec {
    fn clone(&self) -> Rec {
        Rec { u: dup(&self.u), frag: self.frag.clone(), userinfo: self.userinfo.clone() }
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

pub(crate) fn clean(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect()
}

/// `(scheme, text after the colon)` when `s` starts with a URL scheme.
pub(crate) fn scheme_of(s: &str) -> Option<(&str, &str)> {
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
    let (userinfo, auth) = match auth.rsplit_once('@') {
        Some((info, host)) => {
            let (user, pass) = info.split_once(':').unwrap_or((info, ""));
            (Some(Box::new((userinfo_encode(user), userinfo_encode(pass)))), host)
        }
        None => (None, auth),
    };
    if auth.is_empty() {
        return Err(UErr::Invalid);
    }
    let root = Url::parse(&format!("{scheme}://{auth}/")).map_err(|_| UErr::Invalid)?;
    if tail.starts_with("//") {
        return Err(UErr::Gate("empty first path segment"));
    }
    if dot_escape(tail) {
        return Err(UErr::Gate("percent-encoded dot segment"));
    }
    let u = if tail.is_empty() { root } else { root.join(tail).map_err(|_| UErr::Invalid)? };
    Ok(Rec { u, frag: None, userinfo })
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
        Rec { u: b.u.join(rest).map_err(|_| UErr::Invalid)?, frag: None, userinfo: b.userinfo.clone() }
    };
    rec.frag = frag;
    Ok(rec)
}

/// The path and query `p` (it starts with `/`) with the dot segments removed and the characters encoded, by the parser of `http` URLs.
fn norm(p: &str) -> Result<String, UErr> {
    if dot_escape(p) {
        return Err(UErr::Gate("percent-encoded dot segment"));
    }
    let root = Url::parse("http://h/").map_err(|_| UErr::Invalid)?;
    Ok(root.join(p).map_err(|_| UErr::Invalid)?.path)
}

/// `parse` for the `URL` class: a URL of any scheme. `http` and `https` go to `parse`. Another scheme gives an `Alt` record, and so does a relative
/// reference against an `Alt` base. HTTP(S) URLs retain user info. Other-scheme user info, IDNA hosts on other schemes, opaque-path encoding, file drives and backslashes remain gated.
pub(crate) fn parse_any(input: &str, base: Option<&Rec>) -> Result<Rec, UErr> {
    // Fast path: an `http(s)` or scheme-less input against a base that is not an `Alt` is the strict parser's. It reads only the scheme part, and copies nothing.
    if base.is_none_or(|b| !b.is_alt()) {
        let t = input.trim_start_matches(|c: char| c <= ' ').as_bytes();
        let end = t.iter().position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')));
        let fast = match end {
            Some(i) if i > 0 && t[i] == b':' && t[0].is_ascii_alphabetic() => t[..i].eq_ignore_ascii_case(b"http") || t[..i].eq_ignore_ascii_case(b"https"),
            Some(i) => !matches!(t[i], b'\t' | b'\n' | b'\r'),
            None => true,
        };
        if fast {
            return parse(input, base);
        }
    }
    let s = clean(input);
    let s = s.trim_matches(|c: char| c <= ' ');
    if s.contains('\\') {
        return Err(UErr::Gate("backslash in a URL"));
    }
    let (rest, frag) = match s.split_once('#') {
        Some((r, f)) => (r, Some(frag_encode(f))),
        None => (s, None),
    };
    let alt = match scheme_of(rest) {
        Some((sch, _)) if matches!(sch.to_ascii_lowercase().as_str(), "http" | "https") => return parse(input, base),
        Some((sch, after)) => alt_absolute(&sch.to_ascii_lowercase(), after)?,
        None => match base.and_then(Rec::alt) {
            Some(b) => alt_relative(&b, rest)?,
            None => return parse(input, base),
        },
    };
    Ok(Rec { u: alt.into_url(), frag, userinfo: None })
}

fn alt_absolute(scheme: &str, after: &str) -> Result<Alt, UErr> {
    let special = special_port(scheme).is_some();
    let Some(a) = after.strip_prefix("//") else {
        // `scheme:/path` has a path and no host. `scheme:text` has an opaque path (`mailto:`, `data:`, `about:`).
        let (path, q) = after.split_once('?').map_or((after, None), |(p, q)| (p, Some(q)));
        let path = if path.starts_with('/') { norm(path)?.split('?').next().unwrap_or("").to_string() } else { path.to_string() };
        let pq = match q {
            Some(q) => format!("{path}?{q}"),
            None => path,
        };
        return Ok(Alt { scheme: scheme.into(), host: None, port: None, pq });
    };
    let end = a.find(['/', '?']).unwrap_or(a.len());
    let (auth, tail) = a.split_at(end);
    if auth.contains('@') {
        return Err(UErr::Gate("user info in a URL"));
    }
    let (host, port) = match auth.rfind(':') {
        Some(i) if !auth.ends_with(']') => (&auth[..i], Some(&auth[i + 1..])),
        _ => (auth, None),
    };
    host_gate(host)?;
    let port = match port {
        None | Some("") => None,
        Some(p) => Some(p.parse::<u16>().map_err(|_| UErr::Invalid)?).filter(|&p| Some(p) != special_port(scheme)),
    };
    let host = if special { host.to_ascii_lowercase() } else { host.to_string() };
    let pq = if tail.is_empty() {
        String::new()
    } else if tail.starts_with('?') {
        // No path: the query follows it directly (`foo://h?q`; a special scheme has the path `/`).
        if special { format!("/{tail}") } else { tail.to_string() }
    } else {
        norm(tail)?
    };
    Ok(Alt { scheme: scheme.into(), host: Some(host), port, pq })
}

/// `rest` (no fragment) against the `Alt` base `b`.
fn alt_relative(b: &Alt, rest: &str) -> Result<Alt, UErr> {
    let mut out = b.clone();
    if b.host.is_none() && !b.pq.starts_with('/') {
        // An opaque path takes a fragment only.
        return if rest.is_empty() { Ok(out) } else { Err(UErr::Invalid) };
    }
    if let Some(a) = rest.strip_prefix("//") {
        return alt_absolute(&b.scheme, &format!("//{a}"));
    }
    if rest.is_empty() {
        return Ok(out);
    }
    if let Some(q) = rest.strip_prefix('?') {
        out.pq = format!("{}?{q}", out.pathname_of());
        return Ok(out);
    }
    let merged = if rest.starts_with('/') {
        rest.to_string()
    } else {
        let dir = out.pathname_of();
        format!("{}{rest}", &dir[..dir.rfind('/').map_or(0, |i| i + 1)])
    };
    out.pq = norm(if merged.starts_with('/') { &merged } else { return Err(UErr::Invalid) })?;
    Ok(out)
}

impl Alt {
    fn pathname_of(&self) -> &str {
        self.pq.split('?').next().unwrap_or("")
    }
    fn href(&self) -> String {
        match &self.host {
            Some(h) => format!("{}://{h}{}{}", self.scheme, self.port.map_or(String::new(), |p| format!(":{p}")), self.pq),
            None => format!("{}:{}", self.scheme, self.pq),
        }
    }
    /// The record of an `Alt`: `u` has no host and port 0 (an `http` URL never has both), and its path is the whole URL without the fragment.
    /// A `Rec` has no field for it, so `Location` pays nothing for the other schemes. `from_href` is the inverse of `href`.
    fn into_url(self) -> Url {
        Url { scheme: Scheme::Http, host: String::new(), port: 0, path: self.href() }
    }
    fn from_href(h: &str) -> Alt {
        let (scheme, after) = h.split_once(':').unwrap_or((h, ""));
        let scheme = scheme.to_string();
        let Some(a) = after.strip_prefix("//") else { return Alt { scheme, host: None, port: None, pq: after.to_string() } };
        let (auth, tail) = a.split_at(a.find(['/', '?']).unwrap_or(a.len()));
        let (host, port) = match auth.rfind(':') {
            Some(i) if !auth.ends_with(']') => (&auth[..i], auth[i + 1..].parse().ok()),
            _ => (auth, None),
        };
        Alt { scheme, host: Some(host.to_string()), port, pq: tail.to_string() }
    }
}

/// Percent-encode a query with the URL encoder (`#` is escaped first: it would end the query).
fn query_encode(v: &str) -> Result<String, UErr> {
    let u = Url::parse(&format!("http://h/?{}", clean(v).replace('#', "%23"))).map_err(|_| UErr::Invalid)?;
    Ok(u.path.split_once('?').map(|x| x.1.to_string()).unwrap_or_default())
}

impl Rec {
    /// A record made by `parse_any` for a scheme other than `http` and `https` (see `Alt::into_url`).
    fn is_alt(&self) -> bool {
        self.u.port == 0 && self.u.host.is_empty()
    }
    fn alt(&self) -> Option<Alt> {
        self.is_alt().then(|| Alt::from_href(&self.u.path))
    }
    pub fn href(&self) -> String {
        let mut s = if self.is_alt() { self.u.path.clone() } else { self.u.to_string() };
        if let Some(info) = &self.userinfo {
            if !info.0.is_empty() || !info.1.is_empty() {
                let auth = if info.1.is_empty() { info.0.clone() } else { format!("{}:{}", info.0, info.1) };
                s = s.replacen("://", &format!("://{auth}@"), 1);
            }
        }
        if let Some(f) = &self.frag {
            s.push('#');
            s.push_str(f);
        }
        s
    }
    pub fn protocol(&self) -> String {
        if self.is_alt() {
            return self.alt().map_or_else(String::new, |a| format!("{}:", a.scheme));
        }
        format!("{}:", self.u.scheme)
    }
    pub fn hostname(&self) -> String {
        if self.is_alt() {
            return self.alt().and_then(|a| a.host).unwrap_or_default();
        }
        if self.u.host.contains(':') { format!("[{}]", self.u.host) } else { self.u.host.clone() }
    }
    pub fn port(&self) -> String {
        if self.is_alt() {
            return self.alt().and_then(|a| a.port).map_or(String::new(), |p| p.to_string());
        }
        if self.u.port == self.u.scheme.default_port() { String::new() } else { self.u.port.to_string() }
    }
    pub fn host(&self) -> String {
        let p = self.port();
        if p.is_empty() { self.hostname() } else { format!("{}:{p}", self.hostname()) }
    }
    pub fn origin(&self) -> String {
        if self.is_alt() {
            match self.alt() {
                // `blob:` has the origin of the URL after it.
                Some(a) if a.scheme == "blob" => return parse(&a.pq, None).map_or_else(|_| "null".into(), |r| r.origin()),
                Some(a) if special_port(&a.scheme).is_none() || a.host.is_none() => return "null".into(),
                _ => return format!("{}//{}", self.protocol(), self.host()),
            }
        }
        let u = &self.u;
        let (open, close) = if u.host.contains(':') { ("[", "]") } else { ("", "") };
        if u.port == u.scheme.default_port() { format!("{}://{open}{}{close}", u.scheme, u.host) } else { format!("{}://{open}{}{close}:{}", u.scheme, u.host, u.port) }
    }
    /// The path and the query: `/a/b?q`.
    fn pq(&self) -> &str {
        let p = &self.u.path;
        if !self.is_alt() {
            return p;
        }
        let r = &p[p.find(':').map_or(0, |i| i + 1)..];
        match r.strip_prefix("//") {
            Some(a) => &a[a.find(['/', '?']).unwrap_or(a.len())..],
            None => r,
        }
    }
    pub fn pathname(&self) -> &str {
        self.pq().split('?').next().unwrap_or("/")
    }
    /// The query without `?`. `Some("")` for a bare `?`.
    pub fn query(&self) -> Option<&str> {
        self.pq().split_once('?').map(|x| x.1)
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
        let pq = match q {
            Some(q) => format!("{p}?{q}"),
            None => p,
        };
        if self.is_alt() {
            let mut a = Alt::from_href(&self.u.path);
            a.pq = pq;
            self.u = a.into_url();
        } else {
            self.u.path = pq;
        }
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
        if self.is_alt() {
            let mut a = Alt::from_href(&self.u.path);
            if a.pq.starts_with('/') || a.host.is_some() {
                a.pq = norm(&p)?;
                self.u = a.into_url();
            }
        } else {
            self.u = self.u.join(&p).map_err(|_| UErr::Invalid)?;
        }
        self.set_query(q.as_deref());
        Ok(())
    }
    /// Set the host (and the port when `v` has one). An invalid value leaves the URL as it was.
    pub fn set_host(&mut self, v: &str, with_port: bool) -> Result<(), UErr> {
        if self.is_alt() {
            return Ok(());
        }
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
        if self.is_alt() {
            return;
        }
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
        if self.is_alt() {
            return Ok(());
        }
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
    Username,
    Password,
}

impl Part {
    pub fn get(self, r: &Rec) -> String {
        match self {
            Part::Username => r.userinfo.as_ref().map_or(String::new(), |i| i.0.clone()),
            Part::Password => r.userinfo.as_ref().map_or(String::new(), |i| i.1.clone()),
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
            Part::Username | Part::Password => {
                if !r.is_alt() {
                    let info = r.userinfo.get_or_insert_with(|| Box::new((String::new(), String::new())));
                    if self == Part::Username { info.0 = userinfo_encode(v); } else { info.1 = userinfo_encode(v); }
                }
                Ok(())
            }
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

fn userinfo_encode(s: &str) -> String {
    let mut out = String::new();
    for b in clean(s).bytes() {
        if b <= b' ' || b >= 0x7f || b"\"#<>?`{}/:;=@[\\]^|".contains(&b) {
            out.push_str(&format!("%{b:02X}"));
        } else { out.push(b as char); }
    }
    out
}

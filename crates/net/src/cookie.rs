//! A cookie jar: RFC 6265 and its draft update (6265bis), with the details that headless Chrome 155 shows
//! (`tools/fixtures/ui/cookie-cases.txt`, `chrome-cookie.txt`). One jar serves the network (`Set-Cookie` in, `Cookie` out, see `http.rs`)
//! and `document.cookie` (`browser_cookie.rs` in `bare`). It is in memory only.
//!
//! Setting (a `Set-Cookie` header or `document.cookie = text`):
//! - A string with a control character other than TAB (NUL, CR, LF, ...) is ignored. The name and the value are trimmed of spaces and
//!   TABs. A string with no `=` is a cookie with an empty name (`novalue` is read back as `novalue`). The name and the value may
//!   be 4096 bytes together; an attribute value over 1024 bytes is ignored.
//! - Attributes (names in any case): `Expires` (the date of RFC 6265 5.1.1; a bad date is ignored), `Max-Age` (`-`? digits, and it wins over
//!   `Expires`; zero or less deletes), `Domain` (one leading dot is dropped; the host must be the domain or a sub-domain; a public suffix, a
//!   domain with one label or an IP address is accepted only when it is the host itself, and then the cookie is host-only), `Path` (must
//!   start with `/`, else the default path of the URL), `Secure` (refused where the context is not secure), `HttpOnly` (a script cannot make
//!   one: the cookie is ignored), `SameSite` (`Strict`, `Lax`, `None`; another value is "not set", which acts as `Lax`; `None` needs
//!   `Secure`). `Partitioned` and any other attribute are ignored. The `__Secure-` and `__Host-` prefixes are enforced (case does not
//!   matter; `__Host-` needs an explicit `Path=/`).
//! - A new cookie with the name, domain, host-only flag and path of an old one replaces it and goes to the end of the list (Chrome gives
//!   it a new creation time). An expired cookie deletes the old one. An insecure page cannot replace a `Secure` cookie. A script cannot
//!   replace an `HttpOnly` cookie. A time more than 400 days away is cut to 400 days.
//! - A response to a cross-site subresource or `fetch` (`Via::Http { third: true }`) sets only `SameSite=None` cookies.
//! - Over 180 cookies for one domain, the ones that were used the longest ago go until 150 are left (Chrome: the same numbers). Over 3000
//!   cookies, the one used the longest ago goes. "Used" is the time of the last `Cookie` header or `document.cookie` read that held it.
//!
//! Sending (`header`, `script_get`): the cookies whose domain and path match, with `Secure` ones only in a secure context, and by `Party`
//! the `SameSite` rule. Longer paths first and then in order of creation. A host with no cookies costs one hash lookup.
//!
//! Limits: the public suffix list is short (every one-label name, and the common two-label suffixes in `multi_label_suffix`), not the full
//! list; a suffix with three labels (`k12.ca.us`) is not known. The count per domain is for each host name or domain, not each site. The date
//! parser follows RFC 6265; Chrome reads a few odd dates differently (`1 Jan 2000 00:00:00:00`, `000:00:00`).

use bare_url::{Scheme, Url};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_NAME_VALUE: usize = 4096;
const MAX_ATTR: usize = 1024;
const MAX_PER_DOMAIN: usize = 180;
const KEEP_PER_DOMAIN: usize = 150;
const MAX_TOTAL: usize = 3000;
const MAX_AGE_MS: f64 = 400.0 * 86_400_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    /// Not set (or a value that is not known). It acts as `Lax`.
    Unset,
    None,
    Lax,
    Strict,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The host for a host-only cookie, else the `Domain` attribute (lowercase, no dot).
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Milliseconds since 1970. `None`: a session cookie.
    pub expires: Option<f64>,
    /// The tick of the creation, or of the last time that the cookie was sent or read. The smallest goes first when the jar is full.
    used: u64,
}

/// Where a cookie is set or sent: the URL of the document or request, as the jar needs it.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    /// A secure context: `https`, or a loopback host (Chrome sends `Secure` cookies to `localhost` over `http`).
    pub secure: bool,
    pub host: &'a str,
    /// The path, with no query and no fragment.
    pub path: &'a str,
}

impl<'a> At<'a> {
    pub fn of(url: &'a Url) -> At<'a> {
        At { secure: url.scheme == Scheme::Https || loopback(&url.host), host: &url.host, path: url.path.split(['?', '#']).next().unwrap_or("/") }
    }
}

/// A host that is a secure context even over `http`.
pub fn loopback(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "::1" || host == "[::1]" || host.strip_prefix("127.").is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// How the request relates to the document, for `SameSite`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    /// Same site (or a user action): every cookie.
    First,
    /// A cross-site top-level navigation with a safe method: `Lax` and `None` cookies (and the ones with no `SameSite`).
    Nav,
    /// A cross-site subresource, `fetch` or XHR (or a cross-site navigation that is not safe): only `SameSite=None`.
    Third,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `document.cookie`.
    Script,
    /// A `Set-Cookie` header. `third`: the request is cross-site and is not a navigation.
    Http { third: bool },
}

#[derive(Default)]
pub struct Jar {
    /// The cookies by the site (registrable domain) of their domain, each list in order of creation.
    map: HashMap<String, Vec<Cookie>>,
    tick: u64,
    /// Added to the clock of `now`. A test moves time with it.
    pub skew_ms: f64,
}

fn is_ip(host: &str) -> bool {
    host.starts_with('[') || host.contains(':') || host.bytes().all(|b| b.is_ascii_digit() || b == b'.') && host.contains('.')
}

/// The two-label public suffixes that the jar knows. A short list of the common ones, not the Public Suffix List.
fn multi_label_suffix(s: &str) -> bool {
    matches!(
        s,
        "co.uk" | "org.uk" | "ac.uk" | "gov.uk" | "me.uk" | "ltd.uk" | "plc.uk" | "net.uk" | "sch.uk"
            | "com.au" | "net.au" | "org.au" | "edu.au" | "gov.au" | "id.au"
            | "co.nz" | "org.nz" | "net.nz" | "govt.nz" | "ac.nz"
            | "co.jp" | "or.jp" | "ne.jp" | "ac.jp" | "go.jp"
            | "co.in" | "net.in" | "org.in" | "gov.in" | "ac.in" | "firm.in"
            | "com.br" | "net.br" | "org.br" | "gov.br" | "edu.br"
            | "com.cn" | "net.cn" | "org.cn" | "gov.cn" | "edu.cn"
            | "com.hk" | "com.tw" | "com.sg" | "com.my" | "com.ph" | "com.vn" | "com.tr" | "com.mx" | "com.ar" | "com.co" | "com.pe" | "com.ua" | "com.ng" | "com.eg" | "com.pk" | "com.bd" | "com.sa"
            | "co.za" | "co.ke" | "co.il" | "co.kr" | "co.id" | "co.th" | "co.ug" | "co.tz" | "co.zw" | "co.ao"
            | "github.io" | "gitlab.io" | "githubusercontent.com" | "herokuapp.com" | "vercel.app" | "netlify.app" | "pages.dev" | "workers.dev"
            | "blogspot.com" | "appspot.com" | "web.app" | "firebaseapp.com" | "cloudfront.net" | "azurewebsites.net" | "amazonaws.com"
            | "fly.dev" | "onrender.com" | "surge.sh" | "glitch.me" | "repl.co" | "ngrok.io" | "myshopify.com" | "wordpress.com" | "tumblr.com"
    )
}

/// A name that no cookie may name as its `Domain` (unless it is the host itself): a public suffix, an IP address or a one-label name.
fn public(domain: &str) -> bool {
    !domain.contains('.') || is_ip(domain) || multi_label_suffix(domain)
}

/// The site of a host: its registrable domain (the public suffix and one label more). An IP address, a name with one label and a
/// public suffix are their own site. This is the key of the jar, and the "site" of `SameSite`.
pub fn site(host: &str) -> &str {
    if !host.contains('.') || is_ip(host) {
        return host;
    }
    let mut dots = host.rmatch_indices('.').map(|(i, _)| i);
    let _last = dots.next();
    let Some(d2) = dots.next() else { return host };
    if multi_label_suffix(&host[d2 + 1..]) {
        return dots.next().map_or(host, |d3| &host[d3 + 1..]);
    }
    &host[d2 + 1..]
}

fn scheme(s: Scheme) -> &'static str {
    match s {
        Scheme::Http => "http",
        Scheme::Https => "https",
    }
}

/// The site of a URL as `SameSite` sees it: the scheme and the registrable domain, like `https://example.com`.
pub fn site_of(url: &Url) -> String {
    format!("{}://{}", scheme(url.scheme), site(&url.host))
}

/// Whether `url` is in the site `doc` (made by `site_of`), with no allocation.
pub fn same_site(doc: &str, url: &Url) -> bool {
    doc.strip_prefix(scheme(url.scheme)).and_then(|r| r.strip_prefix("://")) == Some(site(&url.host))
}

fn domain_match(host: &str, domain: &str) -> bool {
    host == domain || host.len() > domain.len() && host.ends_with(domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

fn path_match(req: &str, cookie: &str) -> bool {
    req == cookie || req.starts_with(cookie) && (cookie.ends_with('/') || req.as_bytes()[cookie.len()] == b'/')
}

/// The default path of RFC 6265 5.1.4.
fn default_path(path: &str) -> String {
    match path.rfind('/') {
        Some(i) if i > 0 && path.starts_with('/') => path[..i].to_string(),
        _ => "/".to_string(),
    }
}

fn trim(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

impl Cookie {
    fn matches(&self, host: &str, secure: bool, path: &str) -> bool {
        (!self.secure || secure) && if self.host_only { self.domain == host } else { domain_match(host, &self.domain) } && path_match(path, &self.path)
    }

    fn same_key(&self, o: &Cookie) -> bool {
        self.name == o.name && self.domain == o.domain && self.host_only == o.host_only && self.path == o.path
    }

    fn allowed(&self, party: Party) -> bool {
        match party {
            Party::First => true,
            Party::Nav => self.same_site != SameSite::Strict,
            Party::Third => self.same_site == SameSite::None,
        }
    }
}

impl Jar {
    /// The wall clock in ms since 1970, moved by `skew_ms`.
    pub fn now(&self) -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64) + self.skew_ms
    }

    pub fn len(&self) -> usize {
        self.map.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// All cookies (also the expired ones that nothing has swept), by domain, name and path. For a dump: it never gives a decision about sending.
    pub fn list(&self) -> Vec<&Cookie> {
        let mut all: Vec<&Cookie> = self.map.values().flatten().collect();
        all.sort_by(|a, b| (&a.domain, &a.name, &a.path).cmp(&(&b.domain, &b.name, &b.path)));
        all
    }

    /// Store one cookie from `text`, a `Set-Cookie` value or `document.cookie = text`, for a URL `at`. `now` is the wall clock in ms.
    pub fn set(&mut self, at: &At, text: &str, via: Via, now: f64) {
        if text.bytes().any(|b| b < 0x20 && b != b'\t' || b == 0x7f) {
            return;
        }
        let (nv, attrs) = text.split_once(';').unwrap_or((text, ""));
        let (name, value) = match nv.split_once('=') {
            Some((n, v)) => (trim(n), trim(v)),
            None => ("", trim(nv)),
        };
        if name.is_empty() && value.is_empty() || name.len() + value.len() > MAX_NAME_VALUE {
            return;
        }
        let (mut expires, mut max_age, mut domain, mut cpath, mut secure, mut http_only, mut same_site) = (None, None, None::<String>, None::<String>, false, false, SameSite::Unset);
        for a in attrs.split(';') {
            let (an, av) = a.split_once('=').map_or((trim(a), ""), |(n, v)| (trim(n), trim(v)));
            let big = av.len() > MAX_ATTR;
            match an.to_ascii_lowercase().as_str() {
                "expires" => expires = parse_date(av).or(expires),
                "max-age" => {
                    let digits = av.strip_prefix('-').unwrap_or(av);
                    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                        max_age = Some(if av.starts_with('-') { f64::NEG_INFINITY } else { av.parse::<f64>().unwrap_or(f64::INFINITY) });
                    }
                }
                "domain" if !big && !av.is_empty() => domain = Some(av.strip_prefix('.').unwrap_or(av).to_ascii_lowercase()),
                "path" if !big => cpath = Some(av.to_string()),
                "secure" => secure = true,
                "httponly" => http_only = true,
                "samesite" => {
                    same_site = match av.to_ascii_lowercase().as_str() {
                        "none" => SameSite::None,
                        "lax" => SameSite::Lax,
                        "strict" => SameSite::Strict,
                        _ => SameSite::Unset,
                    }
                }
                _ => {}
            }
        }
        // A script cannot make an HttpOnly cookie. `SameSite=None` needs `Secure`. A `Secure` cookie needs a secure context.
        // A response to a cross-site subresource may set `SameSite=None` cookies only.
        if http_only && via == Via::Script || same_site == SameSite::None && !secure || secure && !at.secure || matches!(via, Via::Http { third: true }) && same_site != SameSite::None {
            return;
        }
        let host = at.host;
        let (domain, host_only) = match domain {
            // A public suffix, a one-label name or an IP address may be a domain only for the host itself, and then the cookie is host-only.
            Some(d) if public(&d) || is_ip(host) => {
                if d == host {
                    (d, true)
                } else {
                    return;
                }
            }
            Some(d) if !domain_match(host, &d) => return,
            Some(d) => (d, false),
            None => (host.to_string(), true),
        };
        let root_path = cpath.as_deref() == Some("/");
        let path = match cpath {
            Some(p) if p.starts_with('/') => p,
            _ => default_path(at.path),
        };
        // A cookie with no name has its prefix in the value.
        let pfx = if name.is_empty() { value } else { name };
        let prefixed = |p: &str| pfx.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p));
        if prefixed("__Secure-") && !(secure && at.secure) || prefixed("__Host-") && !(secure && at.secure && host_only && root_path) {
            return;
        }
        let expires = match (max_age, expires) {
            (Some(m), _) => Some(now + (m * 1000.0).min(MAX_AGE_MS)),
            (None, Some(e)) => Some(e.min(now + MAX_AGE_MS)),
            (None, None) => None,
        };
        self.tick += 1;
        let c = Cookie { name: name.into(), value: value.into(), domain, host_only, path, secure, http_only, same_site, expires, used: self.tick };
        let key = site(&c.domain).to_string();
        let list = self.map.entry(key.clone()).or_default();
        // An insecure page must not replace a `Secure` cookie that it could not read, and a script must not replace an HttpOnly one.
        let blocked = list.iter().any(|o| {
            let hides = o.secure && !at.secure && o.name == c.name && (if o.host_only { o.domain == host } else { domain_match(host, &o.domain) }) && path_match(&c.path, &o.path);
            hides || via == Via::Script && o.http_only && o.same_key(&c)
        });
        if blocked {
            if list.is_empty() {
                self.map.remove(&key);
            }
            return;
        }
        list.retain(|o| !o.same_key(&c));
        if expires.is_some_and(|e| e <= now) {
            if list.is_empty() {
                self.map.remove(&key);
            }
            return;
        }
        let domain = c.domain.clone();
        list.push(c);
        let n = list.iter().filter(|o| o.domain == domain).count();
        if n > MAX_PER_DOMAIN {
            // The ones used the longest ago go, until `KEEP_PER_DOMAIN` are left.
            let mut order: Vec<(u64, usize)> = list.iter().enumerate().filter(|(_, o)| o.domain == domain).map(|(i, o)| (o.used, i)).collect();
            order.sort_unstable();
            let mut gone = vec![false; list.len()];
            for &(_, i) in &order[..n - KEEP_PER_DOMAIN] {
                gone[i] = true;
            }
            let mut i = 0;
            list.retain(|_| {
                i += 1;
                !gone[i - 1]
            });
        }
        while self.len() > MAX_TOTAL {
            let oldest = self.map.iter().flat_map(|(k, l)| l.iter().enumerate().map(move |(i, c)| (c.used, k, i))).min().map(|(_, k, i)| (k.clone(), i));
            let Some((k, i)) = oldest else { break };
            let l = self.map.get_mut(&k).expect("the key was just found");
            l.remove(i);
            if l.is_empty() {
                self.map.remove(&k);
            }
        }
    }

    /// The cookies to send, in the order of the spec: longer paths first, then in order of creation. `script`: the ones that
    /// `document.cookie` shows (no HttpOnly). Marks them as used.
    fn pick(&mut self, at: &At, party: Party, script: bool, now: f64) -> Vec<(String, String)> {
        if self.map.is_empty() {
            return Vec::new();
        }
        let key = site(at.host);
        let Some(list) = self.map.get_mut(key) else { return Vec::new() };
        list.retain(|c| c.expires.is_none_or(|e| e > now));
        if list.is_empty() {
            self.map.remove(key);
            return Vec::new();
        }
        self.tick += 1;
        let tick = self.tick;
        let mut hit: Vec<&mut Cookie> = list.iter_mut().filter(|c| !(script && c.http_only) && c.allowed(party) && c.matches(at.host, at.secure, at.path)).collect();
        hit.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        hit.into_iter()
            .map(|c| {
                c.used = tick;
                (c.name.clone(), c.value.clone())
            })
            .collect()
    }

    /// The cookie named `name` that a request to `at` would send (domain, path, `Secure` and time; no `SameSite` rule, and `HttpOnly` counts),
    /// the one with the longest path first. For a test driver: it does not mark the cookie as used.
    pub fn lookup(&self, at: &At, name: &str, now: f64) -> Option<&Cookie> {
        let list = self.map.get(site(at.host))?;
        list.iter().filter(|c| c.name == name && c.expires.is_none_or(|e| e > now) && c.matches(at.host, at.secure, at.path)).max_by_key(|c| c.path.len())
    }

    /// The value of the `Cookie` request header for `at`, or `None` when no cookie goes.
    pub fn header(&mut self, at: &At, party: Party, now: f64) -> Option<String> {
        let hit = self.pick(at, party, false, now);
        (!hit.is_empty()).then(|| join(&hit))
    }

    /// `document.cookie` for a document at `at`: no HttpOnly cookie.
    pub fn script_get(&mut self, at: &At, now: f64) -> String {
        join(&self.pick(at, Party::First, true, now))
    }
}

/// `name=value; name=value`. A cookie with no name gives only its value.
fn join(hit: &[(String, String)]) -> String {
    let mut out = String::new();
    for (n, v) in hit {
        if !out.is_empty() {
            out.push_str("; ");
        }
        if !n.is_empty() {
            out.push_str(n);
            out.push('=');
        }
        out.push_str(v);
    }
    out
}

/// The cookie date of RFC 6265 5.1.1, in ms since 1970. `None`: not a date. A year before 1601 gives 0 (long ago), as in Chrome.
fn parse_date(s: &str) -> Option<f64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for tok in s.split(|c: char| c.is_ascii() && !c.is_ascii_alphanumeric() && c != ':') {
        let digits = tok.bytes().take_while(u8::is_ascii_digit).count();
        let at_end = |n: usize| tok.as_bytes().get(n).is_none_or(|b| !b.is_ascii_digit());
        if time.is_none() && let Some(t) = parse_time(tok) {
            time = Some(t);
        } else if day.is_none() && (1..=2).contains(&digits) && at_end(digits) {
            day = tok[..digits].parse::<u32>().ok();
        } else if month.is_none() && tok.len() >= 3 && tok.is_char_boundary(3) && let Some(m) = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|m| tok[..3].eq_ignore_ascii_case(m)) {
            month = Some(m as u32 + 1);
        } else if year.is_none() && (2..=4).contains(&digits) && at_end(digits) {
            year = tok[..digits].parse::<i64>().ok().map(|y| match y {
                _ if digits > 2 => y,
                70..=99 => y + 1900,
                _ => y + 2000,
            });
        }
    }
    let ((h, mi, sec), day, month, year) = (time?, day?, month?, year?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month as usize - 1];
    if !(1..=dim).contains(&day) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    if year < 1601 {
        return Some(0.0);
    }
    // days since 1970-01-01 (Howard Hinnant's `days_from_civil`)
    let (y, m) = if month <= 2 { (year - 1, month as i64 + 9) } else { (year, month as i64 - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(((days * 24 + h as i64) * 60 + mi as i64) as f64 * 60_000.0 + sec as f64 * 1000.0)
}

/// `hh:mm:ss` with 1 or 2 digits each, then nothing or a non-digit.
fn parse_time(tok: &str) -> Option<(u32, u32, u32)> {
    let mut p = tok.splitn(3, ':');
    let mut field = |last: bool| {
        let f = p.next()?;
        let n = f.bytes().take_while(u8::is_ascii_digit).count();
        (1..=2).contains(&n).then_some(())?;
        (n == f.len() || last && !f.as_bytes()[n].is_ascii_digit()).then_some(())?;
        f[..n].parse::<u32>().ok()
    };
    Some((field(false)?, field(false)?, field(true)?))
}

#[cfg(test)]
mod tests;

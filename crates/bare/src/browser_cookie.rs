//! `document.cookie`: a jar of the cookies that scripts set. RFC 6265 and its draft update (6265bis), with the details that headless
//! Chrome 155 shows (`tools/fixtures/ui/cookie-cases.txt`, `chrome-cookie.txt`).
//!
//! The jar is in memory and lives in a [`crate::browser_navigation_storage::WebStorage`] (so a reload keeps it, and a new tab shares it).
//! It is not connected to the network: a `Set-Cookie` header of a response is not stored and no `Cookie` header is sent
//! (`docs/decisions.md`, "Cookies"). So `document.cookie` shows the cookies of scripts only.
//!
//! Setter rules (all checked against Chrome):
//! - A string with a control character other than TAB (NUL, CR, LF, ...) is ignored. The name and the value are trimmed of spaces and
//!   TABs. A string with no `=` is a cookie with an empty name (`novalue` is read back as `novalue`). The name and the value may
//!   be 4096 bytes together; an attribute value over 1024 bytes is ignored.
//! - Attributes (names in any case): `Expires` (the date of RFC 6265 5.1.1; a bad date is ignored), `Max-Age` (`-`? digits, and it wins over
//!   `Expires`; zero or less deletes), `Domain` (one leading dot is dropped; the host must be the domain or a sub-domain; a domain with
//!   one label, or an IP address, is accepted only when it is the host itself, and then the cookie is host-only), `Path` (must start with
//!   `/`, else the default path of the document), `Secure` (refused on `http`), `HttpOnly` (a script cannot make one: the cookie is
//!   ignored), `SameSite` (`None` needs `Secure`). The `__Secure-` and `__Host-` prefixes are enforced (case does not matter; `__Host-` needs an explicit `Path=/`).
//! - A new cookie with the name, domain, host-only flag and path of an old one replaces it and goes to the end of the list (Chrome gives
//!   it a new creation time). An expired cookie deletes the old one. An `http` page cannot replace a `Secure` cookie. A time more than 400
//!   days away is cut to 400 days.
//! - Over 180 cookies for one domain, the oldest go until 150 are left (Chrome: the same numbers). Over 3000 cookies, the oldest go.
//!
//! Getter: the cookies whose domain and path match the document, with `Secure` ones only on `https`, longer paths first and then in
//! order of creation, as `name=value; name=value` (a cookie with no name gives only its value).
//!
//! Gates: no public suffix list. Chrome refuses `Domain=co.uk` for a host `a.co.uk`; here a domain with two labels is accepted. The date
//! parser follows RFC 6265; Chrome reads a few odd dates differently (`1 Jan 2000 00:00:00:00`, `000:00:00`). The count per domain is
//! per host name, not per registrable domain.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

const MAX_NAME_VALUE: usize = 4096;
const MAX_ATTR: usize = 1024;
const MAX_PER_DOMAIN: usize = 180;
const KEEP_PER_DOMAIN: usize = 150;
const MAX_TOTAL: usize = 3000;
const MAX_AGE_MS: f64 = 400.0 * 86_400_000.0;

#[derive(Debug, PartialEq)]
pub(crate) struct Cookie {
    name: String,
    value: String,
    /// The host for a host-only cookie, else the `Domain` attribute (lowercase, no dot).
    domain: String,
    host_only: bool,
    path: String,
    secure: bool,
    /// Milliseconds since 1970. `None`: a session cookie.
    expires: Option<f64>,
}

/// The cookies, oldest first.
#[derive(Default)]
pub(crate) struct Jar {
    cookies: Vec<Cookie>,
}

fn is_ip(host: &str) -> bool {
    host.contains(':') || host.bytes().all(|b| b.is_ascii_digit() || b == b'.') && host.contains('.')
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
    fn matches(&self, host: &str, https: bool, path: &str) -> bool {
        (!self.secure || https) && if self.host_only { self.domain == host } else { domain_match(host, &self.domain) } && path_match(path, &self.path)
    }

    fn same_key(&self, o: &Cookie) -> bool {
        self.name == o.name && self.domain == o.domain && self.host_only == o.host_only && self.path == o.path
    }
}

impl Jar {
    pub(crate) fn len(&self) -> usize {
        self.cookies.len()
    }

    /// `document.cookie = text` for a document at `https`/`host`/`path`. `now` is the wall clock in ms.
    pub(crate) fn set(&mut self, https: bool, host: &str, path: &str, text: &str, now: f64) {
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
        let (mut expires, mut max_age, mut domain, mut cpath, mut secure) = (None, None, None::<String>, None::<String>, false);
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
                "httponly" => return,
                "samesite" if av.eq_ignore_ascii_case("none") && !secure_later(attrs) => return,
                _ => {}
            }
        }
        if secure && !https {
            return;
        }
        let (domain, host_only) = match domain {
            // A public suffix (here: one label) or an IP address may be a domain only for the host itself, and then the cookie is host-only.
            Some(d) if is_ip(host) || !d.contains('.') => if d == host { (d, true) } else { return },
            Some(d) if !domain_match(host, &d) => return,
            Some(d) => (d, false),
            None => (host.to_string(), true),
        };
        let root_path = cpath.as_deref() == Some("/");
        let path = match cpath {
            Some(p) if p.starts_with('/') => p,
            _ => default_path(path),
        };
        // A cookie with no name has its prefix in the value.
        let pfx = if name.is_empty() { value } else { name };
        let prefixed = |p: &str| pfx.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p));
        if prefixed("__Secure-") && !(secure && https) || prefixed("__Host-") && !(secure && https && host_only && root_path) {
            return;
        }
        let expires = match (max_age, expires) {
            (Some(m), _) => Some(now + (m * 1000.0).min(MAX_AGE_MS)),
            (None, Some(e)) => Some(e.min(now + MAX_AGE_MS)),
            (None, None) => None,
        };
        let c = Cookie { name: name.into(), value: value.into(), domain, host_only, path, secure, expires };
        // An `http` page must not replace a `Secure` cookie that it could not read.
        if !https && self.cookies.iter().any(|o| o.secure && o.name == c.name && (if o.host_only { o.domain == host } else { domain_match(host, &o.domain) }) && path_match(&c.path, &o.path)) {
            return;
        }
        self.cookies.retain(|o| !o.same_key(&c));
        if expires.is_some_and(|e| e <= now) {
            return;
        }
        let domain = c.domain.clone();
        self.cookies.push(c);
        let n = self.cookies.iter().filter(|o| o.domain == domain).count();
        if n > MAX_PER_DOMAIN {
            let mut drop = n - KEEP_PER_DOMAIN;
            self.cookies.retain(|o| o.domain != domain || { let keep = drop == 0; drop = drop.saturating_sub(1); keep });
        }
        if self.cookies.len() > MAX_TOTAL {
            self.cookies.drain(..self.cookies.len() - MAX_TOTAL);
        }
    }

    /// `document.cookie` for a document at `https`/`host`/`path`.
    pub(crate) fn get(&mut self, https: bool, host: &str, path: &str, now: f64) -> String {
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        let mut hit: Vec<&Cookie> = self.cookies.iter().filter(|c| c.matches(host, https, path)).collect();
        hit.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        let mut out = String::new();
        for c in hit {
            if !out.is_empty() {
                out.push_str("; ");
            }
            if !c.name.is_empty() {
                out.push_str(&c.name);
                out.push('=');
            }
            out.push_str(&c.value);
        }
        out
    }
}

/// `SameSite=None` needs `Secure` wherever it is in the attribute list.
fn secure_later(attrs: &str) -> bool {
    attrs.split(';').any(|a| trim(a).eq_ignore_ascii_case("secure"))
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

// ------------------------------------------------------------------------------------------ JS

/// `https`, the host and the path of the document, or `Unsupported` while the document URL is not configured.
fn doc(d: &Dom) -> R<(bool, String, String)> {
    let s = d.nav.s.borrow();
    let e = s.entries.get(s.cur).ok_or_else(|| Error::Unsupported(unsupported(crate::browser_navigation::NOURL)))?;
    Ok((e.url.protocol() == "https:", e.url.hostname(), e.url.pathname().to_string()))
}

pub(crate) fn doc_cookie_get(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let (https, host, path) = doc(d)?;
    let jar = d.nav.s.borrow().store.cookies();
    let s = jar.borrow_mut().get(https, &host, &path, it.wall_now_ms());
    d.js_str(it, &s)
}

pub(crate) fn doc_cookie_set(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let text = d.string_arg(it, a.first().copied().unwrap_or(Value::Undefined))?;
    let (https, host, path) = doc(d)?;
    let jar = d.nav.s.borrow().store.cookies();
    it.charge(1 + (jar.borrow().len() / 64) as u64)?;
    jar.borrow_mut().set(https, &host, &path, &text, it.wall_now_ms());
    Ok(Value::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_700_000_000_000.0;

    fn get(j: &mut Jar) -> String {
        j.get(true, "a.test", "/", NOW)
    }

    #[test]
    fn dates() {
        // 2000-01-01T00:00:00Z is 946684800 s.
        for s in ["Sat, 01 Jan 2000 00:00:00 GMT", "Sat, 1 Jan 2000 0:0:0 GMT", "Sat Jan  1 00:00:00 2000", "Saturday, 01-Jan-00 00:00:00 UTC", "1 JAN 00 00:00:00", "x 1 Jan 2000 00:00:00 1"] {
            assert_eq!(parse_date(s), Some(946_684_800_000.0), "{s}");
        }
        assert_eq!(parse_date("Sat, 29 Feb 2000 12:34:56 GMT"), Some(951_827_696_000.0));
        for s in ["2000-01-01", "1 Jan 2000 24:00:00", "1 Jan 2000 23:60:00", "1 Jan 2000 23:59:60", "32 Jan 2000 00:00:00", "31 Feb 2000 00:00:00", "29 Feb 2001 00:00:00", "1 Foo 2000 00:00:00", "1 Jan 2000 00:00", "1 Jan 10000 00:00:00", "Jan 2000 00:00:00", ""] {
            assert_eq!(parse_date(s), None, "{s}");
        }
        assert_eq!(parse_date("1 Jan 1600 00:00:00"), Some(0.0));
        assert_eq!(parse_date("1 Jan 69 00:00:00"), parse_date("1 Jan 2069 00:00:00"));
    }

    #[test]
    fn default_paths() {
        for (p, want) in [("/", "/"), ("/a", "/"), ("/a/", "/a"), ("/a/b", "/a"), ("/a/b/c", "/a/b"), ("", "/"), ("x", "/")] {
            assert_eq!(default_path(p), want, "{p}");
        }
    }

    #[test]
    fn replace_goes_to_the_end_and_expiry_deletes() {
        let mut j = Jar::default();
        for c in ["a=1", "b=2", "a=3", "c=4; Max-Age=0"] {
            j.set(true, "a.test", "/", c, NOW);
        }
        assert_eq!(get(&mut j), "b=2; a=3");
        j.set(true, "a.test", "/", "b=; Expires=Thu, 01 Jan 1970 00:00:00 GMT", NOW);
        assert_eq!(get(&mut j), "a=3");
        // A cookie ends when its time passes.
        j.set(true, "a.test", "/", "t=1; Max-Age=10", NOW);
        assert_eq!(j.get(true, "a.test", "/", NOW + 9_999.0), "a=3; t=1");
        assert_eq!(j.get(true, "a.test", "/", NOW + 10_000.0), "a=3");
    }

    #[test]
    fn scope_secure_and_paths() {
        let mut j = Jar::default();
        j.set(false, "a.test", "/d/page", "s=1; Secure", NOW);
        j.set(false, "a.test", "/d/page", "n=1", NOW);
        j.set(false, "a.test", "/d/page", "r=1; Path=/", NOW);
        j.set(false, "a.test", "/d/page", "x=1; Path=/other", NOW);
        assert_eq!(j.get(false, "a.test", "/d/page", NOW), "n=1; r=1");
        assert_eq!(j.get(false, "a.test", "/d/", NOW), "n=1; r=1");
        assert_eq!(j.get(false, "a.test", "/d", NOW), "n=1; r=1");
        assert_eq!(j.get(false, "a.test", "/dx", NOW), "r=1");
        assert_eq!(j.get(false, "b.test", "/d/page", NOW), "");
        // `http` cannot replace a Secure cookie
        j.set(true, "a.test", "/", "k=secure; Secure", NOW);
        j.set(false, "a.test", "/", "k=plain", NOW);
        assert_eq!(j.get(true, "a.test", "/", NOW), "r=1; k=secure");
        assert_eq!(j.get(false, "a.test", "/", NOW), "r=1");
    }

    #[test]
    fn domain_rules() {
        let mut j = Jar::default();
        for c in ["a=1; Domain=sub.a.test", "b=1; Domain=test", "c=1; Domain=.test", "d=1; Domain=co.uk", "e=1; Domain=b.test", "f=1; Domain=A.TEST", "g=1; Domain=.a.test"] {
            j.set(true, "a.test", "/", c, NOW);
        }
        assert_eq!(get(&mut j), "f=1; g=1");
        assert_eq!(j.get(true, "x.a.test", "/", NOW), "f=1; g=1");
        assert_eq!(j.get(true, "xa.test", "/", NOW), "");
        // A domain with one label or an IP address is allowed only for the host itself, and the cookie is host-only.
        let mut j = Jar::default();
        j.set(true, "localhost", "/", "a=1; Domain=localhost", NOW);
        j.set(true, "1.2.3.4", "/", "b=1; Domain=1.2.3.4", NOW);
        j.set(true, "1.2.3.4", "/", "c=1; Domain=2.3.4", NOW);
        assert_eq!((j.get(true, "localhost", "/", NOW), j.get(true, "1.2.3.4", "/", NOW), j.get(true, "x.localhost", "/", NOW)), ("a=1".into(), "b=1".into(), "".to_string()));
    }

    #[test]
    fn control_characters_prefixes_and_flags() {
        let mut j = Jar::default();
        for c in ["a=1\nb=2", "a=1;\0 b=2", "h=1; HttpOnly", "n=1; SameSite=None", "__Secure-s=1", "__secure-t=1", "__Host-h=1; Secure; Path=/x", "__Host-d=1; Secure; Domain=a.test; Path=/", "__Host-j=1; Secure", "=", "  ", "w=\t7\t", "ok=1; Foo=bar; ;; Secure ;"] {
            j.set(true, "a.test", "/", c, NOW);
        }
        assert_eq!(get(&mut j), "w=7; ok=1");
        for c in ["l=1; SameSite=None; Secure", "l2=1; Secure; SameSite=None", "m=1; SameSite=Strict", "__Secure-o=1; Secure", "__Host-p=1; Secure; Path=/", "novalue", "=v", "q=\"x y\"; Path=relative"] {
            j.set(true, "a.test", "/", c, NOW);
        }
        assert_eq!(get(&mut j), "w=7; ok=1; l=1; l2=1; m=1; __Secure-o=1; __Host-p=1; v; q=\"x y\"");
        // a prefix needs `https`, and a `Secure` cookie needs it too
        j.set(false, "a.test", "/", "__Secure-z=1; Secure", NOW);
        assert!(!j.get(true, "a.test", "/", NOW).contains("__Secure-z"));
    }

    #[test]
    fn sizes_and_counts() {
        let mut j = Jar::default();
        j.set(true, "a.test", "/", &format!("{}=1", "x".repeat(MAX_NAME_VALUE)), NOW);
        j.set(true, "a.test", "/", &format!("p={}", "x".repeat(MAX_NAME_VALUE - 1)), NOW);
        j.set(true, "a.test", "/", &format!("q={}", "x".repeat(MAX_NAME_VALUE)), NOW);
        j.set(true, "a.test", "/", &format!("r=1; Path=/{}", "x".repeat(MAX_ATTR)), NOW);
        assert_eq!(get(&mut j).split("; ").map(|c| c.split('=').next().unwrap().len()).collect::<Vec<_>>(), vec![1, 1]);
        // 200 cookies for one domain leave 169, the oldest 31 gone (the numbers of Chrome)
        let mut j = Jar::default();
        for i in 0..200 {
            j.set(true, "a.test", "/", &format!("k{i}=v"), NOW);
        }
        let all = get(&mut j);
        assert_eq!((all.split("; ").count(), all.starts_with("k31=v"), all.ends_with("k199=v")), (169, true, true));
    }
}

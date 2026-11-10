use super::*;

const NOW: f64 = 1_700_000_000_000.0;

fn at<'a>(https: bool, host: &'a str, path: &'a str) -> At<'a> {
    At { secure: https, host, path }
}

/// `document.cookie = text` at `https://host/path`.
fn script(j: &mut Jar, https: bool, host: &str, path: &str, text: &str) {
    j.set(&at(https, host, path), text, Via::Script, NOW);
}

/// A `Set-Cookie` header of a first-party response.
fn http(j: &mut Jar, https: bool, host: &str, path: &str, text: &str) {
    j.set(&at(https, host, path), text, Via::Http { third: false }, NOW);
}

fn get(j: &mut Jar) -> String {
    j.script_get(&at(true, "a.test", "/"), NOW)
}

fn sent(j: &mut Jar, https: bool, host: &str, path: &str, party: Party) -> String {
    j.header(&at(https, host, path), party, NOW).unwrap_or_default()
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
        script(&mut j, true, "a.test", "/", c);
    }
    assert_eq!(get(&mut j), "b=2; a=3");
    script(&mut j, true, "a.test", "/", "b=; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(get(&mut j), "a=3");
    // A cookie ends when its time passes.
    script(&mut j, true, "a.test", "/", "t=1; Max-Age=10");
    assert_eq!(j.script_get(&at(true, "a.test", "/"), NOW + 9_999.0), "a=3; t=1");
    assert_eq!(j.script_get(&at(true, "a.test", "/"), NOW + 10_000.0), "a=3");
}

#[test]
fn max_age_wins_over_expires_and_a_negative_one_deletes() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/", "a=1; Max-Age=100; Expires=Thu, 01 Jan 1970 00:00:00 GMT");
    http(&mut j, true, "a.test", "/", "b=1; Max-Age=0; Expires=Fri, 01 Jan 2100 00:00:00 GMT");
    http(&mut j, true, "a.test", "/", "c=1; Max-Age=-5");
    assert_eq!(get(&mut j), "a=1");
    // The old cookie goes when the new one is already dead.
    http(&mut j, true, "a.test", "/", "a=2; Max-Age=-1");
    assert_eq!(get(&mut j), "");
}

#[test]
fn scope_secure_and_paths() {
    let mut j = Jar::default();
    script(&mut j, false, "a.test", "/d/page", "s=1; Secure");
    script(&mut j, false, "a.test", "/d/page", "n=1");
    script(&mut j, false, "a.test", "/d/page", "r=1; Path=/");
    script(&mut j, false, "a.test", "/d/page", "x=1; Path=/other");
    let g = |j: &mut Jar, h: &str, p: &str| j.script_get(&at(false, h, p), NOW);
    assert_eq!(g(&mut j, "a.test", "/d/page"), "n=1; r=1");
    assert_eq!(g(&mut j, "a.test", "/d/"), "n=1; r=1");
    assert_eq!(g(&mut j, "a.test", "/d"), "n=1; r=1");
    assert_eq!(g(&mut j, "a.test", "/dx"), "r=1");
    assert_eq!(g(&mut j, "b.test", "/d/page"), "");
    // `http` cannot replace a Secure cookie
    script(&mut j, true, "a.test", "/", "k=secure; Secure");
    script(&mut j, false, "a.test", "/", "k=plain");
    assert_eq!(get(&mut j), "r=1; k=secure");
    assert_eq!(g(&mut j, "a.test", "/"), "r=1");
}

#[test]
fn domain_rules() {
    let mut j = Jar::default();
    for c in ["a=1; Domain=sub.a.test", "b=1; Domain=test", "c=1; Domain=.test", "d=1; Domain=co.uk", "e=1; Domain=b.test", "f=1; Domain=A.TEST", "g=1; Domain=.a.test"] {
        script(&mut j, true, "a.test", "/", c);
    }
    assert_eq!(get(&mut j), "f=1; g=1");
    assert_eq!(j.script_get(&at(true, "x.a.test", "/"), NOW), "f=1; g=1");
    assert_eq!(j.script_get(&at(true, "xa.test", "/"), NOW), "");
    // A domain with one label or an IP address is allowed only for the host itself, and the cookie is host-only.
    let mut j = Jar::default();
    script(&mut j, true, "localhost", "/", "a=1; Domain=localhost");
    script(&mut j, true, "1.2.3.4", "/", "b=1; Domain=1.2.3.4");
    script(&mut j, true, "1.2.3.4", "/", "c=1; Domain=2.3.4");
    let g = |j: &mut Jar, h: &str| j.script_get(&at(true, h, "/"), NOW);
    assert_eq!((g(&mut j, "localhost"), g(&mut j, "1.2.3.4"), g(&mut j, "x.localhost")), ("a=1".into(), "b=1".into(), "".to_string()));
}

#[test]
fn public_suffixes_cannot_be_a_domain() {
    let mut j = Jar::default();
    for (host, dom) in [("a.co.uk", "co.uk"), ("a.com.au", "com.au"), ("me.github.io", "github.io"), ("a.com", "com")] {
        http(&mut j, true, host, "/", &format!("bad=1; Domain={dom}"));
    }
    assert_eq!(j.len(), 0);
    // The suffix itself is no host-only trap: the domain below it is fine, and the host that is a suffix may set a host-only cookie.
    http(&mut j, true, "a.co.uk", "/", "ok=1; Domain=a.co.uk");
    http(&mut j, true, "co.uk", "/", "own=1; Domain=co.uk");
    assert_eq!(sent(&mut j, true, "b.a.co.uk", "/", Party::First), "ok=1");
    assert_eq!(sent(&mut j, true, "x.co.uk", "/", Party::First), "");
    assert_eq!(sent(&mut j, true, "co.uk", "/", Party::First), "own=1");
    assert_eq!(sent(&mut j, true, "sub.co.uk", "/", Party::First), "");
}

#[test]
fn site_is_the_registrable_domain() {
    for (host, want) in [
        ("www.example.com", "example.com"),
        ("example.com", "example.com"),
        ("a.b.example.co.uk", "example.co.uk"),
        ("me.github.io", "me.github.io"),
        ("x.me.github.io", "me.github.io"),
        ("localhost", "localhost"),
        ("127.0.0.1", "127.0.0.1"),
        ("[::1]", "[::1]"),
        ("co.uk", "co.uk"),
        ("a.test", "a.test"),
        ("x.a.test", "a.test"),
    ] {
        assert_eq!(site(host), want, "{host}");
    }
}

#[test]
fn control_characters_prefixes_and_flags() {
    let mut j = Jar::default();
    for c in ["a=1\nb=2", "a=1;\0 b=2", "h=1; HttpOnly", "n=1; SameSite=None", "__Secure-s=1", "__secure-t=1", "__Host-h=1; Secure; Path=/x", "__Host-d=1; Secure; Domain=a.test; Path=/", "__Host-j=1; Secure", "=", "  ", "w=\t7\t", "ok=1; Foo=bar; ;; Secure ;"] {
        script(&mut j, true, "a.test", "/", c);
    }
    assert_eq!(get(&mut j), "w=7; ok=1");
    for c in ["l=1; SameSite=None; Secure", "l2=1; Secure; SameSite=None", "m=1; SameSite=Strict", "__Secure-o=1; Secure", "__Host-p=1; Secure; Path=/", "novalue", "=v", "q=\"x y\"; Path=relative"] {
        script(&mut j, true, "a.test", "/", c);
    }
    assert_eq!(get(&mut j), "w=7; ok=1; l=1; l2=1; m=1; __Secure-o=1; __Host-p=1; v; q=\"x y\"");
    // a prefix needs `https`, and a `Secure` cookie needs it too
    script(&mut j, false, "a.test", "/", "__Secure-z=1; Secure");
    assert!(!get(&mut j).contains("__Secure-z"));
}

#[test]
fn sizes_and_counts() {
    let mut j = Jar::default();
    script(&mut j, true, "a.test", "/", &format!("{}=1", "x".repeat(MAX_NAME_VALUE)));
    script(&mut j, true, "a.test", "/", &format!("p={}", "x".repeat(MAX_NAME_VALUE - 1)));
    script(&mut j, true, "a.test", "/", &format!("q={}", "x".repeat(MAX_NAME_VALUE)));
    script(&mut j, true, "a.test", "/", &format!("r=1; Path=/{}", "x".repeat(MAX_ATTR)));
    assert_eq!(get(&mut j).split("; ").map(|c| c.split('=').next().unwrap().len()).collect::<Vec<_>>(), vec![1, 1]);
    // 200 cookies for one domain leave 169, the oldest 31 gone (the numbers of Chrome)
    let mut j = Jar::default();
    for i in 0..200 {
        script(&mut j, true, "a.test", "/", &format!("k{i}=v"));
    }
    let all = get(&mut j);
    assert_eq!((all.split("; ").count(), all.starts_with("k31=v"), all.ends_with("k199=v")), (169, true, true));
}

#[test]
fn the_cookie_that_was_used_last_stays() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/", "old=v; Path=/old");
    for i in 0..179 {
        http(&mut j, true, "a.test", "/", &format!("k{i}=v; Path=/k"));
    }
    assert_eq!(j.len(), 180);
    // Only `old` matches this request, so only `old` is used now.
    assert_eq!(sent(&mut j, true, "a.test", "/old", Party::First), "old=v");
    http(&mut j, true, "a.test", "/", "new=v");
    // 181 cookies are over the limit of 180: 31 go and 150 stay. The ones not used for the longest time go first, not the oldest.
    assert_eq!(j.len(), 150);
    let names: Vec<_> = j.list().iter().map(|c| c.name.clone()).collect();
    assert!(names.contains(&"old".to_string()) && names.contains(&"new".to_string()));
    assert!(!names.contains(&"k0".to_string()) && !names.contains(&"k30".to_string()) && names.contains(&"k31".to_string()));
}

#[test]
fn the_jar_holds_3000_cookies() {
    let mut j = Jar::default();
    for i in 0..3100 {
        http(&mut j, true, &format!("h{i}.test"), "/", "k=v");
    }
    assert_eq!(j.len(), 3000);
}

#[test]
fn http_only_is_kept_from_scripts() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/", "sid=secret; HttpOnly");
    http(&mut j, true, "a.test", "/", "vis=1");
    assert_eq!(get(&mut j), "vis=1");
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "sid=secret; vis=1");
    // A script can neither make one nor replace one.
    script(&mut j, true, "a.test", "/", "sid=mine");
    script(&mut j, true, "a.test", "/", "sid=; Max-Age=0");
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "sid=secret; vis=1");
    http(&mut j, true, "a.test", "/", "sid=new; HttpOnly");
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "vis=1; sid=new");
    assert!(j.list().iter().any(|c| c.name == "sid" && c.http_only));
}

#[test]
fn secure_is_never_sent_over_http_but_loopback_counts_as_secure() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/", "s=1; Secure");
    http(&mut j, true, "a.test", "/", "p=1");
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "s=1; p=1");
    assert_eq!(sent(&mut j, false, "a.test", "/", Party::First), "p=1");
    // Over http, a Secure attribute is refused, except where the caller says the context is secure (loopback).
    http(&mut j, false, "a.test", "/", "t=1; Secure");
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "s=1; p=1");
}

#[test]
fn same_site_rules() {
    let mut j = Jar::default();
    for c in ["strict=1; SameSite=Strict", "lax=1; SameSite=Lax", "unset=1", "none=1; SameSite=None; Secure", "odd=1; SameSite=bogus"] {
        http(&mut j, true, "a.test", "/", c);
    }
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::First), "strict=1; lax=1; unset=1; none=1; odd=1");
    // A cross-site subresource or fetch gets only SameSite=None.
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::Third), "none=1");
    // A cross-site top-level navigation (a safe method) gets Lax and the default too, and never Strict.
    assert_eq!(sent(&mut j, true, "a.test", "/", Party::Nav), "lax=1; unset=1; none=1; odd=1");
}

#[test]
fn a_cross_site_response_sets_only_same_site_none() {
    let mut j = Jar::default();
    for c in ["a=1", "b=1; SameSite=Lax", "c=1; SameSite=Strict", "d=1; SameSite=None; Secure", "e=1; SameSite=None"] {
        j.set(&at(true, "a.test", "/"), c, Via::Http { third: true }, NOW);
    }
    assert_eq!(get(&mut j), "d=1");
}

#[test]
fn default_and_explicit_ordering_on_the_wire() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/a/b/page", "deep=1");
    http(&mut j, true, "a.test", "/a/b/page", "mid=1; Path=/a");
    http(&mut j, true, "a.test", "/a/b/page", "root=1; Path=/");
    http(&mut j, true, "a.test", "/a/b/page", "deep2=1");
    // Longer path first, then the older one first.
    assert_eq!(sent(&mut j, true, "a.test", "/a/b/page", Party::First), "deep=1; deep2=1; mid=1; root=1");
}

#[test]
fn the_query_is_not_part_of_the_path() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/x", "a=1; Path=/x");
    for u in ["https://a.test/x?next=/y", "https://a.test/x/y?q#f", "https://a.test/x"] {
        let url = Url::parse(u).unwrap();
        assert_eq!(j.header(&At::of(&url), Party::First, NOW).as_deref(), Some("a=1"), "{u}");
    }
    // Loopback is a secure context over http.
    assert!(At::of(&Url::parse("http://localhost:8080/").unwrap()).secure && At::of(&Url::parse("http://127.0.0.1/").unwrap()).secure);
    assert!(!At::of(&Url::parse("http://a.test/").unwrap()).secure);
}

#[test]
fn a_host_with_no_cookies_sends_none() {
    let mut j = Jar::default();
    assert_eq!(j.header(&at(true, "a.test", "/"), Party::First, NOW), None);
    http(&mut j, true, "a.test", "/", "a=1");
    assert_eq!(j.header(&at(true, "b.test", "/"), Party::First, NOW), None);
}

#[test]
fn list_shows_flags() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/p", "k=v; Secure; HttpOnly; SameSite=Lax; Domain=a.test; Path=/p; Partitioned");
    let l = j.list();
    assert_eq!(l.len(), 1);
    let c = l[0];
    assert_eq!((c.name.as_str(), c.domain.as_str(), c.host_only, c.path.as_str(), c.secure, c.http_only, c.same_site, c.expires), ("k", "a.test", false, "/p", true, true, SameSite::Lax, None));
}

#[test]
fn lookup_finds_a_cookie_for_a_url_with_no_side_effect() {
    let mut j = Jar::default();
    http(&mut j, true, "a.test", "/", "sid=1; HttpOnly; SameSite=Strict");
    http(&mut j, true, "a.test", "/", "sid=2; Path=/deep");
    let l = |j: &Jar, host: &str, path: &str| j.lookup(&at(true, host, path), "sid", NOW).map(|c| c.value.clone());
    assert_eq!(l(&j, "a.test", "/"), Some("1".into()));
    assert_eq!(l(&j, "a.test", "/deep/x"), Some("2".into()));
    assert_eq!(l(&j, "b.test", "/"), None);
    assert_eq!(j.lookup(&at(true, "a.test", "/"), "other", NOW), None);
    assert_eq!(j.lookup(&at(true, "a.test", "/"), "sid", NOW + 1e15).map(|c| c.value.clone()), Some("1".into()), "a session cookie has no end");
}

#[test]
fn insert_round_trips_replaces_and_skips_expired() {
    let mut a = Jar::default();
    http(&mut a, true, "www.a.test", "/", "x=1; Domain=a.test; Secure; HttpOnly; SameSite=Lax; Max-Age=1000");
    http(&mut a, true, "www.a.test", "/p", "y=2");
    let mut b = Jar::default();
    for c in a.list() {
        b.insert(c.clone(), NOW);
    }
    assert_eq!(b.len(), 2);
    let (la, lb) = (a.list(), b.list());
    for (x, y) in la.iter().zip(lb.iter()) {
        assert_eq!((&x.name, &x.value, &x.domain, x.host_only, &x.path, x.secure, x.http_only, x.same_site, x.expires), (&y.name, &y.value, &y.domain, y.host_only, &y.path, y.secure, y.http_only, y.same_site, y.expires));
    }
    let mut r = Cookie::new("x", "9", "a.test", false, "/");
    b.insert(r.clone(), NOW);
    assert_eq!(b.len(), 2);
    assert!(b.list().iter().any(|c| c.name == "x" && c.value == "9"));
    r.name = "old".into();
    r.expires = Some(NOW);
    b.insert(r, NOW);
    assert_eq!(b.len(), 2);
    b.insert(Cookie::new("z", "1", "co.uk", false, "/"), NOW);
    assert_eq!(b.len(), 2);
    b.insert(Cookie::new("z", "1", "localhost", true, "/"), NOW);
    assert_eq!(b.len(), 3);
}

#[test]
fn insert_refuses_line_breaks_and_keeps_the_limits() {
    let mut j = Jar::default();
    j.insert(Cookie::new("a", "1\r\nX-Evil: 1", "a.test", true, "/"), NOW);
    j.insert(Cookie::new("b\n", "1", "a.test", true, "/"), NOW);
    j.insert(Cookie::new("c", "1", "a.test", true, "/\r"), NOW);
    j.insert(Cookie::new("", "", "a.test", true, "/"), NOW);
    j.insert(Cookie::new("d", &"v".repeat(5000), "a.test", true, "/"), NOW);
    assert_eq!(j.len(), 0);
    for i in 0..200 {
        j.insert(Cookie::new(&format!("n{i}"), "1", "a.test", true, "/"), NOW);
    }
    assert_eq!(j.len(), 180, "the domain limit holds");
    let mut far = Cookie::new("f", "1", "b.test", true, "/");
    far.expires = Some(NOW + 1e15);
    j.insert(far, NOW);
    assert!(j.list().iter().find(|c| c.name == "f").unwrap().expires.unwrap() <= NOW + 400.0 * 86_400_000.0, "the expiry is cut to 400 days");
}

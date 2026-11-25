// The loose match of a replay (`BARE_NET_REPLAY_LOOSE`): volatile query values and bodies do not stop a request from finding its reply.
use super::*;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-replay-loose-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn reply(body: &str) -> Entry {
    Entry { status: 200, complete: true, body: body.into(), ..Entry::default() }
}

/// Record `(label, body)` pairs with increasing write times, so the order does not depend on the clock resolution of the file system.
fn record(d: &Path, items: &[(String, &str)]) {
    let rec = Tape::record(d);
    let t0 = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    for (i, (label, body)) in items.iter().enumerate() {
        let slot = rec.slot(label.clone());
        slot.save(&reply(body));
        fs::File::options().write(true).open(&slot.path).unwrap().set_modified(t0 + std::time::Duration::from_secs(i as u64)).unwrap();
    }
}

fn body(t: &Tape, label: &str) -> Option<String> {
    t.find(label).map(|e| String::from_utf8(e.body).unwrap())
}

#[test]
fn volatile_query_values_become_a_wildcard() {
    assert_eq!(loose_key("HEAD https://t:443/diagnostics/network?cb=1795608651054"), "HEAD https://t:443/diagnostics/network?cb=*");
    assert_eq!(loose_key("GET https://l:443/a?client-request-id=Core-964d5c95-f92c-4949-8d11-b4b4e17f3ee2&x=1"), "GET https://l:443/a?client-request-id=*&x=1");
    assert_eq!(loose_key("POST https://e:443/c?w=2 194beab0deadbeef"), "POST https://e:443/c?w=2");
    assert_eq!(loose_key("GET https://a:443/x.js"), "GET https://a:443/x.js");
    // A short number and a short word stay: they choose what the server sends.
    assert_eq!(loose_key("GET https://a:443/x?page=2&lang=en"), "GET https://a:443/x?page=2&lang=en");
}

#[test]
fn a_miss_takes_the_next_reply_of_its_loose_key_in_recording_order() {
    let d = dir("order");
    let post = |n: &[u8]| Tape::label("POST", "https://l:443/token", n);
    record(&d, &[(post(b"a"), "first"), (Tape::label("GET", "https://l:443/other", b""), "other"), (post(b"b"), "second")]);
    let t = Tape::new(&d, true, false).loose();
    assert_eq!(body(&t, &post(b"x")).as_deref(), Some("first"));
    assert_eq!(body(&t, &post(b"y")).as_deref(), Some("second"));
    // All used: the last one again, as the exact match does.
    assert_eq!(body(&t, &post(b"z")).as_deref(), Some("second"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_exact_label_wins_and_a_different_path_still_misses() {
    let d = dir("exact");
    let get = |cb: &str| Tape::label("GET", &format!("https://t:443/ping?cb={cb}"), b"");
    record(&d, &[(get("1795608651054"), "a"), (get("1795608656076"), "b")]);
    let t = Tape::new(&d, true, false).loose();
    assert_eq!(body(&t, &get("1795608656076")).as_deref(), Some("b"));
    assert_eq!(body(&t, &get("1795609999999")).as_deref(), Some("a"));
    assert_eq!(body(&t, "GET https://t:443/pong?cb=1795609999999"), None);
    // Without the loose match a new time misses.
    assert_eq!(body(&Tape::replay(&d), &get("1795609999999")), None);
    let _ = fs::remove_dir_all(&d);
}

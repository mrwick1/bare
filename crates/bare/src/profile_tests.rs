//! The profile directory (`profile.rs`): save and load, the lock, the fallback of `default`, the writes, the file modes.

use crate::browser_navigation_storage::WebStorage;
use crate::profile::{OpenError, Profile, Saver};
use bare_net::cookie::{Cookie, SameSite};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-profile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Add a lasting cookie, a session cookie and two `localStorage` items to `s`.
fn fill(s: &WebStorage) {
    {
        let jar = s.jar();
        let mut jar = jar.lock().unwrap();
        let now = jar.now();
        let mut c = Cookie::new("sid", "secret-1", "a.test", false, "/p");
        (c.expires, c.secure, c.http_only, c.same_site) = (Some(now + 3_600_000.0), true, true, SameSite::Lax);
        jar.insert(c, now);
        jar.insert(Cookie::new("tmp", "secret-2", "b.test", true, "/"), now);
    }
    s.set_item(false, "https://a.test", "k", "v\u{e9}").unwrap();
    s.set_item(false, "https://b.test", "x", "y").unwrap();
    s.set_item(true, "https://a.test", "session", "not saved").unwrap();
}

#[test]
fn a_saved_profile_loads_its_cookies_and_local_storage() {
    let d = dir("round");
    let s = WebStorage::default();
    let saver = Saver::new(Profile::open(&d).unwrap(), s.clone());
    fill(&s);
    drop(saver);
    let st = Profile::open(&d).unwrap().load().unwrap();
    let want = s.export();
    // Each field but `used`, the order of use in one jar, which is not saved.
    let fields = |l: &[Cookie]| l.iter().map(|c| (c.name.clone(), c.value.clone(), c.domain.clone(), c.host_only, c.path.clone(), c.secure, c.http_only, c.same_site, c.expires)).collect::<Vec<_>>();
    assert_eq!(st.cookies.len(), 2);
    assert_eq!(fields(&st.cookies), fields(&want.cookies), "a session cookie is kept too");
    assert_eq!(st.local, want.local);
    assert!(st.session.is_empty(), "sessionStorage is not saved");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_lock_refuses_a_second_holder_and_ends_with_the_last_clone() {
    let d = dir("lock");
    let p = Profile::open(&d).unwrap();
    let pid = std::process::id();
    assert!(matches!(Profile::open(&d), Err(OpenError::Locked(Some(x))) if x == pid));
    let q = p.clone();
    drop(p);
    assert!(matches!(Profile::open(&d), Err(OpenError::Locked(_))), "a clone keeps the lock");
    drop(q);
    Profile::open(&d).unwrap();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_stale_lock_file_is_taken_over() {
    let d = dir("stale");
    std::fs::create_dir_all(&d).unwrap();
    // A holder that ended (a crash or a kill) leaves the file and its process id, but the OS lock is gone.
    std::fs::write(d.join("profile.lock"), "4194999").unwrap();
    let _p = Profile::open(&d).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("profile.lock")).unwrap(), std::process::id().to_string());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_locked_default_profile_gives_memory_and_another_profile_an_error() {
    let data = dir("pick");
    let (held, note) = Profile::pick(None, Some(&data)).unwrap();
    assert!(held.is_some() && note == "profile default", "{note}");
    assert!(data.join("profiles/default/profile.lock").exists());
    let (p, note) = Profile::pick(None, Some(&data)).unwrap();
    assert!(p.is_none() && note.contains("in use by process") && note.contains("in memory"), "{note}");
    let (_work, _) = Profile::pick(Some("work"), Some(&data)).unwrap();
    let e = Profile::pick(Some("work"), Some(&data)).err().unwrap();
    assert!(e.contains("profile work") && e.contains("in use"), "{e}");
    // No data directory (BARE_DATA_DIR is empty): `default` is in memory, a name is an error, a path still works.
    assert!(matches!(Profile::pick(None, None), Ok((None, n)) if n.is_empty()));
    assert!(Profile::pick(Some("work"), None).is_err());
    let path = data.join("by-path");
    assert!(Profile::pick(Some(path.to_str().unwrap()), None).unwrap().0.is_some());
    let _ = std::fs::remove_dir_all(data);
}

#[test]
fn a_write_waits_a_second_and_no_change_writes_nothing() {
    let d = dir("throttle");
    let s = WebStorage::default();
    let mut saver = Saver::new(Profile::open(&d).unwrap(), s.clone());
    let t0 = Instant::now();
    assert_eq!(saver.tick(t0), None);
    assert_eq!(saver.writes, 0, "nothing changed");
    s.set_item(false, "https://a.test", "k", "1").unwrap();
    assert_eq!(saver.tick(t0), None);
    assert_eq!(saver.writes, 1, "the first change writes local.bin at once");
    s.set_item(false, "https://a.test", "k", "2").unwrap();
    assert_eq!(saver.tick(t0 + Duration::from_millis(400)), Some(t0 + Duration::from_secs(1)));
    assert_eq!(saver.writes, 1, "a second change waits for its second");
    assert_eq!(saver.tick(t0 + Duration::from_secs(1)), None);
    assert_eq!(saver.writes, 2);
    s.set_item(false, "https://a.test", "k", "2").unwrap();
    assert_eq!(saver.tick(t0 + Duration::from_secs(5)), None);
    assert_eq!(saver.writes, 2, "a set to the same value is no change");
    s.set_item(false, "https://a.test", "k", "3").unwrap();
    drop(saver);
    let st = Profile::open(&d).unwrap().load().unwrap();
    assert_eq!(st.local, vec![("https://a.test".to_string(), vec![("k".to_string(), "3".to_string())])], "the drop writes the last change");
    assert!(!d.join("cookies.bin").exists(), "the cookies did not change");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_torn_write_never_replaces_the_good_file_and_a_damaged_file_is_an_error() {
    let d = dir("torn");
    let s = WebStorage::default();
    let p = Profile::open(&d).unwrap();
    let mut saver = Saver::new(p.clone(), s.clone());
    fill(&s);
    saver.flush();
    let good = p.load().unwrap();
    // A crash in the middle of the next write leaves a part of the temporary file; the file that the profile reads stays whole.
    std::fs::write(d.join(".cookies.bin.tmp"), b"bareck01\x05\x00").unwrap();
    assert_eq!(p.load().unwrap().cookies.len(), good.cookies.len());
    // The next write replaces the leftover.
    s.set_item(false, "https://c.test", "n", "1").unwrap();
    s.jar().lock().unwrap().clear();
    saver.flush();
    assert!(p.load().unwrap().cookies.is_empty() && !d.join(".cookies.bin.tmp").exists());
    // A file cut short, or with a changed byte, is refused; the message names the file and holds no value.
    let b = std::fs::read(d.join("local.bin")).unwrap();
    for bad in [b[..b.len() - 3].to_vec(), { let mut x = b.clone(); x[14] ^= 1; x }] {
        std::fs::write(d.join("local.bin"), bad).unwrap();
        let e = p.load().unwrap_err();
        assert!(e.contains("local.bin") && e.contains("damaged") && !e.contains("v\u{e9}"), "{e}");
    }
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(unix)]
#[test]
fn the_directory_is_0700_and_the_files_0600() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("modes");
    let p = Profile::open(&d).unwrap();
    let s = WebStorage::default();
    let mut saver = Saver::new(p, s.clone());
    fill(&s);
    saver.flush();
    let mode = |n: &str| std::fs::metadata(d.join(n)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(""), 0o700);
    for f in ["profile.lock", "cookies.bin", "local.bin"] {
        assert_eq!(mode(f), 0o600, "{f}");
    }
    let _ = std::fs::remove_dir_all(d);
}

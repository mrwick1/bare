//! The permission decisions in a profile (`profile.rs`, `permissions.bin`) and in a `StorageState`.

use crate::browser_navigation_storage::{Permission, WebStorage};
use crate::profile::{Profile, Saver};
use std::path::PathBuf;
use std::time::Instant;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-profile-perm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn a_grant_is_saved_in_the_profile_and_loads_again() {
    let d = dir("round");
    let s = WebStorage::default();
    let mut saver = Saver::new(Profile::open(&d).unwrap(), s.clone());
    s.set_permission("https://a.test", "notifications", Some(Permission::Granted));
    s.set_permission("https://b.test", "notifications", Some(Permission::Denied));
    assert_eq!(saver.tick(Instant::now()), None);
    assert_eq!(saver.writes, 1, "only permissions.bin is written");
    assert!(d.join("permissions.bin").is_file());
    drop(saver);
    let st = Profile::open(&d).unwrap().load().unwrap();
    assert_eq!(st.permissions, vec![("https://a.test".to_string(), "notifications".to_string(), Permission::Granted), ("https://b.test".to_string(), "notifications".to_string(), Permission::Denied)]);
    let back = WebStorage::default();
    back.import(&st).unwrap();
    assert_eq!(back.permission("https://a.test", "notifications"), Some(Permission::Granted));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_reset_removes_the_grant_from_the_profile() {
    let d = dir("reset");
    let s = WebStorage::default();
    let saver = Saver::new(Profile::open(&d).unwrap(), s.clone());
    s.set_permission("https://a.test", "notifications", Some(Permission::Granted));
    drop(saver);
    let saver = Saver::new(Profile::open(&d).unwrap(), s.clone());
    s.set_permission("https://a.test", "notifications", None);
    drop(saver);
    assert!(Profile::open(&d).unwrap().load().unwrap().permissions.is_empty());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_damaged_permissions_file_is_an_error_that_names_it() {
    let d = dir("bad");
    let p = Profile::open(&d).unwrap();
    std::fs::write(d.join("permissions.bin"), b"barepm01junk").unwrap();
    let e = p.load().unwrap_err();
    assert!(e.contains("permissions.bin") && e.contains("damaged"), "{e}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn export_holds_the_decisions_sorted() {
    let s = WebStorage::default();
    s.set_permission("https://b.test", "notifications", Some(Permission::Granted));
    s.set_permission("https://a.test", "notifications", Some(Permission::Denied));
    let st = s.export();
    assert_eq!(st.permissions.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["https://a.test", "https://b.test"]);
}

//! The permission decisions in a session file (`state.rs`): the field `permissions`.

use crate::state::{from_json, to_json};
use bare::browser_navigation::Permission;
use bare::page::StorageState;

#[test]
fn the_permissions_round_trip_and_an_old_file_has_none() {
    let st = StorageState { permissions: vec![("https://a.test".into(), "notifications".into(), Permission::Granted), ("https://b.test".into(), "notifications".into(), Permission::Denied)], ..Default::default() };
    let j = to_json(&st);
    assert!(j.contains(r#""permissions":[{"name":"notifications","origin":"https://a.test","state":"granted"}"#), "{j}");
    assert_eq!(from_json(&j, "f").unwrap().permissions, st.permissions);
    assert!(from_json(r#"{"cookies":[],"origins":[]}"#, "f").unwrap().permissions.is_empty());
}

#[test]
fn a_bad_permission_names_the_field() {
    let e = from_json(r#"{"permissions":[{"origin":"https://a.test","name":"notifications","state":"maybe"}]}"#, "f.json").unwrap_err();
    assert_eq!(e, "f.json: bad field permissions[0].state");
}

use super::*;
use crate::fx::FxMap;

fn key(n: u32) -> Key {
    (Identity::of(&format!("var n = {n};")), Mode::Script(ScriptMode::default()))
}

#[test]
fn the_first_sight_keeps_no_unit_and_the_second_keeps_one() {
    clear();
    let source = format!("/*{}*/var once = 1;", " ".repeat(MIN_SOURCE));
    let get = || {
        let mut vm = crate::script::load("").unwrap();
        let code = vm.realm.script_code(&source, ScriptMode::default()).unwrap();
        vm.realm.run_code(code, false).unwrap();
        assert_eq!(vm.global_text("once"), "1");
    };
    get();
    assert_eq!((counts().misses, counts().entries, counts().bytes), (1, 0, 0));
    get();
    assert_eq!((counts().misses, counts().entries), (2, 1));
    get();
    assert_eq!((counts().misses, counts().hits), (2, 1));
}

#[test]
fn the_seen_set_starts_again_at_its_cap() {
    let mut cache = Cache::default();
    for n in 0..MAX_SEEN as u32 {
        assert!(!cache.admit(key(n)));
    }
    assert_eq!(cache.seen.len(), MAX_SEEN);
    assert!(!cache.admit(key(MAX_SEEN as u32)));
    assert_eq!(cache.seen.len(), 1);
    assert!(!cache.admit(key(0)));
    assert!(cache.admit(key(MAX_SEEN as u32)));
    assert!(!cache.seen.contains(&key(MAX_SEEN as u32)));
}

#[test]
fn the_fx_map_keeps_distinct_keys_of_every_length() {
    let mut map = FxMap::default();
    let words: Vec<String> = (0..200).map(|n| "x".repeat(n % 17) + &n.to_string()).collect();
    for (i, w) in words.iter().enumerate() {
        map.insert(w.as_str(), i);
    }
    assert_eq!(map.len(), words.len());
    assert!(words.iter().enumerate().all(|(i, w)| map[w.as_str()] == i));
}

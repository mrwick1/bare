//! The grep gate of S0.4 PR 6. The browser must not name the old engine outside the files on `old_engine_files.txt`, and must not
//! reach the engine through the `it` field of `Browser` outside them. The list can only get shorter: a file that is not on it and
//! names the engine fails the test, and so does a file on it that no longer does. Test files (`*_tests.rs`) are not scanned.
//!
//! Tags: `engine` = the file has `bare_js::` (the old engine crate); `it` = the file uses the field `Browser::it`.

use std::collections::BTreeMap;
use std::path::Path;

fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read the source directory") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") && !p.file_name().is_some_and(|n| n.to_string_lossy().ends_with("_tests.rs")) {
            out.push(p);
        }
    }
}

/// Does the text use the field `it` (`.it` not followed by an identifier character or `(`)?
fn uses_it_field(text: &str) -> bool {
    text.match_indices(".it").any(|(i, _)| !text[i + 3..].starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '('))
}

fn names_engine(text: &str) -> bool {
    text.match_indices("bare_js::").any(|(i, _)| !text[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
}

#[test]
fn the_browser_names_the_old_engine_only_in_the_listed_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut all = Vec::new();
    files(&root.join("src"), &mut all);
    for p in all {
        let text = std::fs::read_to_string(&p).expect("read a source file");
        let rel = p.strip_prefix(root).expect("under the crate").to_string_lossy().replace('\\', "/");
        let mut tags = Vec::new();
        if names_engine(&text) {
            tags.push("engine");
        }
        if uses_it_field(&text) {
            tags.push("it");
        }
        if !tags.is_empty() {
            found.insert(rel, tags);
        }
    }
    let listed: BTreeMap<String, Vec<String>> = std::fs::read_to_string(root.join("old_engine_files.txt"))
        .expect("old_engine_files.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (f, t) = l.split_once(' ').expect("`path tag,tag`");
            (f.to_string(), t.split(',').map(str::to_string).collect())
        })
        .collect();
    let mut bad = Vec::new();
    for (f, tags) in &found {
        match listed.get(f) {
            None => bad.push(format!("{f}: names the old engine ({}) and is not on old_engine_files.txt", tags.join(","))),
            Some(l) => {
                for t in tags {
                    if !l.iter().any(|x| x == t) {
                        bad.push(format!("{f}: new use of `{t}`"));
                    }
                }
            }
        }
    }
    for (f, l) in &listed {
        match found.get(f) {
            None => bad.push(format!("{f}: on the list but no longer names the old engine: remove the line")),
            Some(tags) => {
                for t in l {
                    if !tags.iter().any(|x| x == t) {
                        bad.push(format!("{f}: `{t}` is gone: remove it from the line"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "the old-engine gate failed:\n{}", bad.join("\n"));
}

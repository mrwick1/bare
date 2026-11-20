//! The differential runner as a test: the corpus is clean (except the known differences), and a planted difference is found.
use bare_js_diff::diff::*;

fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(256 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn the_corpus_has_no_new_difference() {
    let r = big_stack(|| {
        let root = root();
        run_all(&corpus(&root), &known(&root))
    });
    print(&r, false);
    assert!(r.rows.len() > 100, "the corpus is read: {} programs", r.rows.len());
    assert!(r.ok(), "new differences: {:?}; stale known: {:?}", r.new_diffs, r.stale);
}

#[test]
fn a_planted_difference_is_reported() {
    // `__engine` is the one global that differs by design.
    let c = |name: &str, src: &str| Case { name: name.into(), src: src.into(), global: String::new() };
    let r = big_stack(move || run_all(&[c("planted", "print(__engine);"), c("same", "print(1 + 1);")], &[]));
    assert_eq!(r.rows[0].verdict, Verdict::Differs);
    assert_eq!(r.rows[1].verdict, Verdict::Same);
    assert_eq!(r.new_diffs, ["planted"]);
    assert!(!r.ok());
    // The same difference in the known list does not fail the run.
    let r = big_stack(move || run_all(&[c("planted", "print(__engine);")], &[("planted".into(), "by design".into())]));
    assert!(r.ok());
}

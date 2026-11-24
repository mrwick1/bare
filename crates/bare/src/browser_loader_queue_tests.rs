//! Tests of the queue of dynamic scripts (`browser_loader.rs`): a burst of inserted scripts all run, as in Chrome.

use crate::browser_loader::PageLimits;
use crate::browser_loader_tests::{PRE, big, js, page, quick};

#[test]
fn a_burst_of_dynamic_scripts_all_runs() {
    big(|| {
        // Teams inserts a few hundred chunk scripts in one task. Each element waits in the queue once; the DOM caps the elements.
        let n = 300;
        let html = format!("{PRE}<script>for (var i = 0; i < {n}; i++) {{ var s = document.createElement('script'); s.textContent = 'log.push(' + i + ')'; document.body.appendChild(s) }} var e = document.createElement('script'); e.src = '/e.js'; document.body.appendChild(e)</script>");
        let (log, r, _) = page(&html, vec![js("/e.js", "log.push('e')")], PageLimits { ..quick() });
        let want: Vec<String> = (0..n).map(|i| i.to_string()).chain(["e".to_string()]).collect();
        assert_eq!(log, want.join("|"), "{r:?}");
    });
}

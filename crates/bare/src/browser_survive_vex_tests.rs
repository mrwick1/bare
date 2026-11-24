//! A runaway script on js2 (the default engine) ends at the step budget of `EvalLimits::max_steps`, and the page keeps running.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_js::eval::EvalLimits;
use std::time::Duration;

#[test]
fn a_runaway_script_on_js2_ends_at_the_step_budget() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let body = "<script>localStorage.setItem('a', '1')</script><script>while (true) {}</script><script>localStorage.setItem('c', '1')</script>";
            let srv: Server = serve(move |path| if path == "/" { Reply::new(200, Some("text/html; charset=utf-8"), body) } else { Reply::new(404, Some("text/plain"), "no") });
            let mut c = SessionConfig::new(&srv.url("/"));
            c.vex = true;
            c.virtual_clock = true;
            c.eval = EvalLimits { max_steps: 2_000_000, ..Default::default() };
            c.page.quiet = Duration::from_millis(100);
            c.limits.max_time = Duration::from_secs(20);
            let mut s = Session::new(c);
            let r = s.run();
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            for k in ["a", "c"] {
                assert_eq!(s.storage().get_item(false, &srv.url(""), k).as_deref(), Some("1"), "{k}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

//! A script budget ends only its own task, however many tasks it ends. The page never stops for good because of aborts.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(body: &'static str) -> Server {
    serve(move |path| if path == "/" { Reply::new(200, Some("text/html; charset=utf-8"), body) } else { Reply::new(404, Some("text/plain"), "no") })
}

#[test]
fn many_aborted_tasks_never_stop_the_page() {
    big(|| {
        let srv = page("<script>
            for (var i = 0; i < 10; i++) setTimeout(function () { while (true) {} }, i + 1);
            setTimeout(function () { localStorage.setItem('after', '1') }, 50);
          </script>");
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.eval = EvalLimits { max_steps: 200_000, ..Default::default() };
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages[0].aborted, 10);
        assert_eq!(s.storage().get_item(false, &srv.url(""), "after").as_deref(), Some("1"));
    });
}

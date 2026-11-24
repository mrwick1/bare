//! A session runs its pages on Vex. `BARE_JS_EXEC` picks nothing: the old names `tree`, `auto` and `byte` are ignored.

use crate::browser_any::AnyBrowser;
use crate::browser_session::{Session, SessionConfig};
use crate::resources::test_server::{Reply, serve};

#[test]
fn a_session_runs_its_page_on_vex() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let server = serve(|_| Reply::new(200, Some("text/html"), "<p>x</p>"));
        let mut session = Session::new(SessionConfig::new(&server.url("/")));
        session.run();
        let browser = session.browser().expect("a browser");
        let AnyBrowser::New(_) = browser;
        assert_eq!(browser.script_text("6 * 7").unwrap(), (false, "42".to_string()));
    }).unwrap().join().unwrap();
}

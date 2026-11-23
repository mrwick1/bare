//! A GC in a listener must not free the event that the host dispatches (js2).

use crate::browser::*;
use crate::eng::Ops;
use crate::eng_new::New;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn a_message_event_lives_while_a_listener_allocates() {
    big(|| {
        let page = "<!doctype html><html><body></body></html>";
        let mut b = Browser::<New>::make(tree::parse(page), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
        b.use_virtual_clock_at(1_000_000.0).unwrap();
        let src = "var seen = 0; var c = new MessageChannel();
            c.port1.addEventListener('message', function () { Promise.resolve().then(function () { var a = []; for (var i = 0; i < 400000; i++) { a.push({ i: i, s: 'x' + i }); if (a.length > 1000) a = []; } }) }); c.port1.addEventListener('message', function () { seen++ }); c.port1.start();
            c.port2.postMessage(1); 1";
        b.run_script(src).expect("script");
        b.advance_time(0.0, 1_000_000).expect("advance");
        let v = b.run_script("seen").expect("script");
        assert_eq!(b.it.display_o(v), "1");
    })
}

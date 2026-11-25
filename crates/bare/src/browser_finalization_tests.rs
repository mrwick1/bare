//! A `FinalizationRegistry` callback runs as a task after a collection, and a throw of it is reported (Vex).

use crate::browser::*;
use crate::eng::Ops;
use crate::eng_new::New;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page() -> Browser<New> {
    let mut b = Browser::<New>::make(tree::parse("<!doctype html><html><body></body></html>"), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
    b.use_virtual_clock_at(1_000_000.0).unwrap();
    b
}

fn text(b: &mut Browser<New>, src: &str) -> String {
    let v = b.run_script(src).expect("script");
    b.it.display_o(v)
}

#[test]
fn the_callback_runs_as_a_task_after_a_collection() {
    big(|| {
        let mut b = page();
        b.run_script("var log = []; var f = new FinalizationRegistry(function (h) { log.push(h); if (h == 'a') throw new Error('boom'); }); (function () { f.register({}, 'a'); f.register({}, 'b'); })(); 1").expect("script");
        // Nothing runs before a collection, and a collection runs no script.
        b.advance_time(0.0, 10).expect("advance");
        assert_eq!(text(&mut b, "log.join()"), "");
        b.collect(&[]).expect("collect");
        assert_eq!(text(&mut b, "log.join()"), "");
        b.advance_time(0.0, 10).expect("advance");
        assert_eq!(text(&mut b, "log.join()"), "a,b");
        // The throw of the first callback was reported and the second ran.
        assert_eq!(b.take_reports().len(), 1);
        // Each callback runs once.
        b.collect(&[]).expect("collect");
        b.advance_time(0.0, 10).expect("advance");
        assert_eq!(text(&mut b, "log.join()"), "a,b");
    })
}

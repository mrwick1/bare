//! A transaction with thousands of requests runs to the end. Each request fires a `success` event. The shim yields to a task
//! after each 64 requests, so a collect between tasks can free the events (WhatsApp, the message download after the link).

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_transaction_with_more_requests_than_the_event_cap_completes() {
    big(|| {
        let lim = BrowserLimits::default();
        let mut b = Browser::new(tree::parse("<body></body>"), lim, EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock_at(1000.0).unwrap();
        b.set_document_url("https://web.example/").unwrap();
        js(
            &mut b,
            "var out = []; var r = indexedDB.open('m', 1);
             r.onupgradeneeded = function () { r.result.createObjectStore('s') };
             r.onsuccess = function () {
               var tx = r.result.transaction('s', 'readwrite'), s = tx.objectStore('s'), n = 0;
               for (var i = 0; i < 5000; i++) s.put(i, i).onsuccess = function () { n++ };
               tx.oncomplete = function () { out.push(n) };
             };",
        );
        let mut err = None;
        // Pump as the loader does: a few tasks, then a collect when one is due.
        for _ in 0..2000 {
            if let Err(e) = b.drain().and_then(|_| b.advance_time(1.0, 8).map(|_| ())) {
                err = Some(format!("{e:?}"));
                break;
            }
            if b.collect_due() {
                b.collect(&[]).unwrap();
            }
        }
        assert_eq!(err, None);
        assert_eq!(js(&mut b, "out.join()"), "5000");
    });
}

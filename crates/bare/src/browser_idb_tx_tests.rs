//! The IndexedDB shim must keep a transaction live across a chain of microtasks, not finish it the
//! moment its request queue first drains. The `idb` promise wrapper and `async`/`await` enqueue the
//! next request several microtask hops after the previous one's `success`, so a transaction that
//! finishes on the first empty queue throws `TransactionInactiveError` on that next request. Teams
//! hit this. Completion must wait for the next task boundary with the queue still empty.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u),
        Err(e) => format!("other:{e:?}"),
    }
}

fn pump(b: &mut Browser) {
    b.drain().unwrap();
    b.advance_time(500.0, 10_000).unwrap();
}

/// A write enqueued several microtask hops after a read's `success` must still land on the same
/// transaction, as the `idb` library and `await` do. Before the fix the transaction finished on the
/// first empty queue and the delayed write threw `TransactionInactiveError`.
#[test]
fn transaction_survives_a_microtask_chain_between_requests() {
    big(|| {
        let mut b = browser();
        js(
            &mut b,
            "var out = []; var req = indexedDB.open('db', 1);
             req.onupgradeneeded = function () { req.result.createObjectStore('s', { keyPath: 'id' }) };
             req.onsuccess = function () {
               var db = req.result, tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s');
               s.put({ id: 1, v: 'a' });
               s.get(1).onsuccess = function () {
                 Promise.resolve().then(function () {}).then(function () {}).then(function () {
                   try { s.put({ id: 2, v: 'b' }); out.push('put2ok'); }
                   catch (e) { out.push('throw:' + e.name); }
                 });
               };
               tx.oncomplete = function () {
                 out.push('complete');
                 var t2 = db.transaction('s'); t2.objectStore('s').count().onsuccess = function (e) { out.push('n' + e.target.result) };
               };
             };",
        );
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join()"), "put2ok,complete,n2");
    });
}

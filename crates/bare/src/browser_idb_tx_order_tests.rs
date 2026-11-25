//! Transaction order: a transaction waits for each earlier, unfinished transaction of its database whose scope overlaps its
//! own, when either one writes. Two readonly transactions run at the same time. WhatsApp reads a row and adds it when it is
//! missing; two such transactions at the same time added the same key twice (`ConstraintError`).

use crate::browser::*;
use crate::browser_navigation::WebStorage;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn overlapping_write_transactions_run_in_order() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
            b.use_virtual_clock_at(1000.0).unwrap();
            b.set_document_url("https://web.example/").unwrap();
            b.attach_storage(WebStorage::default());
            js(
                &mut b,
                "var out = [];
                 var r = indexedDB.open('o', 1);
                 r.onupgradeneeded = function () { r.result.createObjectStore('s'); r.result.createObjectStore('t') };
                 r.onsuccess = function () {
                   var db = r.result;
                   // Read, then add when missing: the second transaction must see the first one's add.
                   function addOnce(tag) {
                     var tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s');
                     s.get('k').onsuccess = function (e) {
                       if (e.target.result === undefined) s.add(tag, 'k').onsuccess = function () { out.push(tag + ' added') };
                       else out.push(tag + ' saw ' + e.target.result);
                     };
                     tx.oncomplete = function () { out.push(tag + ' done') };
                     tx.onabort = function () { out.push(tag + ' abort ' + (tx.error && tx.error.name)) };
                   }
                   addOnce('a'); addOnce('b');
                   // A readonly transaction on another store does not wait; two readonly ones on 's' wait only for the writers.
                   var t = db.transaction('t'); t.objectStore('t').count().onsuccess = function () { out.push('t read') };
                 };",
            );
            b.drain().unwrap();
            b.advance_time(500.0, 10_000).unwrap();
            assert_eq!(js(&mut b, "out.join('|')"), "t read|a added|a done|b saw a|b done");
        })
        .unwrap()
        .join()
        .unwrap();
}

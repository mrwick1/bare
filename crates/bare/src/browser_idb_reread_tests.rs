//! A stored value that holds a platform object (a `Blob`) can be read more than once. Reading a clone record takes its platform
//! objects out of it, so before the fix the second read found a damaged record, and the page stopped (WhatsApp, after the link).

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
fn a_value_with_a_blob_reads_twice() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock_at(1000.0).unwrap();
        b.set_document_url("https://web.example/").unwrap();
        js(
            &mut b,
            "var out = []; var r = indexedDB.open('b', 1);
             r.onupgradeneeded = function () { r.result.createObjectStore('s').put({ b: new Blob(['hi!']) }, 1) };
             r.onsuccess = function () {
               var s = r.result.transaction('s').objectStore('s');
               s.get(1).onsuccess = function (e) { out.push(e.target.result.b.size) };
               s.get(1).onsuccess = function (e) { out.push(e.target.result.b.size) };
               s.getAll().onsuccess = function (e) { out.push(e.target.result[0].b instanceof Blob) };
             };",
        );
        b.drain().unwrap();
        b.advance_time(500.0, 10_000).unwrap();
        assert_eq!(js(&mut b, "out.join()"), "3,3,true");
    });
}

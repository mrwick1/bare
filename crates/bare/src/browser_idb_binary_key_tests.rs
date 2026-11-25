//! Binary keys (`ArrayBuffer` and views): valid IndexedDB keys. They sort after strings and before arrays,
//! byte by byte. WhatsApp keeps its sync keys under binary key ids.

use crate::browser::*;
use crate::browser_navigation::WebStorage;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

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
fn binary_keys_store_find_and_sort() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock_at(1000.0).unwrap();
        b.set_document_url("https://web.example/").unwrap();
        b.attach_storage(WebStorage::default());
        js(
            &mut b,
            "var out = [];
             out.push(indexedDB.cmp(new Uint8Array([1, 2]), new Uint8Array([1, 3])), indexedDB.cmp(new Uint8Array([2]).buffer, 'z'), indexedDB.cmp(new Uint8Array([2]), [1]));
             var r = indexedDB.open('sync', 1);
             r.onupgradeneeded = function () { r.result.createObjectStore('keys', { keyPath: 'id' }); r.result.createObjectStore('raw') };
             r.onsuccess = function () {
               var tx = r.result.transaction(['keys', 'raw'], 'readwrite'), s = tx.objectStore('keys');
               s.put({ id: new Uint8Array([0, 9, 255]), v: 'a' });
               s.put({ id: new Uint8Array([0, 9]), v: 'b' });
               tx.objectStore('raw').put('c', new DataView(new Uint8Array([5, 6, 7]).buffer, 1, 2));
               s.get(new Uint8Array([0, 9, 255])).onsuccess = function (e) { out.push(e.target.result && e.target.result.v) };
               s.getAllKeys().onsuccess = function (e) { out.push(e.target.result.map(function (k) { return k instanceof ArrayBuffer ? Array.from(new Uint8Array(k)).join('.') : typeof k }).join(' ')) };
               tx.objectStore('raw').get(new Uint8Array([6, 7]).buffer).onsuccess = function (e) { out.push(e.target.result) };
               s.count(IDBKeyRange.lowerBound(new Uint8Array([0, 9]), true)).onsuccess = function (e) { out.push(e.target.result) };
             };",
        );
        b.drain().unwrap();
        b.advance_time(500.0, 10_000).unwrap();
        assert_eq!(js(&mut b, "out.join('|')"), "-1|1|-1|a|0.9 0.9.255|c|1");
    });
}

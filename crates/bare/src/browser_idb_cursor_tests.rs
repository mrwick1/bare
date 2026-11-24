//! The IndexedDB shim must give cursors over an index and key-only cursors. Teams' storage library
//! (NoSQLProvider) calls `index(...).openCursor(...)`; the shim had no such method and threw
//! `TypeError: not a function`, which ended the rest of the caller's handler.

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

#[test]
fn index_and_key_cursors_walk_in_key_order() {
    big(|| {
        let mut b = browser();
        js(
            &mut b,
            "var out = []; var req = indexedDB.open('db', 1);
             req.onupgradeneeded = function () { req.result.createObjectStore('p', { keyPath: 'id' }).createIndex('e', 'e'); };
             req.onsuccess = function () {
               var s = req.result.transaction('p', 'readwrite').objectStore('p'), ix = s.index('e');
               s.put({ id: 3, e: 'a' }); s.put({ id: 1, e: 'b' }); s.put({ id: 2, e: 'a' });
               function walk(tag, r) { var got = []; r.onsuccess = function () { var c = r.result;
                 if (!c) { out.push(tag + ':' + got.join(' ')); return; }
                 got.push(c.key + '/' + c.primaryKey + ('value' in c ? '/' + c.value.id : '') + (c.source === (tag[0] === 'i' ? ix : s) ? '' : '!src'));
                 c.continue(); }; }
               walk('ix', ix.openCursor());
               walk('ixPrev', ix.openCursor(null, 'prev'));
               walk('ixNextU', ix.openCursor(null, 'nextunique'));
               walk('ixPrevU', ix.openCursor(null, 'prevunique'));
               walk('ixOnly', ix.openCursor(IDBKeyRange.only('a')));
               walk('ixKey', ix.openKeyCursor());
               walk('sKey', s.openKeyCursor(IDBKeyRange.lowerBound(2)));
             };",
        );
        pump(&mut b);
        assert_eq!(
            js(&mut b, "out.sort().join('|')"),
            "ix:a/2/2 a/3/3 b/1/1|ixKey:a/2 a/3 b/1|ixNextU:a/2/2 b/1/1|ixOnly:a/2/2 a/3/3|ixPrev:b/1/1 a/3/3 a/2/2|ixPrevU:b/1/1 a/2/2|sKey:2/2 3/3"
        );
    });
}

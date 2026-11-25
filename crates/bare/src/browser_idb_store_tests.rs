//! The `indexedDB` data lives in the session store of the origin (`browser_idb.rs`), not in the realm. A new document of the
//! origin finds it; another origin does not. Keys sort in IndexedDB order through their byte form.

use crate::eng_new::Peek;
use crate::browser::*;
use crate::browser_navigation::WebStorage;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn at(url: &str, store: &WebStorage) -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.set_document_url(url).unwrap();
    b.attach_storage(store.clone());
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run `src` (it fills `out`), let the requests finish, and give `out.join('|')`.
fn run(b: &mut Browser, src: &str) -> String {
    js(b, &format!("var out = []; {src}"));
    b.drain().unwrap();
    b.advance_time(500.0, 10_000).unwrap();
    js(b, "out.join('|')")
}

const PUT: &str = "var r = indexedDB.open('wawc', 1);
    r.onupgradeneeded = function () { r.result.createObjectStore('user', { keyPath: 'k' }).createIndex('by', 'v', { unique: true }) };
    r.onsuccess = function () {
      var tx = r.result.transaction('user', 'readwrite');
      tx.objectStore('user').put({ k: 'me', v: 1, d: new Date(5), b: new Uint8Array([7]) });
      tx.oncomplete = function () { out.push('saved') };
    };";

const GET: &str = "var r = indexedDB.open('wawc');
    r.onupgradeneeded = function () { out.push('upgrade') };
    r.onsuccess = function () {
      var db = r.result;
      if (!db.objectStoreNames.contains('user')) { out.push('none'); return }
      var s = db.transaction('user').objectStore('user');
      s.get('me').onsuccess = function (e) { var v = e.target.result; out.push(v.v, v.d instanceof Date && +v.d, v.b[0]) };
      s.index('by').getKey(1).onsuccess = function (e) { out.push(e.target.result) };
    };";

#[test]
fn a_new_document_of_the_origin_sees_the_data() {
    big(|| {
        let store = WebStorage::default();
        let mut a = at("https://web.example/login/", &store);
        assert_eq!(run(&mut a, PUT), "saved");
        drop(a);
        // The page after a navigation: a new realm, the same session store.
        let mut b = at("https://web.example/", &store);
        assert_eq!(run(&mut b, GET), "1|5|7|me");
        // Another origin and another session see nothing.
        let mut c = at("https://other.example/", &store);
        assert_eq!(run(&mut c, GET), "upgrade|none");
        let mut d = at("https://web.example/", &WebStorage::default());
        assert_eq!(run(&mut d, GET), "upgrade|none");
        // A new tab of the session shares the data, as it shares `localStorage`.
        let mut e = at("https://web.example/", &store.new_session());
        assert_eq!(run(&mut e, GET), "1|5|7|me");
    });
}

#[test]
fn keys_sort_in_indexeddb_order() {
    big(|| {
        let mut b = at("https://web.example/", &WebStorage::default());
        let got = run(
            &mut b,
            "var r = indexedDB.open('k', 1);
             r.onupgradeneeded = function () { r.result.createObjectStore('s') };
             r.onsuccess = function () {
               var s = r.result.transaction('s', 'readwrite').objectStore('s');
               var keys = [['a', 1], ['a'], [], 'b\\u0100', 'b', 'a', '', new Date(2), new Date(-1), Infinity, 3, 0.5, -0, -2, -Infinity];
               keys.slice().reverse().forEach(function (k, i) { s.put(i, k) });
               s.getAllKeys().onsuccess = function (e) {
                 out.push(e.target.result.map(function (k) { return k instanceof Date ? 'D' + (+k) : Array.isArray(k) ? '[' + k + ']' : typeof k === 'string' ? JSON.stringify(k) : String(k) }).join(' '));
               };
               s.getAll(IDBKeyRange.bound(0, 3, true, false)).onsuccess = function (e) { out.push(e.target.result.join()) };
               s.count(IDBKeyRange.lowerBound('a', true)).onsuccess = function (e) { out.push(e.target.result) };
               s.getAllKeys(IDBKeyRange.upperBound('', true), 2).onsuccess = function (e) { out.push(e.target.result.join()) };
             };",
        );
        assert_eq!(got, "-Infinity -2 0 0.5 3 Infinity D-1 D2 \"\" \"a\" \"b\" \"bĀ\" [] [a] [a,1]|3,4|5|-Infinity,-2");
    });
}

#[test]
fn abort_undoes_rows_stores_and_a_new_database() {
    big(|| {
        let mut b = at("https://web.example/", &WebStorage::default());
        let got = run(
            &mut b,
            "var r = indexedDB.open('u', 1);
             r.onupgradeneeded = function () { var s = r.result.createObjectStore('s', { autoIncrement: true }); s.put('a'); s.put('b') };
             r.onsuccess = function () {
               var db = r.result, tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s');
               s.put('c'); s.delete(1); s.put('B', 2);
               s.count().onsuccess = function (e) { out.push('in tx ' + e.target.result); tx.abort() };
               tx.onabort = function () {
                 var t2 = db.transaction('s', 'readwrite'), s2 = t2.objectStore('s');
                 s2.getAll().onsuccess = function (e) { out.push(e.target.result.join()) };
                 s2.add('d').onsuccess = function (e) { out.push('key ' + e.target.result) };
                 t2.oncomplete = function () {
                   // An aborted upgrade of a new database leaves no database.
                   var r2 = indexedDB.open('gone', 1);
                   r2.onupgradeneeded = function () { r2.result.createObjectStore('x'); r2.transaction.abort() };
                   r2.onerror = function (e) { e.preventDefault(); indexedDB.databases().then(function (l) { out.push(l.map(function (d) { return d.name + d.version }).join()) }) };
                 };
               };
             };",
        );
        assert_eq!(got, "in tx 2|a,b|key 3|u1");
    });
}

#[test]
fn cursors_walk_from_their_position() {
    big(|| {
        let mut b = at("https://web.example/", &WebStorage::default());
        let got = run(
            &mut b,
            "var r = indexedDB.open('c', 1);
             r.onupgradeneeded = function () { r.result.createObjectStore('s', { keyPath: 'id' }).createIndex('t', 'tags', { multiEntry: true }) };
             r.onsuccess = function () {
               var tx = r.result.transaction('s', 'readwrite'), s = tx.objectStore('s');
               s.put({ id: 1, tags: ['x', 'y'] }); s.put({ id: 2, tags: ['y'] }); s.put({ id: 3, tags: ['x', 'x'] });
               var seen = [];
               s.index('t').openCursor(null, 'next').onsuccess = function (e) {
                 var c = e.target.result;
                 if (!c) { out.push(seen.join(' ')); return }
                 seen.push(c.key + c.primaryKey);
                 if (c.primaryKey === 1 && c.key === 'x') c.delete();
                 c.continue();
               };
               var back = [];
               s.openCursor(null, 'prev').onsuccess = function (e) {
                 var c = e.target.result;
                 if (!c) { out.push(back.join(' ')); return }
                 back.push(c.key);
                 c.advance(2);
               };
               tx.oncomplete = function () { out.push('done') };
             };",
        );
        assert_eq!(got, "3|x1 x3 y2|done");
    });
}

//! The IndexedDB log of a profile (`browser_idb_log.rs`): a new process opens the data of the last one. A cut last frame loses
//! only the last commit. An abort and a read-only transaction write nothing. A long log becomes a snapshot.
//!
//! Binary keys are not here: `shims/idb.js` refuses them today (`valid`). The log writes key bytes as they are, so a binary key
//! takes the same path as a string key.

use crate::browser::*;
use crate::browser_navigation::WebStorage;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::profile::{Profile, Saver};
use bare_html::tree;
use std::path::{Path, PathBuf};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-idb-log-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The log file of the test origin.
fn log(d: &Path) -> PathBuf {
    let mut files: Vec<PathBuf> = std::fs::read_dir(d.join("idb")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files.len(), 1, "{files:?}");
    files.pop().unwrap()
}

/// A process that opens the profile in `d`, runs `src` and its requests, and gives `out.join('|')`. The page, the storage and the
/// profile end at return, as at a close.
fn session(d: &Path, src: &str) -> String {
    let (d, src) = (d.to_path_buf(), src.to_string());
    big(move || {
        let store = WebStorage::default();
        let saver = Saver::new(Profile::open(&d).unwrap(), store.clone());
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock_at(1000.0).unwrap();
        // `crypto.subtle` comes with the fetch setup.
        b.configure_fetch(crate::browser_fetch::FetchConfig { base_url: Some("https://web.example/".into()), client: bare_net::http::Client::new(), limits: Default::default() }).unwrap();
        b.set_document_url("https://web.example/").unwrap();
        b.attach_storage(store);
        let got = match b.run_script(&format!("var out = [];\n{src}")) {
            Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
            Err(e) => format!("other:{e:?}"),
            Ok(_) => {
                for _ in 0..20 {
                    b.drain().unwrap();
                    b.advance_time(100.0, 10_000).unwrap();
                }
                let v = b.run_script("out.join('|')").unwrap();
                b.it.display(v)
            }
        };
        drop(b);
        drop(saver);
        got
    })
}

#[test]
fn a_new_process_reads_the_saved_rows_and_index() {
    let d = dir("restart");
    let put = "var r = indexedDB.open('d', 1);
      r.onupgradeneeded = function () { r.result.createObjectStore('s').createIndex('n', 'n') };
      r.onsuccess = async function () {
        var k = await crypto.subtle.importKey('raw', new Uint8Array(16), 'AES-GCM', false, ['encrypt']);
        var tx = r.result.transaction('s', 'readwrite'), s = tx.objectStore('s');
        s.put({ n: 1, t: 'h\\u00e9llo \\ud83d\\ude00' }, 'str');
        s.put({ n: 2, x: -0.5 }, 42);
        s.put({ n: 3, d: new Date(5) }, new Date(7));
        s.put({ n: 4, b: new Blob(['blob!'], { type: 'text/x' }), u: new Uint8Array([1, 2]) }, 'blob');
        s.put({ n: 5, k: k }, [1, 'a']);
        tx.oncomplete = function () { out.push('ok') };
      };";
    assert_eq!(session(&d, put), "ok");
    let get = "var r = indexedDB.open('d', 1);
      r.onupgradeneeded = function () { out.push('upgrade') };
      r.onsuccess = function () {
        var s = r.result.transaction('s').objectStore('s');
        s.getAllKeys().onsuccess = function (e) {
          out.push(e.target.result.map(function (k) { return k instanceof Date ? 'D' + +k : Array.isArray(k) ? '[' + k + ']' : String(k) }).join(' '));
        };
        s.get('str').onsuccess = function (e) { out.push(e.target.result.t) };
        s.get(42).onsuccess = function (e) { out.push(e.target.result.x) };
        s.get(new Date(7)).onsuccess = function (e) { out.push(+e.target.result.d) };
        s.index('n').getKey(4).onsuccess = function (e) { out.push(e.target.result) };
        s.index('n').count().onsuccess = function (e) { out.push(e.target.result) };
        s.get('blob').onsuccess = function (e) {
          var b = e.target.result.b;
          out.push(b.type, b.size, e.target.result.u.join('.'));
          b.text().then(function (t) { out.push(t) });
        };
        s.get([1, 'a']).onsuccess = function (e) { var k = e.target.result.k; out.push(k.type, k.algorithm.name, k.extractable, k.usages.join()) };
      };";
    assert_eq!(session(&d, get), "42 D7 blob str [1,a]|h\u{e9}llo \u{1f600}|-0.5|5|blob|5|text/x|5|1.2|blob!|secret|AES-GCM|false|encrypt");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.join("idb")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(log(&d)).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(d);
}

/// Open `d` (version 1, store `s`) and put `v` at key `k` in a transaction of its own.
fn put(k: u32, v: &str) -> String {
    format!(
        "var r = indexedDB.open('d', 1);
         r.onupgradeneeded = function () {{ r.result.createObjectStore('s') }};
         r.onsuccess = function () {{ var tx = r.result.transaction('s', 'readwrite'); tx.objectStore('s').put('{v}', {k}); tx.oncomplete = function () {{ out.push('ok') }} }};"
    )
}

const ALL: &str = "var r = indexedDB.open('d', 1);
  r.onsuccess = function () { r.result.transaction('s').objectStore('s').getAll().onsuccess = function (e) { out.push(e.target.result.join()) } };";

#[test]
fn a_cut_last_frame_loses_only_the_last_commit() {
    let d = dir("cut");
    assert_eq!(session(&d, &put(1, "a")), "ok");
    assert_eq!(session(&d, &put(2, "b")), "ok");
    let f = log(&d);
    let n = std::fs::metadata(&f).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&f).unwrap().set_len(n - 3).unwrap();
    assert_eq!(session(&d, ALL), "a");
    // The replay cut the torn frame off, so a later commit is read after the first one.
    assert_eq!(session(&d, &put(3, "c")), "ok");
    assert_eq!(session(&d, ALL), "a,c");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_abort_and_a_read_only_transaction_write_nothing() {
    let d = dir("abort");
    assert_eq!(session(&d, &put(1, "a")), "ok");
    let before = std::fs::read(log(&d)).unwrap();
    let src = "var r = indexedDB.open('d', 1);
      r.onsuccess = function () {
        var db = r.result;
        db.transaction('s').objectStore('s').get(1).onsuccess = function (e) { out.push(e.target.result) };
        var tx = db.transaction('s', 'readwrite');
        tx.objectStore('s').put('x', 2);
        tx.objectStore('s').delete(1);
        tx.onabort = function () { out.push('aborted') };
        tx.objectStore('s').count().onsuccess = function () { tx.abort() };
      };";
    assert_eq!(session(&d, src), "a|aborted");
    assert_eq!(std::fs::read(log(&d)).unwrap(), before);
    assert_eq!(session(&d, ALL), "a");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_long_log_becomes_a_snapshot() {
    let d = dir("snapshot");
    // 300 commits of 1 KiB to one key: the log is about 300 KiB, the live data 2 KiB.
    let src = "var r = indexedDB.open('d', 1), big = 'x'.repeat(1000);
      r.onupgradeneeded = function () { r.result.createObjectStore('s').put('keep', 'k') };
      r.onsuccess = function () {
        var i = 0;
        (function next() {
          var tx = r.result.transaction('s', 'readwrite');
          tx.objectStore('s').put(big + i, 1);
          tx.oncomplete = function () { if (++i < 300) next(); else out.push('ok') };
        })();
      };";
    assert_eq!(session(&d, src), "ok");
    let n = std::fs::metadata(log(&d)).unwrap().len();
    assert!(n < 8 << 10, "the log has {n} bytes");
    let got = session(&d, "var r = indexedDB.open('d', 1);
      r.onsuccess = function () { r.result.transaction('s').objectStore('s').getAll().onsuccess = function (e) { out.push(e.target.result.map(function (v) { return v.length > 100 ? v.slice(1000) : v }).join()) } };");
    assert_eq!(got, "299,keep");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_deleted_database_and_an_upgrade_survive_a_restart() {
    let d = dir("schema");
    let src = "var a = indexedDB.open('a', 1);
      a.onupgradeneeded = function () { var s = a.result.createObjectStore('s'); s.put(1, 1); s.createIndex('i', 'x') };
      a.onsuccess = function () {
        a.result.close();
        var b = indexedDB.open('b', 1);
        b.onupgradeneeded = function () { b.result.createObjectStore('gone').put(1, 1) };
        b.onsuccess = function () {
          b.result.close();
          indexedDB.deleteDatabase('b').onsuccess = function () {
            var a2 = indexedDB.open('a', 2);
            a2.onupgradeneeded = function () {
              a2.result.deleteObjectStore('s');
              var t = a2.result.createObjectStore('t', { autoIncrement: true });
              t.createIndex('v', 'v', { unique: true });
              t.add({ v: 'x' });
            };
            a2.onsuccess = function () { out.push('ok') };
          };
        };
      };";
    assert_eq!(session(&d, src), "ok");
    let got = session(&d, "indexedDB.databases().then(function (l) {
        out.push(l.map(function (x) { return x.name + x.version }).join());
        var a = indexedDB.open('a');
        a.onsuccess = function () {
          var db = a.result;
          out.push(db.version, Array.from(db.objectStoreNames).join());
          var tx = db.transaction('t', 'readwrite'), t = tx.objectStore('t');
          t.add({ v: 'y' }).onsuccess = function (e) { out.push(e.target.result) };
          t.add({ v: 'x' }).onerror = function (e) { e.preventDefault(); out.push(e.target.error.name) };
          t.index('v').getKey('x').onsuccess = function (e) { out.push(e.target.result) };
        };
      });");
    assert_eq!(got, "a2|2|t|2|ConstraintError|1");
    let _ = std::fs::remove_dir_all(d);
}

/// Design section 7: a `WebAssembly.Module` has no disk form.
#[test]
fn a_wasm_module_does_not_go_into_a_saved_store() {
    let d = dir("wasm");
    let src = "var r = indexedDB.open('d', 1);
      r.onupgradeneeded = function () { r.result.createObjectStore('s') };
      r.onsuccess = function () {
        var m = new WebAssembly.Module(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]));
        var tx = r.result.transaction('s', 'readwrite'), q = tx.objectStore('s').put({ m: m }, 1);
        // Gate: the request fails, where the standard throws at the call. The host checks the record after the call returns.
        q.onsuccess = function () { out.push('stored') };
        q.onerror = function () { out.push(q.error.name) };
        tx.onabort = function () { out.push('aborted') };
      };";
    assert_eq!(session(&d, src), "DataCloneError|aborted");
    let _ = std::fs::remove_dir_all(d);
}

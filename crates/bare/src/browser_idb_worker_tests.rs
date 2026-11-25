//! IndexedDB in dedicated workers (design-indexeddb-store step 3, design-persistent-profile step 4). A worker of an origin uses the
//! store of its page: a worker on the page thread (the default) and a worker on its own thread (an isolated agent). With a profile,
//! the writes of a worker go to the log of the origin.

use crate::browser::*;
use crate::browser_loader::{PageConfig, PageLimits};
use crate::browser_navigation::WebStorage;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::profile::{Profile, Saver};
use bare_net::http::Client;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const URL: &str = "https://web.example/";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-idb-worker-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Run the page `html` at `URL` until its script sets `done`, and give `done`. `isolated`: an isolated agent, so a worker runs on
/// its own thread. `profile`: the storage is saved in that directory; the page, the storage and the profile end at return.
fn page(html: &str, isolated: bool, profile: Option<&Path>) -> String {
    let (html, profile) = (html.to_string(), profile.map(Path::to_path_buf));
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
        let store = WebStorage::default();
        let saver = profile.map(|d| Saver::new(Profile::open(&d).unwrap(), store.clone()));
        let eval = EvalLimits { max_steps: 50_000_000, ..Default::default() };
        let mut b = Browser::new(bare_html::tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap();
        b.set_document_url(URL).unwrap();
        b.attach_storage(store);
        if isolated {
            b.attach_isolated_agent(Default::default()).unwrap();
        }
        let client = Client::new();
        let limits = PageLimits { quiet: Duration::ZERO, ..Default::default() };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let _wake = bare_net::parallel::on_end(move || {
            let _ = send.try_send(());
        });
        b.run_page(PageConfig { url: URL.into(), client: client.fork(), limits: limits.clone(), fetch: Some((client.fork(), Default::default())) });
        let deadline = Instant::now() + Duration::from_secs(20);
        let got = loop {
            let v = b.run_script("done").unwrap();
            let s = b.it.display(v);
            if !s.is_empty() {
                break s;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break "timeout".into();
            }
            let _ = receive.recv_timeout(left.min(Duration::from_millis(10)));
            b.resume_page(client.fork(), limits.clone(), &[]);
        };
        drop(b);
        drop(saver);
        got
    }).unwrap().join().unwrap()
}

/// A page with the script `src` and the worker `worker` (a `data:` worker has the origin of the page). `fail(e)` ends the page with
/// the error.
fn with_worker(src: &str, worker: &str) -> String {
    let w = format!("data:text/javascript,{}", url_escape(worker));
    format!("<script>var done = ''; function fail(e) {{ done = 'error:' + (e && e.name) + ':' + (e && e.message) }}
      var w = new Worker({w:?}); w.onerror = function () {{ done = 'worker error' }};\n{src}</script>")
}

fn url_escape(s: &str) -> String {
    s.bytes().map(|c| if c.is_ascii_alphanumeric() { (c as char).to_string() } else { format!("%{c:02X}") }).collect()
}

/// Put a row with a `CryptoKey` and a `Blob` at key `w` of store `s` of database `d`, then call `then()`.
const PUT: &str = "function put(text, then) {
    var r = indexedDB.open('d', 1);
    r.onupgradeneeded = function () { r.result.createObjectStore('s') };
    r.onerror = function () { then('open:' + r.error) };
    r.onsuccess = async function () {
      try {
        var k = await crypto.subtle.importKey('raw', new Uint8Array(16), 'AES-GCM', false, ['encrypt']);
        var tx = r.result.transaction('s', 'readwrite');
        tx.objectStore('s').put({ k: k, b: new Blob([text], { type: 'text/x' }) }, 'w');
        tx.oncomplete = function () { r.result.close(); then('put') };
      } catch (e) { then(e.name + ':' + e.message) }
    };
  }";

/// Read the row of `PUT` and give its parts to `then` as one string.
const GET: &str = "function get(then) {
    var r = indexedDB.open('d', 1);
    r.onerror = function () { then('open:' + r.error) };
    r.onsuccess = function () {
      try {
        r.result.transaction('s').objectStore('s').get('w').onsuccess = function (e) {
          var v = e.target.result;
          if (!v) { then('no row'); return }
          v.b.text().then(function (t) { then([v.k.type, v.k.algorithm.name, v.k.usages.join(), v.b.type, t].join('|')) });
        };
      } catch (e) { then(e.name + ':' + e.message) }
    };
  }";

fn worker_puts_page_gets(isolated: bool) -> String {
    let worker = format!("{PUT}\nput('from worker', postMessage)");
    let src = format!("{GET}\nw.onmessage = function (e) {{ if (e.data !== 'put') done = 'worker:' + e.data; else get(function (v) {{ done = v }}) }}");
    page(&with_worker(&src, &worker), isolated, None)
}

fn page_puts_worker_gets(isolated: bool) -> String {
    let worker = format!("{GET}\nonmessage = function () {{ get(postMessage) }}");
    let src = format!("{PUT}\nw.onmessage = function (e) {{ done = e.data }};\nput('from page', function (m) {{ if (m !== 'put') done = 'page:' + m; else w.postMessage('go') }})");
    page(&with_worker(&src, &worker), isolated, None)
}

#[test]
fn a_worker_on_the_page_thread_shares_the_store_of_its_page() {
    assert_eq!(worker_puts_page_gets(false), "secret|AES-GCM|encrypt|text/x|from worker");
    assert_eq!(page_puts_worker_gets(false), "secret|AES-GCM|encrypt|text/x|from page");
}

#[test]
fn a_worker_on_its_own_thread_shares_the_store_of_its_page() {
    assert_eq!(worker_puts_page_gets(true), "secret|AES-GCM|encrypt|text/x|from worker");
    assert_eq!(page_puts_worker_gets(true), "secret|AES-GCM|encrypt|text/x|from page");
}

#[test]
fn the_write_of_a_worker_survives_a_restart() {
    for isolated in [false, true] {
        let d = dir(&format!("restart-{isolated}"));
        let worker = format!("{PUT}\nput('saved by worker', postMessage)");
        let src = "w.onmessage = function (e) { done = e.data }";
        assert_eq!(page(&with_worker(src, &worker), isolated, Some(&d)), "put");
        let read = format!("<script>var done = '';\n{GET}\nget(function (v) {{ done = v }})</script>");
        assert_eq!(page(&read, false, Some(&d)), "secret|AES-GCM|encrypt|text/x|saved by worker", "isolated: {isolated}");
        let _ = std::fs::remove_dir_all(d);
    }
}

/// `run(side, then)`: 40 write transactions of 10 rows each in the keys `[side, i, j]`, one after the other; each fifth one aborts.
const RUN: &str = "function run(side, then) {
    var r = indexedDB.open('c');
    r.onerror = function () { then('open:' + r.error) };
    r.onsuccess = function () {
      var db = r.result, i = 0;
      (function next() {
        if (i === 40) { db.close(); then('ran'); return }
        var tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s'), n = i++;
        for (var j = 0; j < 10; j++) s.put({ side: side, n: n * 10 + j }, [side, n, j]);
        if (n % 5 === 4) { tx.onabort = next; tx.abort() } else tx.oncomplete = next;
      })();
    };
  }";

/// The counts of the store of `RUN` and of its index, as `rows|side 0|side 1|keys of side 0 in order`.
const COUNT: &str = "function count(then) {
    var r = indexedDB.open('c');
    r.onsuccess = function () {
      var s = r.result.transaction('s').objectStore('s'), ix = s.index('side'), out = [];
      s.count().onsuccess = function (e) { out[0] = e.target.result };
      ix.count(0).onsuccess = function (e) { out[1] = e.target.result };
      ix.count(1).onsuccess = function (e) { out[2] = e.target.result };
      ix.getAll(0).onsuccess = function (e) {
        var ok = e.target.result.every(function (v, k) { return v.side === 0 && v.n % 10 === k % 10 && Math.floor(v.n / 10) % 5 !== 4 });
        out[3] = ok && e.target.result.length;
        then(out.join('|'));
      };
    };
  }";

#[test]
fn concurrent_write_transactions_of_a_page_and_a_worker_keep_the_store_consistent() {
    let d = dir("concurrent");
    // The page makes the database; then the page and the worker write at once.
    let worker = format!("{RUN}\nonmessage = function () {{ run(1, postMessage) }}");
    let src = format!("{RUN}\n{COUNT}\nvar left = 2;
      function one(m) {{ if (m !== 'ran') done = 'run:' + m; else if (--left === 0) count(function (c) {{ done = c }}) }}
      w.onmessage = function (e) {{ one(e.data) }};
      var r = indexedDB.open('c', 1);
      r.onupgradeneeded = function () {{ r.result.createObjectStore('s').createIndex('side', 'side') }};
      r.onsuccess = function () {{ r.result.close(); w.postMessage('go'); run(0, one) }};");
    assert_eq!(page(&with_worker(&src, &worker), true, Some(&d)), "640|320|320|320");
    let read = format!("<script>var done = '';\n{COUNT}\ncount(function (c) {{ done = c }})</script>");
    assert_eq!(page(&read, false, Some(&d)), "640|320|320|320");
    let _ = std::fs::remove_dir_all(d);
}

/// No locks between connections yet (design-indexeddb-store step 4): an upgrade in the page while a worker keeps a connection open
/// does not wait. The worker gets no `versionchange` event and the page no `blocked` event.
#[test]
fn an_upgrade_in_the_page_while_a_worker_holds_a_connection_does_not_wait() {
    for isolated in [false, true] {
        let worker = "var db; var r = indexedDB.open('v', 1);
          r.onupgradeneeded = function () { r.result.createObjectStore('a') };
          r.onsuccess = function () { db = r.result; db.onversionchange = function () { postMessage('versionchange') }; postMessage('open') };";
        let src = "var seen = [];
          w.onmessage = function (e) {
            if (e.data !== 'open') { seen.push(e.data); return }
            var r = indexedDB.open('v', 2);
            r.onblocked = function () { seen.push('blocked') };
            r.onupgradeneeded = function () { seen.push('upgrade'); r.result.createObjectStore('b') };
            r.onsuccess = function () { seen.push('success', [].slice.call(r.result.objectStoreNames).join()); done = seen.join('|') };
            r.onerror = function () { fail(r.error) };
          };";
        assert_eq!(page(&with_worker(src, worker), isolated, None), "upgrade|success|a,b", "isolated: {isolated}");
    }
}

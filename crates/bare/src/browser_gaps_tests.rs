//! Tests of the small DOM and web API gaps that stopped real sites in `bare-drive`: `HTMLCollection.item`, `noModule`, `hasFocus`,
//! an iterable `styleSheets`, `window.event`, `Element.animate`, `Path2D` and a minimal `indexedDB`.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let html = "<head><style>p{color:red}</style><style>b{color:blue}</style></head><body><p id=p class=c>x</p><span id=s class=c></span><canvas id=cv></canvas></body>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run the jobs and the timers that are due.
fn pump(b: &mut Browser) {
    b.drain().unwrap();
    b.advance_time(500.0, 10_000).unwrap();
}

#[test]
fn collections_have_item() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var c = document.getElementsByClassName('c'); [c.length, c.item(0).id, c.item(1).id, c.item(2), c.item(-1)].join()"), "2,p,s,,");
        assert_eq!(js(&mut b, "document.getElementsByTagName('span').item(0).id + ',' + document.body.children.item(1).id + ',' + document.scripts.item(0)"), "s,s,null");
        assert_eq!(js(&mut b, "document.body.childNodes.item(0).id + ',' + Array.isArray(c) + ',' + c.map(function (e) { return e.id }).join('')"), "p,true,ps");
        assert_eq!(js(&mut b, "c.item.length + ',' + Object.keys(c).join()"), "1,0,1");
    });
}

#[test]
fn script_has_no_module() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "'noModule' in HTMLScriptElement.prototype"), "true");
        assert_eq!(js(&mut b, "var s = document.createElement('script'); s.noModule"), "false");
        assert_eq!(js(&mut b, "s.noModule = true; [s.noModule, s.hasAttribute('nomodule')].join()"), "true,true");
        assert_eq!(js(&mut b, "s.noModule = false; [s.noModule, s.hasAttribute('nomodule')].join()"), "false,false");
    });
}

#[test]
fn document_has_focus_is_true() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof document.hasFocus + ',' + document.hasFocus()"), "function,true");
    });
}

#[test]
fn style_sheets_are_iterable() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof document.styleSheets[Symbol.iterator] + ',' + [...document.styleSheets].length"), "function,2");
        assert_eq!(js(&mut b, "var n = 0; for (var s of document.styleSheets) { n += s.cssRules.length } n"), "2");
        assert_eq!(js(&mut b, "Array.from(document.styleSheets).length"), "2");
    });
}

#[test]
fn window_event_is_the_event_in_dispatch() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof window.event + ',' + ('event' in window)"), "undefined,true");
        js(&mut b, "var seen = []; var p = document.getElementById('p'); p.addEventListener('foo', function (e) { seen.push(window.event === e); document.getElementById('s').dispatchEvent(new Event('bar')); seen.push(window.event === e) }); document.getElementById('s').addEventListener('bar', function (e) { seen.push(event === e && e.type === 'bar') }); p.dispatchEvent(new Event('foo'));");
        assert_eq!(js(&mut b, "seen.join() + ',' + typeof window.event"), "true,true,true,undefined");
        assert_eq!(js(&mut b, "window.event = 5; window.event"), "5");
    });
}

#[test]
fn animate_returns_a_minimal_animation() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var p = document.getElementById('p'); var a = p.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 100 }); [a instanceof Animation, a.playState, typeof a.cancel, typeof a.finish, typeof a.play, typeof a.pause, a.finished instanceof Promise].join()"), "true,running,function,function,function,function,true");
        js(&mut b, "var log = []; a.onfinish = function (e) { log.push('onfinish') }; a.finished.then(function (x) { log.push(x === a) }); a.play(); a.pause(); a.cancel();");
        assert_eq!(js(&mut b, "a.playState"), "idle");
        js(&mut b, "var b2 = p.animate([], 10); b2.onfinish = function () { log.push('b2') }; b2.finished.then(function () { log.push('fin') })");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "fin,b2");
        assert_eq!(js(&mut b, "document.getAnimations ? 1 : 0; b2.playState"), "finished");
    });
}

#[test]
fn path2d_is_a_constructor_with_no_op_methods() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var p = new Path2D(); var q = new Path2D('M0 0 L1 1'); var r = new Path2D(p); [typeof Path2D, p instanceof Path2D].join()"), "function,true");
        assert_eq!(js(&mut b, "['moveTo', 'lineTo', 'bezierCurveTo', 'quadraticCurveTo', 'arc', 'arcTo', 'ellipse', 'rect', 'roundRect', 'closePath', 'addPath'].map(function (m) { return p[m](new Path2D(), 1, 2, 3, 4, 5, 6) }).join('')"), "");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(p)"), "[object Path2D]");
    });
}

#[test]
fn indexeddb_stores_values_in_memory() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[typeof indexedDB, typeof IDBKeyRange, typeof indexedDB.open, 'indexedDB' in window].join()"), "object,function,function,true");
        js(
            &mut b,
            "var out = []; var req = indexedDB.open('db', 1);
             req.onupgradeneeded = function (e) { out.push('up' + e.oldVersion + e.newVersion); var db = req.result; var s = db.createObjectStore('s', { keyPath: 'id' }); out.push(db.objectStoreNames.contains('s')); db.createObjectStore('k', { autoIncrement: true }); };
             req.onsuccess = function () {
               var db = req.result; out.push(db.name + db.version);
               var tx = db.transaction(['s', 'k'], 'readwrite'); var s = tx.objectStore('s');
               s.put({ id: 2, v: 'b' }); s.put({ id: 1, v: 'a' }); s.put({ id: 3, v: 'c' });
               tx.objectStore('k').add('auto').onsuccess = function (e) { out.push('k' + e.target.result) };
               s.get(1).onsuccess = function (e) { out.push(e.target.result.v) };
               s.get(9).onsuccess = function (e) { out.push(String(e.target.result)) };
               s.count().onsuccess = function (e) { out.push('n' + e.target.result) };
               s.getAll().onsuccess = function (e) { out.push(e.target.result.map(function (x) { return x.id }).join('')) };
               s.getAll(IDBKeyRange.bound(2, 3)).onsuccess = function (e) { out.push('r' + e.target.result.length) };
               s.delete(2);
               s.getAll().onsuccess = function (e) { out.push(e.target.result.map(function (x) { return x.id }).join('')) };
               var cur = []; s.openCursor().onsuccess = function (e) { var c = e.target.result; if (c) { cur.push(c.key + c.value.v); c.continue() } else { out.push(cur.join('')) } };
               tx.oncomplete = function () { out.push('done'); };
             };",
        );
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join()"), "up01,true,db1,k1,a,undefined,n3,123,r2,13,1a3c,done");
        js(&mut b, "var o2 = []; var r2 = indexedDB.open('db'); r2.onsuccess = function () { var db = r2.result; o2.push(db.version, db.objectStoreNames.length); db.transaction('s').objectStore('s').get(3).onsuccess = function (e) { o2.push(e.target.result.v) }; db.close() };");
        pump(&mut b);
        assert_eq!(js(&mut b, "o2.join()"), "1,2,c");
        js(&mut b, "var r3 = indexedDB.deleteDatabase('db'); r3.onsuccess = function () { o2.push('del'); var r4 = indexedDB.open('db'); r4.onupgradeneeded = function (e) { o2.push('up' + e.oldVersion + r4.result.objectStoreNames.length) } };");
        pump(&mut b);
        assert_eq!(js(&mut b, "o2.join()"), "1,2,c,del,up00");
        assert_eq!(js(&mut b, "var k = IDBKeyRange.only(5); [k.lower, k.upper, IDBKeyRange.lowerBound(2, true).lowerOpen, IDBKeyRange.upperBound(7).includes(7), IDBKeyRange.bound(1, 3, false, true).includes(3)].join()"), "5,5,true,true,false");
    });
}

#[test]
fn indexeddb_errors_roll_back_and_abort() {
    big(|| {
        let mut b = browser();
        js(
            &mut b,
            "var out = []; var req = indexedDB.open('e', 2);
             req.onupgradeneeded = function () { req.result.createObjectStore('s', { keyPath: 'id' }) };
             req.onsuccess = function () {
               var db = req.result, tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s');
               s.put({ id: 1 });
               s.add({ id: 1 }).onerror = function (e) { out.push(e.target.error.name) };
               s.put({ id: 2 });
               tx.onabort = function () { out.push('abort'); var t2 = db.transaction('s'); t2.objectStore('s').count().onsuccess = function (e) { out.push('n' + e.target.result) } };
               tx.oncomplete = function () { out.push('complete') };
               try { db.transaction('s').objectStore('s').put({ id: 3 }) } catch (e) { out.push(e.name) }
               try { db.transaction('nope') } catch (e) { out.push(e.name) }
             };",
        );
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join()"), "ReadOnlyError,NotFoundError,ConstraintError,abort,n0");
        js(&mut b, "var o2 = []; var r = indexedDB.open('e', 1); r.onerror = function (e) { o2.push(r.error.name); e.preventDefault() };");
        pump(&mut b);
        assert_eq!(js(&mut b, "o2.join()"), "VersionError");
    });
}

#[test]
fn indexeddb_index_wrapper_lookup() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[IDBDatabase,IDBObjectStore,IDBIndex,IDBCursor,IDBTransaction].every(function (C) { return typeof C === 'function' && typeof C.prototype === 'object' })"), "true");
    });
}

#[test]
fn indexeddb_index_interface() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof IDBIndex"), "function");
        assert_eq!(js(&mut b, "var C = IDBIndex, p = C.prototype; [C === IDBIndex, p === IDBIndex.prototype, p.constructor === C, Object.prototype.toString.call(p)].join()"), "true,true,true,[object IDBIndex]");
        assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(globalThis, 'IDBIndex'); [d.value === C, d.writable, d.enumerable, d.configurable].join()"), "true,true,false,true");
        assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(p, Symbol.toStringTag); [d.value, d.writable, d.enumerable, d.configurable].join()"), "IDBIndex,false,false,true");
        assert_eq!(js(&mut b, "[typeof p.get, typeof p.openCursor, typeof IDBObjectStore.prototype.createIndex, typeof IDBObjectStore.prototype.index].join()"), "function,undefined,function,function");
    });
}

#[test]
fn indexeddb_index_constructor_is_illegal() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "new IDBIndex()"), "throw:TypeError:Illegal constructor");
        assert_eq!(js(&mut b, "IDBIndex()"), "throw:TypeError:Illegal constructor");
    });
}

#[test]
fn indexeddb_unused_names_stay_lazy() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "document.body.id = 'sentinel'; Object.getOwnPropertyNames(globalThis).filter(function (n) { return n === 'indexedDB' || n.startsWith('IDB') }).join()"), "");
    });
}

#[test]
fn indexeddb_github_expiration_wrapper() {
    big(|| {
        let mut b = browser();
        js(&mut b, r###"function r(e){return new Promise((t,a)=>{e.onsuccess=()=>t(e.result),e.onerror=()=>a(e.error)})}
let o="expiresAt",c=(e,t)=>`${e}::${t}`;
function s(e,t){let a,n=c(e,t),i=async(e,i)=>i((await function(){if(a)return a;let e=globalThis.indexedDB.open(n);e.onupgradeneeded=()=>e.result.createObjectStore(t).createIndex(o,"state.expiresAt");let i=r(e);return a=i,(async()=>{try{let e=await i,t=()=>{a===i&&(a=void 0)};e.onclose=t,e.onversionchange=()=>{e.close(),t()}}catch{}})(),a}()).transaction(t,e).objectStore(t));return i.batchKey=n,i}
async function M(e){let t=e.index(o).getAllKeys(IDBKeyRange.upperBound(Date.now(),!0),250),a=e.count(),[n,i]=await Promise.all([r(t),r(a)]);return{expiredKeys:n,totalEntries:i}}
var wrapperSentinel = false; var failure = ''; var access=s('regression','queries');
access('readwrite', async function(st){await r(st.put({state:{expiresAt:1}},'expired')); await r(st.put({state:{expiresAt:9007199254740991}},'live'));}).then(function(){return access('readonly',M)}).then(function(x){wrapperSentinel=x.expiredKeys.join()==='expired' && x.totalEntries===2;}).catch(function(e){failure=e.name+':'+e.message});
"###);
        pump(&mut b);
        assert_eq!(js(&mut b, "String(wrapperSentinel) + failure"), "true");
    });
}

#[test]
fn indexeddb_indexes_follow_rows_and_rollback() {
    big(|| {
        let mut b = browser();
        js(&mut b, r#"
var out=[], db, saved; var req=indexedDB.open('indexes',1);
req.onupgradeneeded=function(){var s=req.result.createObjectStore('s'); saved=s.createIndex('v','v',{unique:true}); s.createIndex('tags','tags',{multiEntry:true});s.createIndex('pair',['v','n']);
s.put({v:'b',n:1,tags:['x','x','y']},2);s.put({v:'a',n:2,tags:['x']},1);};
req.onsuccess=function(){db=req.result;var tx=db.transaction('s','readwrite'),s=tx.objectStore('s');
out.push(s.indexNames.join(),s.index('v') instanceof IDBIndex,s.index('v').objectStore===s);
function read(r){r.onsuccess=function(){out.push(JSON.stringify(r.result));};}
read(s.index('v').getAllKeys());read(s.index('tags').getAllKeys('x'));read(s.index('pair').getKey(['b',1]));
s.put({v:'c',n:3},1);read(s.index('v').getKey('a'));read(s.index('v').get('c'));
s.delete(2);read(s.index('tags').count());s.clear();read(s.index('v').count());
tx.oncomplete=function(){out.push('done');};};
"#);
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join('|')"), "pair,tags,v|true|true|[1,2]|[1,2]|2||{\"v\":\"c\",\"n\":3}|0|0|done");
        js(&mut b, r#"
var tx=db.transaction('s','readwrite'),s=tx.objectStore('s');s.put({v:'same'},1);s.put({v:'same'},2);tx.onabort=function(){out.push(tx.error.name);var t=db.transaction('s');t.objectStore('s').index('v').count().onsuccess=function(e){out.push(e.target.result);};};
"#);
        pump(&mut b);
        assert_eq!(js(&mut b, "out.slice(-2).join()"), "ConstraintError,0");
        assert_eq!(js(&mut b, r#"var errors=[];var t=db.transaction('s'),s=t.objectStore('s');for(var f of [function(){s.index('missing')},function(){s.createIndex('x','v')},function(){IDBObjectStore.prototype.index.call({},'v')},function(){IDBIndex.prototype.count.call({})}]){try{f()}catch(e){errors.push(e.name)}}errors.join()"#), "NotFoundError,InvalidStateError,TypeError,TypeError");
        pump(&mut b);
        js(&mut b, r#"
var upgrade=indexedDB.open('indexes',2);upgrade.onupgradeneeded=function(){var s=upgrade.transaction.objectStore('s');s.put({v:'z'},1);s.put({v:'z'},2);s.createIndex('bad','v',{unique:true});};upgrade.onerror=function(e){e.preventDefault();out.push(upgrade.error.name);};
"#);
        pump(&mut b);
        assert_eq!(js(&mut b, "out[out.length-1]"), "AbortError");
    });
}

#[test]
fn indexeddb_index_upgrade_existing_records() {
    big(|| {
        let mut b = browser();
        js(&mut b, r#"
var out=[],r=indexedDB.open('existing',1);r.onupgradeneeded=function(){var s=r.result.createObjectStore('s');s.put({v:'z',n:1},2);s.put({v:'a',n:1},1);};r.onsuccess=function(){r.result.close();var u=indexedDB.open('existing',2);u.onupgradeneeded=function(){var s=u.transaction.objectStore('s');var ix=s.createIndex('v','v');out.push(ix===s.index('v'),s.indexNames.contains('v'));ix.getAllKeys().onsuccess=function(e){out.push(e.target.result.join());};try{s.createIndex('v','v')}catch(e){out.push(e.name)}try{s.createIndex('bad',['v'],{multiEntry:true})}catch(e){out.push(e.name)};};u.onsuccess=function(){u.result.close();var a=indexedDB.open('existing',3);a.onupgradeneeded=function(){var s=a.transaction.objectStore('s');s.deleteIndex('v');s.createIndex('new','v');a.transaction.abort();};a.onerror=function(e){e.preventDefault();var b=indexedDB.open('existing');b.onsuccess=function(){out.push(b.result.version,b.result.transaction('s').objectStore('s').indexNames.join());};};};};
"#);
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join('|')"), "true|true|ConstraintError|InvalidAccessError|1,2|2|v");
        js(&mut b, r#"var u=indexedDB.open('existing',3);u.onupgradeneeded=function(){u.transaction.objectStore('s').createIndex('n','n',{unique:true});};u.onerror=function(e){e.preventDefault();out.push(u.error.name);var r=indexedDB.open('existing');r.onsuccess=function(){out.push(r.result.version,r.result.transaction('s').objectStore('s').indexNames.join());};};"#);
        pump(&mut b);
        assert_eq!(js(&mut b, "out.slice(-3).join()"), "AbortError,2,v");
        assert_eq!(js(&mut b, "try { Object.getOwnPropertyDescriptor(IDBIndex.prototype, 'keyPath').get.call({}) } catch(e) { e.name }"), "TypeError");
    });
}

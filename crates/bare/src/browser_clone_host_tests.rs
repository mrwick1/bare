//! Tests of the structured clone of platform objects with data: `Blob`, `File` and `CryptoKey` (HTML serializable objects).

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
    let html = "<head><style>p{color:red}</style><style>b{color:blue}</style></head><body><p id=p class=c>x</p><span id=s class=c></span><canvas id=cv></canvas></body>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
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

/// Run the jobs and the timers that are due.
fn pump(b: &mut Browser) {
    b.drain().unwrap();
    b.advance_time(500.0, 10_000).unwrap();
}


#[test]
fn clone_keeps_blob_and_file_data() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var r = ''; var bl = structuredClone(new Blob(['hi'], {type:'text/plain'})); var f = structuredClone(new File(['abc'], 'a.txt', {type:'x/y', lastModified: 7})); Promise.all([bl.text(), f.text()]).then(function (t) { r = [bl instanceof Blob, bl.size, bl.type, t[0], f instanceof File, f.name, f.lastModified, f.type, t[1]].join(); });");
        pump(&mut b);
        assert_eq!(js(&mut b, "r"), "true,2,text/plain,hi,true,a.txt,7,x/y,abc");
        // The copy in an object graph and in a message keeps the same data.
        assert_eq!(js(&mut b, "var o = structuredClone({a: [bl, bl]}); [o.a[0] === o.a[1], o.a[0] !== bl, o.a[0].size].join()"), "true,true,2");
    })
}

#[test]
fn clone_keeps_a_crypto_key_usable() {
    big(|| {
        // `crypto.subtle` needs a secure context.
        let mut b = crate::browser_fetch_tests::mk(Some("https://example.com/".into()), crate::browser_fetch::FetchLimits::default());
        b.use_virtual_clock_at(1000.0).unwrap();
        js(&mut b, "var r = ''; var c = crypto.subtle, iv = new Uint8Array(16); c.generateKey({name:'AES-CBC', length:256}, false, ['encrypt','decrypt']).then(function (k) { var k2 = structuredClone(k); r = [Object.getPrototypeOf(k2) === Object.getPrototypeOf(k), k2 !== k, k2.algorithm.name, k2.algorithm.length, k2.extractable, k2.usages.join('+')].join(); return c.encrypt({name:'AES-CBC', iv: iv}, k, new Uint8Array([1,2,3])).then(function (ct) { return c.decrypt({name:'AES-CBC', iv: iv}, k2, ct); }); }).then(function (p) { r += '|' + new Uint8Array(p).join(); }, function (e) { r += '|' + e.name + ':' + e.message; });");
        pump(&mut b);
        assert_eq!(js(&mut b, "r"), "true,true,AES-CBC,256,false,encrypt+decrypt|1,2,3");
    })
}

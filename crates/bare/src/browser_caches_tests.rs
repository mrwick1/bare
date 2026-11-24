//! Tests of the in-memory `caches` (CacheStorage) shim, `shims/caches.js`.

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
fn caches_open_put_and_match_round_trips() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[typeof fetch, typeof Response, typeof caches, typeof CacheStorage, typeof Cache].join()"), "function,function,object,function,function");
        js(&mut b, "var out = []; caches.open('v1').then(c => c.put(new Request('https://x/a'), new Response('hi'))).then(() => caches.open('v1')).then(c => c.match('https://x/a')).then(r => r.text()).then(t => out.push(t)); caches.open('v1').then(c => c.match('https://x/none')).then(r => out.push(String(r))); caches.open('v1').then(c => c.put('ftp://x/a', new Response('z'))).catch(e => out.push(e.name));");
        pump(&mut b);
        let out = js(&mut b, "out.join()");
        assert!(out.contains("hi"), "{out}");
        assert!(out.contains("undefined"), "{out}");
        assert!(out.contains("TypeError"), "{out}");
    });
}

#[test]
fn caches_has_keys_and_delete() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var out = []; caches.open('v1').then(c => c.put('https://x/a', new Response('hi')).then(() => c.keys())).then(k => out.push(k.map(r => r.url).join())).then(() => caches.has('v1')).then(h => out.push(h)).then(() => caches.keys()).then(k => out.push(k.join())).then(() => caches.delete('v1')).then(d => out.push(d)).then(() => caches.has('v1')).then(h => out.push(h));");
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join('|')"), "https://x/a|true|v1|true|false");
    });
}

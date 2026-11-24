//! Tests of `navigator.serviceWorker` (`shims/serviceworker.js`): a container with no registrations, as in Ladybird.

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
fn service_worker_container_has_no_registrations() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "['serviceWorker' in navigator, typeof navigator.serviceWorker, navigator.serviceWorker === navigator.serviceWorker, navigator.serviceWorker instanceof ServiceWorkerContainer, navigator.serviceWorker.controller, typeof ServiceWorkerRegistration, typeof ServiceWorker].join()"), "true,object,true,true,,function,function");
        js(&mut b, "var out = []; const sw = navigator.serviceWorker; sw.getRegistration().then(r => out.push('one:' + r)); sw.getRegistrations().then(r => out.push('all:' + r.length)); sw.register('/sw.js').catch(e => out.push('reg:' + e.name)); let ready = 'pending'; sw.ready.then(() => ready = 'resolved');");
        pump(&mut b);
        assert_eq!(js(&mut b, "out.sort().join('|') + '|' + ready"), "all:0|one:undefined|reg:NotSupportedError|pending");
        assert_eq!(js(&mut b, "try { new ServiceWorkerContainer(); 'made' } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "let n = 0; navigator.serviceWorker.addEventListener('message', () => n++); navigator.serviceWorker.dispatchEvent(new Event('message')); n + ':' + Object.prototype.toString.call(navigator.serviceWorker)"), "1:[object ServiceWorkerContainer]");
    });
}

#[test]
fn service_worker_globals_come_first_and_match_the_container() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[typeof ServiceWorkerRegistration, Object.keys(window).includes('ServiceWorker'), Object.getPrototypeOf(navigator.serviceWorker) === ServiceWorkerContainer.prototype, ServiceWorkerContainer.prototype instanceof EventTarget].join()"), "function,false,true,true");
    });
}

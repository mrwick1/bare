//! Tests of the global of a dedicated worker (`browser_worker.rs`, `install_scope`; `design-workers.md`, step 3): the interface names
//! `WorkerGlobalScope` and `DedicatedWorkerGlobalScope`, and the prototype chain of the global. WhatsApp Web checks
//! `self instanceof DedicatedWorkerGlobalScope` in its workers and stops when it is false.

use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// The value that the worker `/w.js` posts first, as the page logs it in `localStorage`.
fn worker_says(worker: &'static str) -> Option<String> {
    let srv = serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html; charset=utf-8"), "<script>new Worker('/w.js').onmessage = function(e){ localStorage.setItem('log', e.data) }</script>".to_string()),
        "/w.js" => Reply::new(200, Some("text/javascript"), worker.to_string()),
        _ => Reply::new(404, Some("text/plain"), "no".to_string()),
    });
    let srv: Server = srv;
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    s.run();
    s.storage().get_item(false, &srv.url(""), "log")
}

#[test]
fn the_global_is_a_dedicated_worker_global_scope() {
    let got = big(|| worker_says("postMessage([typeof DedicatedWorkerGlobalScope, typeof WorkerGlobalScope, self instanceof DedicatedWorkerGlobalScope,
        self instanceof WorkerGlobalScope, self instanceof EventTarget, Object.getPrototypeOf(DedicatedWorkerGlobalScope.prototype) === WorkerGlobalScope.prototype,
        Object.prototype.toString.call(self)].join())"));
    assert_eq!(got.as_deref(), Some("function,function,true,true,true,true,[object DedicatedWorkerGlobalScope]"));
}

#[test]
fn the_interfaces_cannot_be_constructed_and_the_global_keeps_its_functions() {
    let got = big(|| worker_says("var r = []; for (var C of [WorkerGlobalScope, DedicatedWorkerGlobalScope]) { try { new C(); r.push('made') } catch (e) { r.push(e.name) } }
        r.push(typeof setTimeout, typeof fetch, typeof addEventListener, typeof atob, typeof postMessage, typeof importScripts); postMessage(r.join())"));
    assert_eq!(got.as_deref(), Some("TypeError,TypeError,function,function,function,function,function,function"));
}

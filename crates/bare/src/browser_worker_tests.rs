//! Tests of `Worker` (`browser_worker.rs`): a worker that fails to start. The rules are from the HTML Standard ("Dedicated workers
//! and the Worker interface") and from what a browser does when the worker script cannot load. No test ran in Chrome.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_document_url("https://example.test/dir/page.html").expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn pump(b: &mut Browser) {
    b.advance_time(10.0, 1000).expect("advance");
}

#[test]
fn worker_is_a_constructor_with_an_event_target_prototype() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof Worker + ',' + (Worker.prototype instanceof EventTarget) + ',' + Worker.name + ',' + Worker.length + ',' + ('Worker' in window)"), "function,true,Worker,1,true");
        assert_eq!(js(&mut b, "var w = new Worker('data:,1'); (w instanceof Worker) + ',' + (w instanceof EventTarget) + ',' + typeof w.postMessage + typeof w.terminate + ',' + w.onmessage + w.onmessageerror + w.onerror"), "true,true,functionfunction,nullnullnull");
        assert_eq!(js(&mut b, "try { Worker('data:,1'); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Worker(); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "class W extends Worker {} (new W('data:,1') instanceof W)"), "true");
    });
}

#[test]
fn a_bad_url_is_a_syntax_error_and_a_bad_type_is_a_type_error() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "try { new Worker('http://[bad'); } catch (e) { e.name + ',' + (e instanceof DOMException) + ',' + e.code; }"), "SyntaxError,true,12");
        assert_eq!(js(&mut b, "try { new Worker('data:,1', { type: 'shared' }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Worker('data:,1', 5); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "new Worker('data:,1', { type: 'module' }) instanceof Worker && new Worker('data:,1', { type: 'classic', name: 'n' }) instanceof Worker && new Worker('data:,1', null) instanceof Worker"), "true");
    });
}

#[test]
fn the_url_kinds_that_a_worker_takes() {
    big(|| {
        let mut b = browser();
        let ok = "['worker.js', '/w.js', 'https://example.test/w.js', 'data:text/javascript,1', URL.createObjectURL(new Blob(['1'])), new URL('w.js', location.href)].map(function (u) { try { new Worker(u); return 'ok'; } catch (e) { return e.name; } }).join()";
        assert_eq!(js(&mut b, ok), "ok,ok,ok,ok,ok,ok");
        assert_eq!(js(&mut b, "try { new Worker('https://other.test/w.js'); } catch (e) { e.name; }"), "SecurityError");
        assert_eq!(js(&mut b, "try { new Worker('ftp://example.test/w.js'); } catch (e) { e.name; }"), "SecurityError");
    });
}

#[test]
fn the_error_event_fires_once_after_the_current_task() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var log = []; var w = new Worker('data:,1'); w.onerror = function (e) { log.push(e.type + ':' + (e.target === w) + ':' + e.bubbles + ':' + e.cancelable + ':' + (e instanceof ErrorEvent)); }; w.addEventListener('error', function () { log.push('listener'); }); w.addEventListener('message', function () { log.push('message'); }); log.push('sync'); log.join()");
        assert_eq!(r, "sync");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sync,error:true:false:false:false,listener");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.length"), "3");
    });
}

#[test]
fn a_listener_that_the_task_finds_removed_does_not_run_and_a_throwing_one_does_not_stop_the_others() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var w = new Worker('data:,1'); var f = function () { log.push('f'); }; w.addEventListener('error', f); w.removeEventListener('error', f); w.addEventListener('error', function () { throw new Error('x'); }); w.addEventListener('error', function () { log.push('g'); });");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "g");
    });
}

#[test]
fn post_message_does_nothing_but_checks_what_it_can_clone() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var w = new Worker('data:,1'); var got = 0; w.onmessage = function () { got++; };");
        assert_eq!(js(&mut b, "[w.postMessage('a'), w.postMessage({ a: [1, 2] }), w.postMessage(1, []), w.postMessage(null, { transfer: [] })].join()"), ",,,");
        assert_eq!(js(&mut b, "try { w.postMessage(function () {}); } catch (e) { e.name + ',' + e.code; }"), "DataCloneError,25");
        assert_eq!(js(&mut b, "try { w.postMessage(Symbol('s')); } catch (e) { e.name; }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { w.postMessage(); } catch (e) { e.name; }"), "TypeError");
        pump(&mut b);
        assert_eq!(js(&mut b, "got"), "0");
    });
}

#[test]
fn terminate_stops_the_error_and_a_second_call_is_fine() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var w = new Worker('data:,1'); w.onerror = function () { log.push('error'); }; var r = [w.terminate(), w.terminate()].join(); var v = new Worker('data:,2'); v.onerror = function () { log.push('v'); };");
        pump(&mut b);
        assert_eq!(js(&mut b, "r + '|' + log.join()"), ",|v");
        assert_eq!(js(&mut b, "w.postMessage('x'); 'ok'"), "ok");
    });
}

#[test]
fn at_most_64_worker_objects_live_and_a_collected_one_frees_a_place() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var ws = []; for (var i = 0; i < 64; i++) ws.push(new Worker('data:,1')); try { new Worker('data:,1'); } catch (e) { e.name + ',' + e.code; }"), "NotSupportedError,9");
        pump(&mut b);
        assert_eq!(js(&mut b, "ws = null; 1"), "1");
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, "new Worker('data:,1') instanceof Worker"), "true");
    });
}

#[test]
fn a_page_that_catches_the_error_goes_on() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var state = 'start'; function boot() { state = 'fallback'; }\n\
            var u = URL.createObjectURL(new Blob(['self.onmessage = function () {}'], { type: 'text/javascript' }));\n\
            var w = new Worker(u); w.onerror = function (e) { boot(); }; w.postMessage({ type: 'init' }); state = 'waiting';");
        assert_eq!(js(&mut b, "state"), "waiting");
        pump(&mut b);
        assert_eq!(js(&mut b, "state"), "fallback");
    });
}

#[test]
fn one_console_warning_for_each_page_names_the_first_url() {
    big(|| {
        let mut b = browser();
        js(&mut b, "new Worker('data:,1'); new Worker('/two.js'); new Worker('data:,3');");
        let recs = b.take_console_records();
        let text: Vec<String> = recs.iter().map(|r| format!("{:?}:{}", r.level, r.args.iter().map(|&v| b.it.display(v)).collect::<Vec<_>>().join(" "))).collect();
        assert_eq!(text, vec!["Warn:Worker scripts do not run in bare (data:,1)".to_string()]);
    });
}

// ------------------------------------------------------------------------------------------ SharedWorker

#[test]
fn shared_worker_is_a_constructor_with_an_event_target_prototype_and_a_port() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof SharedWorker + ',' + (SharedWorker.prototype instanceof EventTarget) + ',' + SharedWorker.name + ',' + SharedWorker.length + ',' + ('SharedWorker' in window)"), "function,true,SharedWorker,1,true");
        assert_eq!(js(&mut b, "var w = new SharedWorker('data:,1'); (w instanceof SharedWorker) + ',' + (w instanceof EventTarget) + ',' + (w.port instanceof MessagePort) + ',' + (w.port === w.port) + ',' + typeof w.postMessage + ',' + w.onerror"), "true,true,true,true,undefined,null");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(w) + ',' + (new SharedWorker('data:,1').port !== w.port) + ',' + (w instanceof Worker)"), "[object SharedWorker],true,false");
        assert_eq!(js(&mut b, "try { SharedWorker('data:,1'); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new SharedWorker(); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "class W extends SharedWorker {} (new W('data:,1') instanceof W)"), "true");
    });
}

#[test]
fn the_port_of_a_shared_worker_takes_messages_and_answers_none() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var w = new SharedWorker('data:,1'); var got = 0; w.port.onmessage = function () { got++; }; w.port.start();");
        assert_eq!(js(&mut b, "w.port.postMessage('a'); 'ok'"), "ok");
        pump(&mut b);
        assert_eq!(js(&mut b, "got"), "0");
    });
}

#[test]
fn shared_worker_takes_a_name_or_options_and_checks_the_url() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[new SharedWorker('data:,1', 'n'), new SharedWorker('data:,1', { name: 'n', type: 'module' }), new SharedWorker('data:,1', null), new SharedWorker('data:,1', undefined)].every(function (w) { return w instanceof SharedWorker; })"), "true");
        assert_eq!(js(&mut b, "try { new SharedWorker('data:,1', { type: 'shared' }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new SharedWorker('http://[bad'); } catch (e) { e.name + ',' + e.code; }"), "SyntaxError,12");
        assert_eq!(js(&mut b, "try { new SharedWorker('https://other.test/w.js'); } catch (e) { e.name; }"), "SecurityError");
        assert_eq!(js(&mut b, "['worker.js', '/w.js', 'data:,1', new URL('w.js', location.href)].map(function (u) { try { new SharedWorker(u); return 'ok'; } catch (e) { return e.name; } }).join()"), "ok,ok,ok,ok");
    });
}

#[test]
fn shared_worker_fires_error_once_after_the_current_task() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var log = []; var w = new SharedWorker('data:,1'); w.onerror = function (e) { log.push(e.type + ':' + (e.target === w) + ':' + e.bubbles + ':' + e.cancelable); }; w.addEventListener('error', function () { log.push('listener'); }); log.push('sync'); log.join()");
        assert_eq!(r, "sync");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sync,error:true:false:false,listener");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.length"), "3");
    });
}

#[test]
fn shared_and_dedicated_workers_share_the_cap_and_do_not_mix_their_methods() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var ws = []; for (var i = 0; i < 32; i++) { ws.push(new Worker('data:,1')); ws.push(new SharedWorker('data:,1')); } try { new SharedWorker('data:,1'); } catch (e) { e.name; }"), "NotSupportedError");
        assert_eq!(js(&mut b, "try { Worker.prototype.postMessage.call(ws[1], 1); } catch (e) { e.name; }"), "TypeError");
    });
}

#[test]
fn a_page_that_tests_for_shared_worker_takes_the_modern_path() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "!window.SharedWorker ? 'es5' : 'modern'"), "modern");
    });
}

#[test]
fn a_collected_shared_worker_frees_its_place_and_its_port() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var ws = []; for (var i = 0; i < 64; i++) ws.push(new SharedWorker('data:,1')); try { new SharedWorker('data:,1'); } catch (e) { e.name; }"), "NotSupportedError");
        pump(&mut b);
        assert_eq!(js(&mut b, "ws = null; 1"), "1");
        b.collect(&[]).expect("collect");
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, "new SharedWorker('data:,1').port instanceof MessagePort"), "true");
    });
}

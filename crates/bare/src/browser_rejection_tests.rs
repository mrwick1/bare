//! Tests of `unhandledrejection`, `rejectionhandled` and the `Uncaught (in promise)` console line (`browser_rejection.rs`).

use crate::browser::*;
use crate::browser_console::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk() -> Browser {
    Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    let s = match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    };
    b.drain().unwrap();
    s
}

/// Every record as `L:text` (`L`, `W`, `E`).
fn lines(b: &mut Browser) -> Vec<String> {
    b.take_console_records()
        .into_iter()
        .map(|r| {
            let c = match r.level { ConsoleLevel::Log => 'L', ConsoleLevel::Warn => 'W', ConsoleLevel::Error => 'E' };
            format!("{c}:{}", b.format_console_record(&r).unwrap())
        })
        .collect()
}

#[test]
fn an_unhandled_rejection_fires_the_event_and_logs_one_line() {
    big(|| {
        let mut b = mk();
        let setup = "var seen = []; addEventListener('unhandledrejection', function (e) { seen.push([e.type, String(e.reason), e.promise instanceof Promise, e instanceof PromiseRejectionEvent, e.cancelable, e.bubbles].join()); if (e.reason === 2) e.preventDefault(); });
            addEventListener('rejectionhandled', function (e) { seen.push(e.type + ':' + e.reason.message + ':' + (e.promise === p1)); });
            function boom() { return new TypeError('boom'); }
            var p1 = Promise.reject(boom()); Promise.reject(2); Promise.reject('text'); Promise.reject(3).catch(function () {}); 0";
        assert_eq!(js(&mut b, setup), "0");
        assert_eq!(js(&mut b, "seen.join('|')"), "unhandledrejection,TypeError: boom,true,true,true,false|unhandledrejection,2,true,true,true,false|unhandledrejection,text,true,true,true,false");
        let l = lines(&mut b);
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(l[0].starts_with("E:Uncaught (in promise) TypeError: boom at boom (script#"), "{l:?}");
        assert_eq!(l[1], "E:Uncaught (in promise) text");
        assert_eq!(js(&mut b, "seen = []; p1.catch(function () {}); 0"), "0");
        assert_eq!(js(&mut b, "seen.join('|')"), "rejectionhandled:boom:true");
        assert!(lines(&mut b).is_empty());
    });
}

#[test]
fn the_handler_property_and_the_constructor_work() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "var n = 0; onunhandledrejection = function (e) { n++; return false; }; Promise.reject(1); 0"), "0");
        assert_eq!(js(&mut b, "n"), "1");
        // A handler property that returns false cancels the event (HTML "the event handler processing algorithm").
        assert!(lines(&mut b).is_empty());
        assert_eq!(js(&mut b, "var p = Promise.resolve(); var e = new PromiseRejectionEvent('x', { promise: p, reason: 7 }); [e.type, e.promise === p, e.reason, e.cancelable].join()"), "x,true,7,false");
        assert_eq!(js(&mut b, "try { new PromiseRejectionEvent('x', {}); 'made' } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { PromiseRejectionEvent('x', { promise: p }); 'called' } catch (e) { e.name }"), "TypeError");
        // Nothing listens: the line is logged.
        assert_eq!(js(&mut b, "onunhandledrejection = null; Promise.reject(undefined); 0"), "0");
        assert_eq!(lines(&mut b), ["E:Uncaught (in promise) undefined"]);
    });
}

#[test]
fn a_listener_that_rejects_again_is_heard_at_the_next_checkpoint() {
    big(|| {
        let mut b = mk();
        // Each checkpoint notifies once, as the standard's queued task does: the rejection inside the listener waits for the next one.
        assert_eq!(js(&mut b, "var n = 0; addEventListener('unhandledrejection', function (e) { n++; e.preventDefault(); Promise.reject(0); }); Promise.reject(1); 0"), "0");
        assert_eq!(js(&mut b, "n"), "1");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "n"), "3");
        assert!(lines(&mut b).is_empty());
    });
}

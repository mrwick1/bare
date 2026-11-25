use crate::browser::{Browser, BrowserLimits};
use crate::eng::Ops;
use crate::js_limits::{EvalLimits, HeapLimits};
use std::sync::atomic::{AtomicUsize, Ordering};

static WAKEUPS: AtomicUsize = AtomicUsize::new(0);

fn wake() {
    WAKEUPS.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn remote_notify_wakes_and_settles_on_the_browser_scheduler() {
    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let mut browser = Browser::new(
            bare_html::tree::parse("<body></body>"),
            BrowserLimits::default(), EvalLimits::default(), HeapLimits::default(),
        ).unwrap();
        browser.it.install_shared_memory(false, Some(wake));
        let buffer = browser.it.run_script_o(r#"
            var data = new SharedArrayBuffer(4);
            var view = new Int32Array(data);
            var settled = false;
            Atomics.waitAsync(view, 0, 0).value.then(value => { settled = value === "ok"; });
            data
        "#).unwrap();
        let shared = browser.it.exported_shared_buffer(buffer).unwrap();
        assert!(browser.next_scheduled_due().is_none());

        std::thread::Builder::new().stack_size(8 << 20).spawn(move || {
            let mut worker = bare_vex::script::load("").unwrap().realm;
            worker.install_shared_memory(true, None);
            let buffer = worker.import_shared_buffer(shared).unwrap();
            worker.set_own(worker.global, "buffer", buffer);
            let count = worker.run_script_o("Atomics.notify(new Int32Array(buffer), 0)").unwrap();
            assert_eq!(count.as_int(), Some(1));
        }).unwrap().join().unwrap();

        assert_eq!(WAKEUPS.load(Ordering::Relaxed), 1);
        assert!(browser.next_scheduled_due().is_some());
        assert_eq!(browser.run_scheduled_tasks(1).unwrap().ran, 1);
        assert_eq!(browser.it.run_script_o("settled").unwrap().as_bool(), Some(true));
    }).unwrap().join().unwrap();
}


#[test]
fn isolated_browser_clones_shared_memory_and_rejects_history_storage() {
    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let mut browser = Browser::new(
            bare_html::tree::parse("<body></body>"),
            BrowserLimits::default(), EvalLimits::default(), HeapLimits::default(),
        ).unwrap();
        browser.set_document_url("https://example.test/").unwrap();
        let before = browser.it.run_script_o("crossOriginIsolated === false && typeof SharedArrayBuffer === 'undefined'").unwrap();
        assert_eq!(before.as_bool(), Some(true));
        browser.attach_isolated_agent(Default::default()).unwrap();
        let result = browser.it.run_script_o(r#"
            var data = new SharedArrayBuffer(4);
            var copy = structuredClone(data);
            var view = new Int32Array(copy);
            Atomics.store(view, 0, 23);
            var historyError, waitError;
            try { history.pushState(data, ""); } catch (e) { historyError = e.name; }
            try { Atomics.wait(view, 0, 23, 0); } catch (e) { waitError = e.name; }
            crossOriginIsolated && data !== copy &&
                new Int32Array(data)[0] === 23 &&
                historyError === "DataCloneError" && waitError === "TypeError"
        "#).unwrap();
        assert_eq!(result.as_bool(), Some(true));
    }).unwrap().join().unwrap();
}

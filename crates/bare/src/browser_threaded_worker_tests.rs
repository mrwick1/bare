use crate::browser::{Browser, BrowserLimits};
use crate::browser_loader::{PageConfig, PageLimits};
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_net::http::Client;
use std::time::{Duration, Instant};

#[test]
fn isolated_worker_wait_does_not_block_its_pages_timers() {
    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let url = "https://example.test/";
        let html = r#"
            <script>
            var done = false, ticks = 0;
            var shared = new SharedArrayBuffer(4);
            var view = new Int32Array(shared);
            var worker = new Worker("data:text/javascript," + encodeURIComponent(`
                onmessage = event => {
                    const view = new Int32Array(event.data);
                    postMessage("waiting");
                    const status = Atomics.wait(view, 0, 0, 5000);
                    Atomics.store(view, 0, 42);
                    postMessage(status);
                    Atomics.wait(view, 0, 42);
                };
            `));
            worker.onmessage = event => {
                if (event.data === "waiting") {
                    var id = setInterval(() => {
                        ticks++;
                        if (Atomics.notify(view, 0) === 1) clearInterval(id);
                    }, 1);
                } else {
                    done = event.data === "ok" && ticks > 0 && Atomics.load(view, 0) === 42;
                }
            };
            worker.postMessage(shared);
            </script>
        "#;
        let mut browser = Browser::new(bare_html::tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        browser.set_document_url(url).unwrap();
        browser.attach_isolated_agent(Default::default()).unwrap();
        let client = Client::new();
        let limits = PageLimits { quiet: Duration::ZERO, ..Default::default() };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let _wake = bare_net::parallel::on_end(move || { let _ = send.try_send(()); });
        browser.run_page(PageConfig { url: url.into(), client: client.fork(), limits: limits.clone(), fetch: None });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if browser.run_script("done").unwrap().as_bool() == Some(true) { break; }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "worker did not complete its wait");
            let _ = receive.recv_timeout(remaining.min(Duration::from_millis(10)));
            browser.resume_page(client.fork(), limits.clone(), &[]);
        }
        let status = browser.dom.kids.threaded_status().expect("live worker");
        browser.run_script("worker.terminate();").unwrap();
        loop {
            if status.load(std::sync::atomic::Ordering::Acquire) != 0 { break; }
            receive.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("worker exited after termination");
        }
        assert_eq!(browser.run_script("Atomics.notify(view, 0)").unwrap().as_int(), Some(0));
    }).unwrap().join().unwrap();
}


#[test]
fn foreign_cluster_message_dispatches_messageerror() {
    use crate::browser_child::Out;
    use crate::browser_remote_window::Side;
    use crate::eng::Ops;

    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let make = || {
            let mut browser = Browser::new(bare_html::tree::parse(""), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
            browser.attach_isolated_agent(Default::default()).unwrap();
            browser
        };
        let mut sender = make();
        let mut receiver = make();
        receiver.run_script(r#"
            var received = "";
            addEventListener("message", () => { received = "message"; });
            addEventListener("messageerror", e => { received = e.type + ":" + e.data; });
        "#).unwrap();
        let dom = sender.dom.clone();
        let cloned = sender.it.host_scope_o(|it| crate::browser_scoped::run(it, |scope| {
            let buffer = scope.run_script("new SharedArrayBuffer(4)")?;
            crate::browser_navigation_state::clone_state_with(&dom, scope, buffer, &[])
        })).unwrap();
        let dom = receiver.dom.clone();
        receiver.it.host_scope_o(|it| dom.deliver(it, Out {
            to: Side::Parent, target_origin: "*".into(), origin: "https://example.test".into(), cloned,
        }, None)).unwrap();
        receiver.run_scheduled_tasks(1).unwrap();
        let result = receiver.run_script(r#"received === "messageerror:null""#).unwrap();
        assert_eq!(result.as_bool(), Some(true));
    }).unwrap().join().unwrap();
}

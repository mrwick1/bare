use super::*;
use crate::resources::test_server::{Reply, serve};

#[test]
fn isolation_request_comes_from_the_final_document_response() {
    let server = serve(|path| {
        let redirect = |target| Reply::new(302, None, "").header("Location", target);
        let document = || Reply::new(200, Some("text/html"), "<p>document</p>");
        match path {
            "/to-plain" => redirect("/plain")
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp"),
            "/to-isolated" => redirect("/isolated"),
            "/isolated" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp"),
            "/credentialless" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "credentialless"),
            "/denied" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp")
                .header("Permissions-Policy", "cross-origin-isolated=()"),
            "/report-only" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy-Report-Only", "require-corp"),
            "/duplicate" => document()
                .header("Cross-Origin-Opener-Policy", "same-origin")
                .header("Cross-Origin-Embedder-Policy", "require-corp")
                .header("Cross-Origin-Embedder-Policy", "unsafe-none"),
            _ => document(),
        }
    });
    let cases = [
        ("/to-plain", None),
        ("/to-isolated", Some(EmbedderPolicy::RequireCorp)),
        ("/credentialless", Some(EmbedderPolicy::Credentialless)),
        ("/report-only", None),
        ("/denied", None),
        ("/duplicate", None),
    ];
    for (path, requested) in cases {
        for streaming in [false, true] {
            let url = server.url(path);
            let mut client = Client::new();
            let feed = streaming.then(|| Arc::new(Feed::default()));
            let producer = feed.clone();
            let response = load_document(
                &mut client, &url, None, None, &SessionLimits::default(),
                Instant::now() + std::time::Duration::from_secs(5), None, false, producer.as_ref(), false,
            ).unwrap();
            assert_eq!(response.isolation_requested, requested, "{path}");
            if let Some(feed) = feed {
                let Some(Msg::Head(head)) = feed.pop() else { panic!("expected a streamed response head") };
                assert_eq!(head.isolation_requested, requested, "{path}");
            }
        }
    }
}

#[test]
fn a_navigated_document_enforces_corp_and_shares_memory_with_its_worker() {
    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let third_party = serve(|path| {
            if path == "/module" {
                return Reply::js("globalThis.moduleAllowed = true;")
                    .header("Access-Control-Allow-Origin", "*");
            }
            if path == "/module-blocked" {
                return Reply::js("globalThis.moduleBlocked = false;")
                    .header("Cross-Origin-Resource-Policy", "cross-origin");
            }
            if path == "/redirect" {
                return Reply::new(302, None, "").header("Location", "/blocked")
                    .header("Cross-Origin-Resource-Policy", "cross-origin");
            }
            if path == "/cors" {
                return Reply::new(200, Some("text/plain"), "allowed")
                    .header("Access-Control-Allow-Origin", "*");
            }
            let script = Reply::js(if path == "/allowed" { "var allowed = true" } else { "var blocked = false" });
            if path == "/allowed" {
                script.header("Cross-Origin-Resource-Policy", "cross-origin")
            } else {
                script.header("Cache-Control", "public, max-age=60")
            }
        });
        let blocked = third_party.url("/blocked");
        let allowed = third_party.url("/allowed");
        let redirect = third_party.url("/redirect");
        let cors = third_party.url("/cors");
        let module = third_party.url("/module");
        let blocked_module = third_party.url("/module-blocked");
        let server = serve(move |path| {
            match path {
                "/page" => Reply::new(200, Some("text/html"), format!(r#"
                    <script>
                      var blocked = true, allowed = false, denied = false, corsAllowed = false;
                      var frameAllowed = false, frameBlocked = true;
                      var moduleAllowed = false, moduleBlocked = true;
                      addEventListener("message", e => {{
                        if (e.data === "allowed-frame") frameAllowed = true;
                        if (e.data === "blocked-frame") frameBlocked = false;
                      }});
                    </script>
                    <script src="{blocked}"></script><script src="{redirect}"></script><script src="{allowed}"></script>
                    <iframe src="/allowed-frame"></iframe><iframe src="/blocked-frame"></iframe>
                    <script type="module" src="{module}"></script>
                    <script type="module" src="{blocked_module}"></script>
                    <script>
                      var shared = new SharedArrayBuffer(4);
                      var view = new Int32Array(shared), done = false;
                      var worker = new Worker("/worker");
                      worker.onmessage = e => {{
                        done = e.data === "ok" && view[0] === 42;
                      }};
                      worker.postMessage(shared);
                      var unsafeWorker = new Worker("/worker-without-policy");
                      unsafeWorker.onerror = () => {{ denied = true; }};
                      fetch("{cors}").then(r => r.text()).then(s => {{ corsAllowed = s === "allowed"; }});
                    </script>
                "#)).header("Cross-Origin-Opener-Policy", "same-origin")
                    .header("Cross-Origin-Embedder-Policy", "require-corp"),
                "/allowed-frame" => Reply::new(200, Some("text/html"), r#"<script>parent.postMessage("allowed-frame", "*");</script>"#)
                    .header("Cross-Origin-Embedder-Policy", "require-corp"),
                "/blocked-frame" => Reply::new(200, Some("text/html"), r#"<script>parent.postMessage("blocked-frame", "*");</script>"#),
                "/worker-without-policy" => Reply::js(r#"postMessage("unexpected");"#),
                "/worker" => Reply::js(r#"
                    onmessage = e => {
                        const view = new Int32Array(e.data);
                        Atomics.store(view, 0, 42);
                        postMessage(crossOriginIsolated ? "ok" : "not isolated");
                        close();
                    };
                "#).header("Cross-Origin-Embedder-Policy", "require-corp"),
                _ => Reply::new(200, Some("text/html"), "<p>plain document</p>"),
            }
        });
        for live in [false, true] {
            let mut cfg = SessionConfig::new(&server.url("/page"));
            cfg.live = live;
            let mut primed = Client::new();
            let cached = Url::parse(&third_party.url("/blocked")).unwrap();
            let response = primed.get(&cached).unwrap();
            assert_eq!(response.status, 200);
            cfg.client = Box::new(move || primed.fork());
            cfg.page.quiet = std::time::Duration::ZERO;
            let mut session = Session::new(cfg);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let _wake = bare_net::parallel::on_end(move || { let _ = tx.try_send(()); });
            session.run();
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(AnyBrowser::New(browser)) = session.browser()
                    && browser.run_script("crossOriginIsolated && blocked && allowed && done && denied && corsAllowed && frameAllowed && frameBlocked && moduleAllowed && moduleBlocked").ok().and_then(|v| v.as_bool()) == Some(true)
                {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let state = session.browser().and_then(|browser| match browser {
                        AnyBrowser::New(browser) => {
                            use crate::eng::Ops;
                            browser.run_script("[crossOriginIsolated, typeof blocked, typeof allowed, typeof done, typeof worker, globalThis.blocked, globalThis.allowed, globalThis.done, globalThis.denied, globalThis.corsAllowed, globalThis.frameAllowed, globalThis.frameBlocked, globalThis.moduleAllowed, globalThis.moduleBlocked].join(',')").ok().map(|v| browser.it.display_o(v))
                        }
                    });
                    panic!("isolated document did not finish (live={live}, state={state:?})");
                }
                let _ = rx.recv_timeout(remaining.min(std::time::Duration::from_millis(10)));
                let result = session.resume(&[]);
                assert!(matches!(result.stop, SessionStop::Page(Stop::Settled)), "{:?}", result.stop);
            }
            session.navigate(Some(&server.url("/plain")));
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(AnyBrowser::New(browser)) = session.browser()
                    && browser.document_url().is_some_and(|url| url.ends_with("/plain"))
                {
                    let result = browser.run_script("!crossOriginIsolated && typeof SharedArrayBuffer === 'undefined'").unwrap();
                    assert_eq!(result.as_bool(), Some(true));
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "plain navigation did not finish");
                let _ = rx.recv_timeout(remaining.min(std::time::Duration::from_millis(10)));
                session.resume(&[]);
            }
        }
    }).unwrap().join().unwrap();
}

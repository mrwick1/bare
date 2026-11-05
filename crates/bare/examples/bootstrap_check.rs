//! Run the saved public login document and cached scripts in bare over loopback.
//! Usage: bootstrap_check BUNDLE_DIRECTORY [REQUIRED_SELECTOR=form]. No authentication or Node execution.
#[path = "../src/test_server.rs"]
mod test_server;
use bare::browser_session::{Session, SessionConfig, SessionStop};
use bare::Stop;
use std::{collections::HashMap, io::Read, path::Path, time::Duration};
use test_server::{Reply, serve};

fn main() {
    let dir = std::env::args().nth(1).expect("bundle directory");
    let selector = std::env::args().nth(2).unwrap_or_else(|| "form".into());
    let mut assets = HashMap::new();
    let mut total = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "js") { continue; }
        let mut bytes = Vec::new();
        std::fs::File::open(&path).unwrap().take((8 << 20) + 1).read_to_end(&mut bytes).unwrap();
        total += bytes.len();
        assert!(bytes.len() <= 8 << 20 && total <= 32 << 20 && assets.len() < 128);
        assets.insert(path.file_name().unwrap().to_string_lossy().to_string(), bytes);
    }
    assert!(!assets.is_empty());
    let worker = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) {64 << 20} else {8 << 20}).spawn(move || {
        let srv = serve(move |path| {
            let path = path.split('?').next().unwrap_or("");
            if path == "/login" { return Reply::new(200, Some("text/html"), include_bytes!("../../../tools/fixtures/staging/login.html").to_vec()); }
            if path.ends_with(".js") && let Some(bytes) = Path::new(path).file_name().and_then(|n| n.to_str()).and_then(|n| assets.get(n)) {
                return Reply::js(bytes.clone());
            }
            Reply::new(404, Some("text/plain"), "not cached")
        });
        let mut cfg = SessionConfig::new(&srv.url("/login"));
        cfg.virtual_clock = true;
        cfg.limits.max_time = Duration::from_secs(5);
        cfg.page.quiet = Duration::from_millis(100);
        cfg.page.max_tasks = 5000;
        cfg.page.max_rounds = 5000;
        let mut session = Session::new(cfg);
        let result = session.run();
        println!("stop={:?} documents={} tasks={}", result.stop, result.pages.len(), result.tasks);
        for p in &result.pages { println!("scripts={} throws={} load_errors={} fetches={} bytes={}", p.scripts_run, p.script_throws, p.load_errors, p.fetches, p.bytes); }
        let mut present = false;
        let mut unhandled = 0;
        if let Some(b) = session.browser() {
            present = match b.query(&selector) {
                Ok(node) => node.is_some(),
                Err(_) => { println!("required_selector_check=failed"); false }
            };
            unhandled = b.it.unhandled_rejections().len();
            println!("required_selector_present={present} unhandled_rejections={unhandled}");
            for report in b.take_reports() {
                // Emit only known API names, never arbitrary exception text.
                let missing = ["WeakMap", "WeakSet", "Reflect", "navigator", "console", "style", "ReadableStream", "MutationObserver", "requestAnimationFrame", "requestIdleCallback", "innerWidth", "innerHTML", "getComputedStyle", "focus", "createElementNS"];
                println!("reported_known_api={}", missing.iter().find(|name| report.message.contains(**name)).copied().unwrap_or("unclassified"));
                let kind = report.message.split(':').next().unwrap_or("");
                println!("reported_error={}", if ["Error","TypeError","ReferenceError","SyntaxError","RangeError"].contains(&kind) { kind } else { "other" });

            }
        }
        present && unhandled == 0 && matches!(result.stop, SessionStop::Page(Stop::Settled)) && result.pages.iter().all(|p| p.script_throws == 0 && p.load_errors == 0)
    }).unwrap();
    if !worker.join().unwrap() { std::process::exit(1); }
}

//! The window path of the paint (`Page::paint_at`) must write trace records: `paint/build` for a display list and `paint/draw` for
//! each raster pass. A redraw with a retained list adds a `draw` and no `build`. With tracing off no file is made.

use crate::browser::*;
use crate::page::{Page, Source};
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_observe::{process as p, Ids};

const FIXTURE: &str = "paint_trace_tests::window_paint_fixture";

#[test]
fn window_paint_fixture() {
    if std::env::var_os("BARE_PAINT_TRACE_FIXTURE").is_none() { return; }
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let owner = p::Process::from_env(false).unwrap();
        {
            let _scope = p::enter(Ids { session: Some(9), ..Ids::default() });
            let html = "<body><p>text</p><input value=abc></body>";
            let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
            b.set_viewport(400.0, 300.0).unwrap();
            b.set_document_url("http://localhost/trace.html").unwrap();
            let mut page = Page::new(Source::Browser(Box::new(b.into())), (400, 300));
            page.quiet = true;
            for _ in 0..2 {
                assert!(page.capture_tall(300, false).is_some());
            }
        }
        owner.shutdown();
    }).unwrap().join().unwrap();
}

fn run(mode: &str) -> (std::path::PathBuf, std::process::Output) {
    let path = std::env::temp_dir().join(format!("paint-trace-{}-{mode}.jsonl", std::process::id()));
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", FIXTURE, "--nocapture"])
        .env("BARE_PAINT_TRACE_FIXTURE", "1").env("BARE_OBSERVE", mode)
        .env("BARE_OBSERVE_FILE", &path).output().unwrap();
    (path, out)
}

#[test]
fn window_paint_writes_build_and_draw_records() {
    let (path, out) = run("trace");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let count = |event: &str| records.iter().filter(|r| r["category"] == "paint" && r["event"] == event && r["session_id"] == 9).count();
    assert_eq!(count("build"), 1, "one list for two paints");
    assert_eq!(count("draw"), 2, "one raster pass per paint");
    assert_eq!(records.iter().filter(|r| r["category"] == "paint" && r["event"] == "paint").count(), 0, "the window path has no outer paint span");

    let (path, out) = run("off");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(!path.exists(), "tracing off must write no file");
}

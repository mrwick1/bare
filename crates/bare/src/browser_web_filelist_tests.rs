//! Tests of the `FileList` global (`browser_web.rs`). Page bundles read `FileList` (for example `x instanceof FileList`) and throw a
//! `ReferenceError` when it is absent.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn run(src: &str) -> String {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn({
            let src = src.to_string();
            move || {
                let doc = tree::parse("<!doctype html><html><body><input type=file id=f></body></html>");
                let mut b = Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
                let v = b.run_script(&src).expect("script");
                b.it.display(v)
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn file_list_is_an_interface_with_an_illegal_constructor() {
    assert_eq!(run("typeof FileList"), "function");
    assert_eq!(run("[FileList.name, FileList.prototype.constructor === FileList, Object.prototype.toString.call(FileList.prototype)].join()"), "FileList,true,[object FileList]");
    assert_eq!(run("try { new FileList(); 'made' } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal constructor");
    assert_eq!(run("[{} instanceof FileList, new File([], 'a') instanceof FileList].join()"), "false,false");
    assert_eq!(run("var d = Object.getOwnPropertyDescriptor(globalThis, 'FileList'); [typeof d.get, d.enumerable, d.configurable].join()"), "function,false,true");
}

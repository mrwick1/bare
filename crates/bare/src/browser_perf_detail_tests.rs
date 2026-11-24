//! Tests of the `detail` of a `PerformanceMark` and a `PerformanceMeasure` in the performance timeline (User Timing L3).

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


#[test]
fn timeline_keeps_the_entry_and_its_detail() {
    big(|| {
        let mut b = browser();
        let got = js(&mut b, "var d = {taskId: 7}; var m = performance.mark('t', {detail: d}); d.taskId = 8; var e = performance.getEntriesByName('t', 'mark')[0]; [e === m, e.detail === m.detail, e.detail !== d, e.detail.taskId, performance.getEntries().indexOf(m) >= 0].join()");
        assert_eq!(got, "true,true,true,7,true");
        let got = js(&mut b, "var q = performance.measure('q', {start: 0, detail: {n: [1]}}); var f = performance.getEntriesByType('measure')[0]; [f === q, f.detail.n[0], performance.mark('u').detail, performance.getEntriesByName('u')[0].detail].join()");
        assert_eq!(got, "true,1,,");
        // `clearMarks` drops the kept entry.
        assert_eq!(js(&mut b, "performance.clearMarks('t'); performance.getEntriesByName('t').length"), "0");
    })
}

#[test]
fn mark_detail_that_cannot_be_cloned_throws() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "try { performance.mark('x', {detail: function () {}}); 'no' } catch (e) { e.name + ':' + performance.getEntriesByName('x').length }"), "DataCloneError:0");
    })
}

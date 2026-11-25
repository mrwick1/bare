//! A gate (`Fault::Unsupported`) that a script reaches throws a `NotSupportedError` DOMException. The script can catch it, an uncaught
//! one ends only its task, and the console keeps the name of the feature (`docs/internal/design-unsupported-throws.md`).

use crate::browser::*;
use crate::browser_console::ConsoleLevel;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk() -> Browser {
    Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// The error records of the console, as text.
fn errors(b: &mut Browser) -> Vec<String> {
    b.take_console_records().into_iter().filter(|r| r.level == ConsoleLevel::Error).map(|r| b.format_console_record(&r).unwrap()).collect()
}

// A gate of the old form (`Fault::Unsupported` through `lift_err`) and one of the new form (`unsupp`).
const OLD: &str = "new Headers().append('x', '\\u00e9')";
const NEW: &str = "document.createElementNS(null, 'x')";

#[test]
fn a_caught_gate_is_a_not_supported_error_and_the_script_goes_on() {
    big(|| {
        let mut b = mk();
        for (src, what) in [(OLD, "non-ASCII header value"), (NEW, "createElementNS with a null namespace")] {
            let r = js(&mut b, &format!("var out; try {{ {src}; out = 'no throw' }} catch (e) {{ out = [e instanceof DOMException, e.name, e.code, e.message].join('|') }} out + '|after'"));
            assert!(r.starts_with("true|NotSupportedError|9|"), "{r}");
            assert!(r.contains(what) && r.ends_with("|after"), "{r}");
        }
    });
}

#[test]
fn an_uncaught_gate_in_a_timer_ends_that_task_only() {
    big(|| {
        let mut b = mk();
        b.use_virtual_clock_at(1000.0).unwrap();
        js(&mut b, &format!("window.log = []; setTimeout(function () {{ log.push('a'); {OLD}; log.push('not here') }}, 1); setTimeout(function () {{ log.push('b') }}, 5)"));
        b.advance_time(10.0, 1000).expect("the page goes on");
        assert_eq!(js(&mut b, "log.join()"), "a,b");
        let reports: Vec<String> = b.take_reports().into_iter().map(|r| r.message).collect();
        assert!(reports.iter().any(|m| m.contains("NotSupportedError")), "{reports:?}");
    });
}

#[test]
fn a_gate_stays_visible_in_the_console_even_when_caught() {
    big(|| {
        let mut b = mk();
        js(&mut b, &format!("try {{ {NEW} }} catch (e) {{}}"));
        let e = errors(&mut b);
        assert!(e.iter().any(|t| t.contains("not supported") && t.contains("createElementNS with a null namespace")), "{e:?}");
    });
}

/// Did the script end with a gate (an uncaught `NotSupportedError`)? For the tests of other modules that read a gate through `run_script`.
pub(crate) fn is_gate(b: &mut Browser, r: Result<bare_vex::value::Value, Error>) -> bool {
    matches!(r, Err(Error::Throw(v)) if crate::eng_new::is_object(v) && b.it.get_str(v, "name") == "NotSupportedError")
}

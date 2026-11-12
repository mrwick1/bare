//! Tests of `FileReader` (`shims/filereader.js`): the four `read*` methods, their events, `abort`, and the errors of a read.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let html = "<body></body>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run the jobs and the timers that are due.
fn pump(b: &mut Browser) {
    b.drain().unwrap();
    b.advance_time(500.0, 10_000).unwrap();
    b.drain().unwrap();
}

/// Run a setup line. It must not throw; its value is not checked.
fn setup(b: &mut Browser, src: &str) {
    let r = js(b, src);
    assert!(!["throw", "other", "unsupp"].iter().any(|p| r.starts_with(p)), "{r}");
}

#[test]
fn file_reader_is_a_constructor_with_states() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof FileReader"), "function");
        assert_eq!(js(&mut b, "[FileReader.EMPTY, FileReader.LOADING, FileReader.DONE].join()"), "0,1,2");
        assert_eq!(js(&mut b, "var r = new FileReader(); [r.readyState, r.result, r.error].map(String).join()"), "0,null,null");
    });
}

#[test]
fn read_as_text_fires_events_in_order() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(), log = []; ['loadstart', 'progress', 'load', 'loadend'].forEach(function (t) { r.addEventListener(t, function () { log.push(t) }) }); r.readAsText(new Blob(['héllo'])); log.push('sync:' + r.readyState)");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sync:1,loadstart,progress,load,loadend");
        assert_eq!(js(&mut b, "[r.result === 'héllo', r.readyState].join()"), "true,2");
    });
}

#[test]
fn read_as_text_takes_the_encoding_label() {
    big(|| {
        let mut b = browser();
        // Only UTF-8 is built into `TextDecoder` here. Other labels stop with `Unsupported` in the host (see the notes).
        setup(&mut b, "var r = new FileReader(); r.readAsText(new Blob([new Uint8Array([0xc3, 0xa9])]), 'utf-8')");
        pump(&mut b);
        assert_eq!(js(&mut b, "r.result === 'é'"), "true");
    });
}

#[test]
fn read_as_array_buffer_gives_the_bytes() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(); r.readAsArrayBuffer(new Blob(['abc']))");
        pump(&mut b);
        assert_eq!(js(&mut b, "[r.result instanceof ArrayBuffer, r.result.byteLength].join()"), "true,3");
    });
}

#[test]
fn read_as_data_url_gives_base64_with_the_type() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(); r.readAsDataURL(new Blob(['hi'], { type: 'text/plain' }))");
        pump(&mut b);
        assert_eq!(js(&mut b, "r.result"), "data:text/plain;base64,aGk=");
        assert_eq!(js(&mut b, "r.readAsDataURL(new Blob(['hi'])); 'started'"), "started");
        pump(&mut b);
        assert_eq!(js(&mut b, "r.result"), "data:application/octet-stream;base64,aGk=");
    });
}

#[test]
fn read_as_binary_string_has_one_char_per_byte() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(); r.readAsBinaryString(new Blob(['hé']))");
        pump(&mut b);
        assert_eq!(js(&mut b, "[r.result.length, r.result.charCodeAt(1)].join()"), "3,195");
    });
}

#[test]
fn a_second_read_while_loading_throws_invalid_state() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var r = new FileReader(); r.readAsText(new Blob(['a'])); try { r.readAsText(new Blob(['b'])); 'no throw' } catch (e) { e.name + ':' + (e instanceof DOMException) }"), "InvalidStateError:true");
    });
}

#[test]
fn a_non_blob_argument_throws_type_error() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "try { new FileReader().readAsText('x'); 'no throw' } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn abort_fires_abort_then_loadend_and_clears_the_result() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(), log = []; r.onabort = function () { log.push('abort:' + r.readyState) }; r.onloadend = function () { log.push('loadend') }; r.onload = function () { log.push('load') }; r.readAsText(new Blob(['x'])); r.abort(); log.push('sync:' + r.readyState + ':' + r.result)");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "abort:2,loadend,sync:2:null");
        assert_eq!(js(&mut b, "r.result"), "null");
    });
}

#[test]
fn abort_when_not_loading_does_nothing() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var r = new FileReader(), n = 0; r.onabort = function () { n++ }; r.abort(); r.readAsText(new Blob(['x'])); 'started'"), "started");
        pump(&mut b);
        assert_eq!(js(&mut b, "r.abort(); [r.readyState, r.result, n].join()"), "2,,0");
    });
}

#[test]
fn onload_handler_runs() {
    big(|| {
        let mut b = browser();
        setup(&mut b, "var r = new FileReader(); r.onload = function () { window.got = r.result }; r.readAsText(new Blob(['ok']))");
        pump(&mut b);
        assert_eq!(js(&mut b, "window.got"), "ok");
    });
}

//! Tests of `DataTransfer`, `DataTransferItem`, `DataTransferItemList` and `ClipboardEvent` (`shims/clipboard.js`), from page script.

use crate::browser::*;
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

fn run(src: &'static str) -> String {
    big(move || {
        let mut b = mk();
        let r = js(&mut b, src);
        b.drain().unwrap();
        if r.starts_with("throw") || r.starts_with("other") { r } else { js(&mut b, "typeof out === 'undefined' ? '' : out.join('|')") }
    })
}

#[test]
fn a_new_data_transfer_is_empty() {
    assert_eq!(
        run("var d = new DataTransfer(); out = [d.dropEffect, d.effectAllowed, d.types.length, d.items.length, d.files.length, typeof d.getData('text/plain'), d.getData('text/plain') === ''];"),
        "none|all|0|0|0|string|true"
    );
}

#[test]
fn set_data_get_data_and_types_use_lowercase_names_and_the_text_alias() {
    assert_eq!(
        run("var d = new DataTransfer(); d.setData('Text/Plain', 'a'); d.setData('text/html', '<b>'); d.setData('text', 'b'); out = [d.getData('text'), d.getData('TEXT/PLAIN'), d.getData('text/html'), d.getData('x/y'), d.types.join(',')];"),
        "b|b|<b>||text/plain,text/html"
    );
}

#[test]
fn clear_data_removes_one_type_or_all() {
    assert_eq!(
        run("var d = new DataTransfer(); d.setData('text/plain', 'a'); d.setData('text/html', 'b'); d.clearData('text/html'); var one = d.types.join(','); d.clearData(); out = [one, d.types.length];"),
        "text/plain|0"
    );
}

#[test]
fn items_add_a_string_or_a_file_and_the_list_has_indices() {
    assert_eq!(
        run("var d = new DataTransfer(); d.items.add('hi', 'text/plain'); var f = new File([new Uint8Array([1, 2, 3])], 'image.png', {type: 'image/png'}); d.items.add(f); \
             var a = d.items[0], b = d.items[1]; \
             out = [d.items.length, a.kind, a.type, b.kind, b.type, b.getAsFile() === f, a.getAsFile() === null, d.files.length, d.files[0] === f, d.files.item(0) === f, d.types.join(','), Object.prototype.toString.call(d.items)];"),
        "2|string|text/plain|file|image/png|true|true|1|true|true|text/plain,Files|[object DataTransferItemList]"
    );
}

#[test]
fn items_remove_and_clear_and_iterate() {
    assert_eq!(
        run("var d = new DataTransfer(); d.items.add('a', 'text/plain'); d.items.add('b', 'text/html'); var kinds = []; for (var i of d.items) kinds.push(i.type); d.items.remove(0); var left = d.items.length + ':' + d.items[0].type + ':' + (d.items[1] === undefined); d.items.clear(); out = [kinds.join(','), left, d.items.length];"),
        "text/plain,text/html|1:text/html:true|0"
    );
}

#[test]
fn adding_the_same_string_type_twice_is_a_not_supported_error() {
    assert_eq!(
        run("var d = new DataTransfer(); d.items.add('a', 'text/plain'); try { d.items.add('b', 'text/plain'); out = ['no']; } catch (e) { out = [e.name]; }"),
        "NotSupportedError"
    );
}

#[test]
fn get_as_string_calls_back_later_with_the_text() {
    assert_eq!(
        big(|| {
            let mut b = mk();
            js(&mut b, "var d = new DataTransfer(); d.items.add('hey', 'text/plain'); out = []; var finished = false; d.items[0].getAsString(function (s) { out.push('cb:' + s); finished = true; }); out.push('sync');");
            crate::browser_xhr_tests::settle(&mut b);
            js(&mut b, "out.join('|')")
        }),
        "sync|cb:hey"
    );
}

#[test]
fn a_clipboard_event_is_an_event_with_clipboard_data() {
    assert_eq!(
        run("var d = new DataTransfer(); var e = new ClipboardEvent('paste', {clipboardData: d, bubbles: true}); var n = new ClipboardEvent('copy'); \
             out = [e instanceof Event, e.type, e.clipboardData === d, e.bubbles, e.cancelable, n.clipboardData === null, n.bubbles, e.isTrusted, Object.prototype.toString.call(e)];"),
        "true|paste|true|true|false|true|false|false|[object ClipboardEvent]"
    );
}

#[test]
fn the_globals_are_functions() {
    assert_eq!(run("out = [typeof DataTransfer, typeof DataTransferItem, typeof DataTransferItemList, typeof ClipboardEvent, 'DataTransfer' in window];"), "function|function|function|function|true");
}

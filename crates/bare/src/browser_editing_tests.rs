//! Tests of `isContentEditable`, `contentEditable` and the `document.execCommand` text commands (`browser_editing.rs`).

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body><div id=out></div></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// A page with an editing host `#e`, the caret at the start of it, and a log of the `input` events.
const SETUP: &str = "document.body.innerHTML = '<div contenteditable id=e></div>'; \
    var e = document.getElementById('e'); window.log = []; \
    e.addEventListener('input', function (ev) { log.push(ev.constructor.name + ':' + ev.inputType + ':' + ev.data + ':' + ev.target.id + ':' + ev.bubbles); }); \
    var s = getSelection(); s.collapse(e, 0);";

#[test]
fn is_content_editable_and_reflection() {
    big(|| {
        let mut b = browser();
        let src = "document.body.innerHTML = '<div id=a contenteditable><p id=b>x</p><span id=c contenteditable=false><i id=d>y</i></span></div><div id=f></div><div id=g contenteditable=plaintext-only></div>'; \
            var q = function (i) { return document.getElementById(i); }; \
            [q('a').isContentEditable, q('b').isContentEditable, q('c').isContentEditable, q('d').isContentEditable, q('f').isContentEditable, q('g').isContentEditable].join()";
        assert_eq!(js(&mut b, src), "true,true,false,false,false,true");
        assert_eq!(js(&mut b, "[q('a').contentEditable, q('b').contentEditable, q('c').contentEditable, q('f').contentEditable, q('g').contentEditable].join()"), "true,inherit,false,inherit,plaintext-only");
        assert_eq!(js(&mut b, "q('f').contentEditable = 'true'; [q('f').getAttribute('contenteditable'), q('f').isContentEditable].join()"), "true,true");
        assert_eq!(js(&mut b, "q('f').contentEditable = 'inherit'; [q('f').hasAttribute('contenteditable'), q('f').isContentEditable].join()"), "false,false");
        assert_eq!(js(&mut b, "try { q('f').contentEditable = 'bad'; 'no' } catch (e) { e.name }"), "SyntaxError");
        assert_eq!(js(&mut b, "document.designMode"), "off");
    });
}

#[test]
fn query_command_functions() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[typeof document.execCommand, typeof document.queryCommandState].join()"), "function,function");
        assert_eq!(js(&mut b, "['insertText', 'DELETE', 'forwardDelete', 'insertParagraph', 'insertLineBreak', 'selectAll'].map(function (n) { return document.queryCommandSupported(n); }).join()"), "true,true,true,true,true,true");
        assert_eq!(js(&mut b, "document.queryCommandSupported('nope')"), "false");
        assert_eq!(js(&mut b, "[document.queryCommandState('insertText'), document.queryCommandValue('insertText') === '', document.queryCommandIndeterm('insertText')].join()"), "false,true,false");
        assert_eq!(js(&mut b, "document.execCommand('nope', false, null)"), "false");
        assert_eq!(js(&mut b, "document.queryCommandEnabled('insertText')"), "false");
        assert_eq!(js(&mut b, &format!("{SETUP} document.queryCommandEnabled('insertText')")), "true");
    });
}

#[test]
fn insert_text_merges_and_fires_one_input_each() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{SETUP} document.execCommand('insertText', false, 'Hello')")), "true");
        assert_eq!(js(&mut b, "document.execCommand('insertText', false, ' world')"), "true");
        assert_eq!(js(&mut b, "[e.textContent, e.childNodes.length, s.anchorNode === e.firstChild, s.anchorOffset, s.isCollapsed].join()"), "Hello world,1,true,11,true");
        assert_eq!(js(&mut b, "log.join('|')"), "InputEvent:insertText:Hello:e:true|InputEvent:insertText: world:e:true");
    });
}

#[test]
fn delete_and_forward_delete() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{SETUP} document.execCommand('insertText', false, 'abcd'); s.collapse(e.firstChild, 4); document.execCommand('delete')")), "true");
        assert_eq!(js(&mut b, "[e.textContent, s.anchorOffset].join()"), "abc,3");
        assert_eq!(js(&mut b, "s.collapse(e.firstChild, 1); document.execCommand('forwardDelete'); [e.textContent, s.anchorOffset].join()"), "ac,1");
        assert_eq!(js(&mut b, "s.collapse(e.firstChild, 0); document.execCommand('delete')"), "false");
        assert_eq!(js(&mut b, "s.setPosition(e.firstChild, 0); var r = document.createRange(); r.setStart(e.firstChild, 0); r.setEnd(e.firstChild, 2); s.removeAllRanges(); s.addRange(r); document.execCommand('delete'); [e.textContent === '', s.isCollapsed].join()"), "true,true");
        assert_eq!(js(&mut b, "log.slice(1).join('|')"), "InputEvent:deleteContentBackward:null:e:true|InputEvent:deleteContentForward:null:e:true|InputEvent:deleteContentBackward:null:e:true");
    });
}

#[test]
fn select_all_then_insert_text_replaces_everything() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{SETUP} e.innerHTML = '<p>one</p><p>two</p>'; s.collapse(e.firstChild.firstChild, 1); document.execCommand('selectAll')")), "true");
        assert_eq!(js(&mut b, "s.toString()"), "onetwo");
        assert_eq!(js(&mut b, "document.execCommand('insertText', false, 'X')"), "true");
        assert_eq!(js(&mut b, "[e.textContent, e.childNodes.length, e.innerHTML].join()"), "X,1,X");
    });
}

#[test]
fn insert_paragraph_splits_the_block() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{SETUP} document.execCommand('insertText', false, 'ab'); s.collapse(e.firstChild, 1); document.execCommand('insertParagraph')")), "true");
        assert_eq!(js(&mut b, "e.innerHTML"), "<div>a</div><div>b</div>");
        assert_eq!(js(&mut b, "[s.anchorNode === e.lastChild.firstChild, s.anchorOffset].join()"), "true,0");
        assert_eq!(js(&mut b, "document.execCommand('insertText', false, 'Z'); e.innerHTML"), "<div>a</div><div>Zb</div>");
        assert_eq!(js(&mut b, "log[log.length - 2]"), "InputEvent:insertParagraph:null:e:true");
    });
}

#[test]
fn insert_line_break_adds_br() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{SETUP} document.execCommand('insertText', false, 'ab'); s.collapse(e.firstChild, 1); document.execCommand('insertLineBreak')")), "true");
        assert_eq!(js(&mut b, "e.innerHTML"), "a<br>b");
        assert_eq!(js(&mut b, "document.execCommand('insertText', false, 'X'); e.innerHTML"), "a<br>Xb");
        assert_eq!(js(&mut b, "log[1]"), "InputEvent:insertLineBreak:null:e:true");
    });
}

#[test]
fn outside_an_editing_host_nothing_changes() {
    big(|| {
        let mut b = browser();
        let src = "document.body.innerHTML = '<p id=p>text</p><div contenteditable=false id=n>no</div>'; window.n = 0; \
            document.addEventListener('input', function () { n++; }); getSelection().collapse(document.getElementById('p').firstChild, 2); \
            [document.execCommand('insertText', false, 'X'), document.execCommand('delete'), document.execCommand('insertParagraph'), document.execCommand('insertLineBreak'), document.queryCommandEnabled('insertText'), document.getElementById('p').textContent, n].join()";
        assert_eq!(js(&mut b, src), "false,false,false,false,false,text,0");
        assert_eq!(js(&mut b, "getSelection().removeAllRanges(); document.execCommand('insertText', false, 'X')"), "false");
    });
}

/// A host with `hello world` and `world` selected, and a log of the `input` events.
const FMT: &str = "document.body.innerHTML = '<div contenteditable id=e>hello world</div>'; \
    var e = document.getElementById('e'); window.log = []; \
    e.addEventListener('input', function (ev) { log.push(ev.inputType); }); \
    var s = getSelection(); window.pick = function (a, b, c, d) { var r = document.createRange(); r.setStart(a, b); r.setEnd(c, d); s.removeAllRanges(); s.addRange(r); }; pick(e.firstChild, 6, e.firstChild, 11);";

#[test]
fn bold_italic_underline_toggle() {
    big(|| {
        let mut b = browser();
        for (cmd, tag, kind) in [("bold", "b", "formatBold"), ("italic", "i", "formatItalic"), ("underline", "u", "formatUnderline")] {
            assert_eq!(js(&mut b, &format!("{FMT} [document.queryCommandState('{cmd}'), document.execCommand('{cmd}'), document.queryCommandState('{cmd}'), e.innerHTML].join()")), format!("false,true,true,hello <{tag}>world</{tag}>"));
            assert_eq!(js(&mut b, "s.toString()"), "world");
            assert_eq!(js(&mut b, &format!("[document.execCommand('{cmd}'), document.queryCommandState('{cmd}'), e.innerHTML, s.toString()].join()")), "true,false,hello world,world");
            assert_eq!(js(&mut b, "log.join()"), format!("{kind},{kind}"));
        }
        assert_eq!(js(&mut b, &format!("{FMT} document.execCommand('bold'); document.execCommand('italic'); e.innerHTML")), "hello <b><i>world</i></b>");
        assert_eq!(js(&mut b, &format!("{FMT} e.innerHTML = 'a <strong>bc</strong>'; s.collapse(e.lastChild.firstChild, 1); document.queryCommandState('bold')")), "true");
        // A collapsed selection changes nothing.
        assert_eq!(js(&mut b, &format!("{FMT} s.collapse(e.firstChild, 3); [document.execCommand('bold'), e.innerHTML, log.length].join()")), "true,hello world,0");
        // Outside an editing host there is nothing to do.
        assert_eq!(js(&mut b, "document.body.innerHTML = '<p id=p>text</p>'; getSelection().selectAllChildren(document.getElementById('p')); document.execCommand('bold')"), "false");
    });
}

#[test]
fn lists_on_and_off() {
    big(|| {
        let mut b = browser();
        let src = format!("{FMT} [document.queryCommandState('insertUnorderedList'), document.execCommand('insertUnorderedList'), document.queryCommandState('insertUnorderedList'), e.innerHTML].join()");
        assert_eq!(js(&mut b, &src), "false,true,true,<ul><li>hello world</li></ul>");
        assert_eq!(js(&mut b, "[document.execCommand('insertUnorderedList'), document.queryCommandState('insertUnorderedList'), e.innerHTML].join()"), "true,false,<div>hello world</div>");
        assert_eq!(js(&mut b, "[document.execCommand('insertOrderedList'), document.queryCommandState('insertOrderedList'), document.queryCommandState('insertUnorderedList'), e.innerHTML].join()"), "true,true,false,<ol><li>hello world</li></ol>");
        assert_eq!(js(&mut b, "log.join()"), "insertUnorderedList,insertUnorderedList,insertOrderedList");
        // Two blocks become two items of one list.
        assert_eq!(js(&mut b, &format!("{FMT} e.innerHTML = '<div>a</div><div>b</div><div>c</div>'; pick(e.firstChild.firstChild, 0, e.children[1].firstChild, 1); document.execCommand('insertUnorderedList'); e.innerHTML")), "<ul><li>a</li><li>b</li></ul><div>c</div>");
    });
}

#[test]
fn links() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{FMT} document.execCommand('createLink', false, '')")), "false");
        assert_eq!(js(&mut b, "[document.execCommand('createLink', false, 'http://x.test/'), e.innerHTML, s.toString()].join()"), "true,hello <a href=\"http://x.test/\">world</a>,world");
        assert_eq!(js(&mut b, "[document.execCommand('unlink'), e.innerHTML].join()"), "true,hello world");
        assert_eq!(js(&mut b, "log.join()"), "insertLink,formatRemove");
        assert_eq!(js(&mut b, &format!("{FMT} s.collapse(e.firstChild, 5); [document.execCommand('createLink', false, 'u'), e.innerHTML].join()")), "true,hello<a href=\"u\">u</a> world");
    });
}

#[test]
fn insert_html_and_remove_format() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, &format!("{FMT} [document.execCommand('insertHTML', false, '<b>x</b>y'), e.innerHTML, s.isCollapsed, log.join()].join()")), "true,hello <b>x</b>y,true,insertHTML");
        assert_eq!(js(&mut b, "document.execCommand('insertText', false, 'Z'); e.innerHTML"), "hello <b>x</b>yZ");
        assert_eq!(js(&mut b, &format!("{FMT} e.innerHTML = '<b>a</b> <i>b</i><u>c</u><span>d</span><a href=\"h\">e</a>'; s.selectAllChildren(e); [document.execCommand('removeFormat'), e.innerHTML, log.join()].join()")), "true,a bcd<a href=\"h\">e</a>,formatRemove");
        for c in ["bold", "italic", "underline", "insertUnorderedList", "insertOrderedList", "createLink", "unlink", "insertHTML", "removeFormat"] {
            assert_eq!(js(&mut b, &format!("document.queryCommandSupported('{c}')")), "true", "{c}");
        }
    });
}

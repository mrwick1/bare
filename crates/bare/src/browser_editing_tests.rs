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

/// Key input at a focused editing host: `beforeinput` then `input`, and a canceled `beforeinput` stops the change.
#[test]
fn keys_edit_a_contenteditable() {
    big(|| {
        let mut b = browser();
        js(&mut b, "document.body.innerHTML = '<div contenteditable id=e></div>'; var e = document.getElementById('e'); window.log = []; \
            ['beforeinput', 'input'].forEach(function (t) { e.addEventListener(t, function (ev) { log.push(t + ':' + ev.inputType + ':' + ev.data + ':' + ev.cancelable); }); });");
        let e = b.query("#e").unwrap().unwrap();
        b.focus(e).unwrap();
        for (i, k) in ["H", "i", "Enter", "x", "Backspace"].iter().enumerate() {
            b.drive_key(if i == 0 { Some(e) } else { None }, k).unwrap();
        }
        assert_eq!(js(&mut b, "e.textContent + '|' + e.children.length"), "Hi|2");
        assert_eq!(
            js(&mut b, "log.join(' ')"),
            "beforeinput:insertText:H:true input:insertText:H:false beforeinput:insertText:i:true input:insertText:i:false \
             beforeinput:insertParagraph:null:true input:insertParagraph:null:false beforeinput:insertText:x:true input:insertText:x:false \
             beforeinput:deleteContentBackward:null:true input:deleteContentBackward:null:false"
        );
        // A canceled beforeinput stops the edit.
        js(&mut b, "e.addEventListener('beforeinput', function (ev) { ev.preventDefault(); });");
        b.drive_key(None, "z").unwrap();
        assert_eq!(js(&mut b, "e.textContent"), "Hi");
    });
}

/// `drive_type` accepts an editing host, clears it first, and `\n` is Enter.
#[test]
fn drive_type_edits_a_contenteditable() {
    big(|| {
        let mut b = browser();
        js(&mut b, "document.body.innerHTML = '<div contenteditable id=e></div>';");
        let e = b.query("#e").unwrap().unwrap();
        assert!(b.is_editing_host(e));
        b.drive_type(e, "Hello world").unwrap();
        assert_eq!(js(&mut b, "e.textContent"), "Hello world");
        b.drive_type(e, "Bye").unwrap();
        assert_eq!(js(&mut b, "e.textContent"), "Bye");
        b.drive_type(e, "a\nb").unwrap();
        assert_eq!(js(&mut b, "e.textContent + '|' + e.children.length"), "ab|2");
    });
}

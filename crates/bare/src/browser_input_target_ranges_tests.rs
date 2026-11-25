//! `InputEvent.getTargetRanges()` and `StaticRange`. A `beforeinput` on an editing host gives the range that the edit replaces, as Chrome
//! does. CKEditor 5 (the Teams compose box) builds its insert selection from these ranges: an empty list made it throw
//! "Cannot read properties of null (reading 'parent')", and nothing was typed.
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

#[test]
fn beforeinput_on_an_editing_host_gives_the_selected_range() {
    let mut b = browser("<!doctype html><div id=h contenteditable><p>abcd</p></div><input id=i>");
    js(&mut b, "window.log = []; const h = document.getElementById('h'); \
        for (const t of ['beforeinput', 'input']) h.addEventListener(t, e => { const r = e.getTargetRanges(); \
        log.push(t + ':' + r.length + (r.length ? ':' + [r[0] instanceof StaticRange, r[0].startContainer.nodeName, r[0].startOffset, r[0].endContainer.nodeName, r[0].endOffset, r[0].collapsed].join(',') : '')); }); \
        h.focus(); const t = h.firstChild.firstChild; getSelection().setBaseAndExtent(t, 1, t, 3)");
    b.ui_insert_text("X").unwrap();
    assert_eq!(js(&mut b, "log.join(' ')"), "beforeinput:1:true,#text,1,#text,3,false input:0");
    assert_eq!(js(&mut b, "document.getElementById('h').textContent"), "aXd");
}

#[test]
fn beforeinput_on_a_text_field_gives_no_range() {
    let mut b = browser("<!doctype html><input id=i>");
    js(&mut b, "window.log = []; const i = document.getElementById('i'); i.addEventListener('beforeinput', e => log.push(e.getTargetRanges().length)); i.focus()");
    b.ui_insert_text("X").unwrap();
    assert_eq!(js(&mut b, "log.join(' ')"), "0");
}

#[test]
fn a_static_range_keeps_its_points() {
    let mut b = browser("<!doctype html><p id=p>abc</p>");
    let r = js(&mut b, "const p = document.getElementById('p'); const t = p.firstChild; \
        const r = new StaticRange({ startContainer: t, startOffset: 1, endContainer: p, endOffset: 1 }); t.data = 'z'; \
        [Object.getPrototypeOf(StaticRange.prototype) === AbstractRange.prototype, r.startContainer === t, r.startOffset, r.endContainer === p, r.endOffset, r.collapsed, String(r)].join()");
    assert_eq!(r, "true,true,1,true,1,false,[object StaticRange]");
    let e = js(&mut b, "try { new StaticRange({ startContainer: document.body, startOffset: 0 }); 'no' } catch (e) { e.name }");
    assert_eq!(e, "TypeError");
    let e = js(&mut b, "try { new StaticRange({ startContainer: document.doctype, startOffset: 0, endContainer: document.body, endOffset: 0 }); 'no' } catch (e) { e.name }");
    assert_eq!(e, "InvalidNodeTypeError");
    assert_eq!(js(&mut b, "try { StaticRange() } catch (e) { e.name }"), "TypeError");
}

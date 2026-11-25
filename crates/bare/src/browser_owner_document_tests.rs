//! `ownerDocument` of a node in a document that `DOMParser` or `createHTMLDocument` made is that document, not the page.
//! DOMPurify (`RETURN_DOM`) gives the `body` of its parsed document; CKEditor in Teams takes `.ownerDocument.body` and moves its children.

use crate::eng_new::Peek;
use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_node_of_a_parsed_document_has_that_document_as_its_owner() {
    big(|| {
        let html = "<body><div id=app></div></body>";
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        assert_eq!(js(&mut b, "var p = new DOMParser().parseFromString('<p>x</p>', 'text/html'); [p.body.ownerDocument === p, p.body.firstChild.ownerDocument === p, p.body.ownerDocument !== document].join()"), "true,true,true");
        assert_eq!(js(&mut b, "var h = document.implementation.createHTMLDocument(''); h.body.ownerDocument === h"), "true");
        assert_eq!(js(&mut b, "var f = p.body.ownerDocument.createDocumentFragment(), r = p.body.ownerDocument.body.childNodes; while (r.length > 0) f.appendChild(r[0]); [f.childNodes.length, document.getElementById('app') !== null].join()"), "1,true");
        assert_eq!(js(&mut b, "[document.getElementById('app').ownerDocument === document, document.body.ownerDocument === document].join()"), "true,true");
    })
}

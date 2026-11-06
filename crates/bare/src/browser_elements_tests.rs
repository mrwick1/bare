//! Tests of `browser_elements.rs` (`HTMLAnchorElement`, `HTMLAreaElement`, `HTMLTemplateElement`) and of `new FormData(form)`. The Chrome cases come first
//! (`tools/fixtures/ui/{anchor,template,formdata}-cases.txt`, answers in `chrome-*.txt`). They use absolute URLs, because the base URL of a Chrome
//! iframe is the temporary file of the recorder. The tests below cover the base URL, the lazy globals, the collector and the limits.

use crate::browser::*;
use crate::browser_loader::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;

topic_test!(anchor_cases, "anchor", 20);
topic_test!(template_cases, "template", 23);
topic_test!(formdata_cases, "formdata", 28);
topic_test!(fragment_cases, "fragment", 75);
topic_test!(insert_cases, "insert", 63);

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><html><body>{body}</body></html>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
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

#[test]
fn relative_urls_resolve_against_the_document_url() {
    let mut b = browser("<a id=a href=a></a><a id=b href='../b?c#d'></a><a id=c href=/root></a><a id=d href=//other.test/p></a><a id=e href='?q'></a><a id=f href='#h'></a><a id=g href=''></a><a id=h href='  a  '></a>");
    // no document URL yet: the text stays as written
    assert_eq!(js(&mut b, "document.getElementById('a').href"), "a");
    b.set_document_url("http://example.com/dir/page.html?x#y").unwrap();
    let want = [
        "http://example.com/dir/a",
        "http://example.com/b?c#d",
        "http://example.com/root",
        "http://other.test/p",
        "http://example.com/dir/page.html?q",
        "http://example.com/dir/page.html?x#h",
        "http://example.com/dir/page.html?x",
        "http://example.com/dir/a",
    ];
    for (i, w) in want.iter().enumerate() {
        let id = (b'a' + i as u8) as char;
        assert_eq!(js(&mut b, &format!("document.getElementById('{id}').href")), *w, "{id}");
    }
    assert_eq!(js(&mut b, "var a = document.getElementById('b'); [a.protocol, a.host, a.pathname, a.search, a.hash, a.origin].join(' ')"), "http: example.com /b ?c #d http://example.com");
    // the setters resolve against the same base and write the attribute
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); a.pathname = '/z'; a.getAttribute('href')"), "http://example.com/z");
}

#[test]
fn relative_urls_follow_history_changes() {
    let mut b = browser("<a id=a href=x></a>");
    b.set_document_url("http://example.com/one/").unwrap();
    assert_eq!(js(&mut b, "document.getElementById('a').href"), "http://example.com/one/x");
    assert_eq!(js(&mut b, "history.pushState(null, '', '/two/'); document.getElementById('a').href"), "http://example.com/two/x");
}

#[test]
fn a_base_element_moves_the_base_of_a_loaded_page() {
    big(|| {
        let doc = "<base href='/b/c/'><a id=a href=x></a><a id=b href='/y'></a><area id=c href='../z'><script>window.out = ['a', 'b', 'c'].map(function (i) { return document.getElementById(i).href }).join(' ')</script>";
        let mut b = Browser::new(tree::parse(doc), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: "http://example.test/p/page".into(), client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        let v = b.run_script("out").unwrap();
        assert_eq!(b.it.display(v), "http://example.test/b/c/x http://example.test/y http://example.test/b/z");
    });
}

#[test]
fn opaque_and_bad_urls_keep_their_text() {
    let mut b = browser("<a id=a href='mailto:a@b.c?s=1#t'></a><a id=b href='http://'></a><a id=c href='data:text/plain,x'></a>");
    b.set_document_url("http://example.com/").unwrap();
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); [a.href, a.protocol, a.pathname, a.search, a.hash, a.host, a.origin].join(' ')"), "mailto:a@b.c?s=1#t mailto: a@b.c ?s=1 #t  null");
    assert_eq!(js(&mut b, "var a = document.getElementById('b'); [a.href, a.protocol, a.pathname, a.origin].join(' ')"), "http:// :  ");
    assert_eq!(js(&mut b, "var a = document.getElementById('c'); [a.href, a.protocol, a.pathname].join(' ')"), "data:text/plain,x data: text/plain,x");
}

#[test]
fn the_interfaces_are_lazy_globals() {
    let mut b = browser("<a id=a href='http://e.com/'></a><template id=t><b></b></template>");
    // wrapping the elements and reading their members does not make the globals
    assert_eq!(js(&mut b, "var a = document.getElementById('a'), t = document.getElementById('t'); a.pathname + t.content.childNodes.length"), "/1");
    for n in ["HTMLAnchorElement", "HTMLAreaElement", "HTMLTemplateElement"] {
        assert_eq!(js(&mut b, &format!("var d = Object.getOwnPropertyDescriptor(window, '{n}'); typeof d.get + ',' + d.configurable + ',' + d.enumerable")), "function,true,false", "{n}");
    }
    assert_eq!(js(&mut b, "Object.getPrototypeOf(a) === HTMLAnchorElement.prototype && HTMLAnchorElement.prototype.constructor === HTMLAnchorElement"), "true");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(window, 'HTMLAnchorElement'); 'value' in d && d.writable && !d.enumerable && d.configurable"), "true");
    // a write replaces the accessor, as for every lazy global
    assert_eq!(js(&mut b, "HTMLTemplateElement = 5; HTMLTemplateElement"), "5");
    // the prototype of the wrapper is the one of the interface, also if the global was never read
    let mut c = browser("<template id=t></template>");
    assert_eq!(js(&mut c, "var p = Object.getPrototypeOf(document.getElementById('t')); p === HTMLTemplateElement.prototype && p.constructor === HTMLTemplateElement"), "true");
}

#[test]
fn interface_objects_are_not_constructors() {
    let mut b = browser("");
    for n in ["HTMLAnchorElement", "HTMLAreaElement", "HTMLTemplateElement"] {
        assert!(js(&mut b, &format!("new {n}()")).starts_with("throw:TypeError"), "{n}");
        assert!(js(&mut b, &format!("{n}()")).starts_with("throw:TypeError"), "{n}");
    }
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLAnchorElement.prototype).sort().join()"), "constructor,hash,host,hostname,href,origin,pathname,port,protocol,search,toString");
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLAreaElement.prototype).sort().join()"), "constructor,hash,host,hostname,href,origin,pathname,port,protocol,search,toString");
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLTemplateElement.prototype).sort().join()"), "constructor,content");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(HTMLAnchorElement.prototype, 'origin'); typeof d.get + ',' + typeof d.set"), "function,undefined");
}

#[test]
fn a_foreign_or_other_element_is_not_a_html_anchor_or_template() {
    let mut b = browser("<svg><a id=s></a><template id=st></template></svg><div id=d></div>");
    assert_eq!(js(&mut b, "[document.getElementById('s') instanceof HTMLAnchorElement, document.getElementById('st') instanceof HTMLTemplateElement, document.getElementById('d') instanceof HTMLAnchorElement].join()"), "false,false,false");
    assert_eq!(js(&mut b, "var t = document.createElementNS('http://www.w3.org/2000/svg', 'template'); 'content' in t"), "false");
    assert_eq!(js(&mut b, "var g = Object.getOwnPropertyDescriptor(HTMLTemplateElement.prototype, 'content').get; g.call(document.getElementById('st'))"), "throw:TypeError:Illegal invocation");
}

#[test]
fn template_content_survives_collection_with_its_expandos() {
    let mut b = browser("<template id=t><p id=in>x</p></template>");
    js(&mut b, "var c = document.getElementById('t').content; c.mark = 1; c.firstChild.mark = 2; delete window.c;");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "var c = document.getElementById('t').content; [c.mark, c.firstChild.mark, c.childNodes.length].join()"), "1,2,1");
    // the template wrapper and the content keep each other: drop the template variable, collect, read through the document
    js(&mut b, "var d = document.createElement('template'); d.content.mark = 3; d.content.appendChild(document.createElement('i')); window.keep = d.content; d = null;");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "[keep.mark, keep.childNodes.length, keep.firstChild.tagName].join()"), "3,1,I");
}

#[test]
fn template_content_belongs_to_the_one_document_arena() {
    // Deviation from Chrome: the content has no inert document of its own, so `ownerDocument` is `document`.
    let mut b = browser("<template id=t><p>x</p></template>");
    assert_eq!(js(&mut b, "var c = document.getElementById('t').content; [c.ownerDocument === document, c.firstChild.ownerDocument === document].join()"), "true,true");
}

#[test]
fn a_template_is_still_inert_for_styles_and_scripts() {
    let mut b = browser("<template id=t><script>window.ran = 1</script><style>body { display: none }</style></template><p id=p>shown</p>");
    assert_eq!(js(&mut b, "typeof window.ran + ',' + getComputedStyle(document.body).display"), "undefined,block");
    assert_eq!(js(&mut b, "document.getElementById('t').content.querySelectorAll('script').length + ',' + document.scripts.length"), "1,0");
}

#[test]
fn only_a_fragment_without_a_host_can_be_inserted() {
    let want = "unsupported:inserting a ShadowRoot or DocumentFragment as a node (fragment insertion)";
    let mut b = browser("<template id=t><p>1</p></template><div id=host></div>");
    // a shadow root stays an error, however it is inserted
    for call in ["h.appendChild(r)", "h.insertBefore(r, null)", "h.append(r)", "h.append('x', r)", "h.appendChild(document.createElement('i')); h.replaceChild(r, h.lastChild)", "h.lastChild.replaceWith(r)"] {
        assert_eq!(js(&mut b, &format!("var h = document.getElementById('host'); h.innerHTML = ''; var r = h.shadowRoot || h.attachShadow({{mode: 'open'}}); h.appendChild(document.createElement('i')); {call}")), want, "{call}");
    }
    // a content fragment moves its children, also into a fragment, and a fragment cannot go inside itself or its own child
    assert_eq!(js(&mut b, "var c = document.getElementById('t').content, d = c.cloneNode(true); c.appendChild(d); c.childNodes.length + ',' + d.childNodes.length"), "2,0");
    assert_eq!(js(&mut b, "var c = document.getElementById('t').content; try { c.firstChild.appendChild(c) } catch (e) { e.name }"), "HierarchyRequestError");
    // a fragment into the document node is still refused
    assert_eq!(js(&mut b, "document.appendChild(document.getElementById('t').content.cloneNode(true))"), want);
}

#[test]
fn a_fragment_is_one_kind_of_node_however_it_is_made() {
    let mut b = browser("<template id=t></template>");
    // The prototype tag does not need a template or an anchor: the first fragment sets it, and a later one does not set it again.
    assert_eq!(js(&mut b, "Object.prototype.toString.call(new DocumentFragment()) + Object.prototype.toString.call(document.createDocumentFragment())"), "[object DocumentFragment][object DocumentFragment]");
    assert_eq!(js(&mut b, "var f = document.createDocumentFragment(); [f instanceof DocumentFragment, f.constructor === DocumentFragment, f.ownerDocument === document, f.cloneNode(true) instanceof DocumentFragment].join()"), "true,true,true,true");
    // `createDocumentFragment`, `new DocumentFragment` and `before` do not wrap or make anything until called: the realm stays small
    let mut b = browser("<p id=p></p>");
    assert_eq!(js(&mut b, "typeof document.createDocumentFragment + typeof document.getElementById('p').before"), "functionfunction");
    // a fragment that nothing holds is collected with its children
    let mut b = browser("");
    js(&mut b, "for (var i = 0; i < 2000; i++) { var f = new DocumentFragment(); f.append(document.createElement('p'), 'x'); f = null }");
    b.collect(&[]).unwrap();
    assert!(b.stats().wrappers < 100, "{:?}", b.stats());
    assert_eq!(js(&mut b, "document.createDocumentFragment().childNodes.length"), "0");
}

#[test]
fn a_page_can_use_scripts_of_a_fragment() {
    // Chrome: a script made by a script and put in a fragment runs once when the fragment goes into the document, in order, with all its siblings in place. (Chrome runs it at the insert; the loader of this browser runs it when the page script ends.)
    big(|| {
        let doc = "<div id=host></div><div id=other></div><script>
            window.out = []; var h = document.getElementById('host'), f = new DocumentFragment();
            ['a', 'b', 'c'].forEach(function (n) { var s = document.createElement('script'); s.textContent = 'window.out.push(\"' + n + '\" + document.querySelectorAll(\"#host > *\").length)'; f.appendChild(s) });
            f.append(document.createElement('p'));
            h.appendChild(f);
            var s = document.createElement('script'); s.textContent = 'window.out.push(\"x\")'; var g = document.createDocumentFragment(); g.appendChild(s);
            h.appendChild(g); document.getElementById('other').appendChild(s);
        </script>";
        let mut b = Browser::new(tree::parse(doc), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: "http://example.test/p/page".into(), client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        let v = b.run_script("window.out.join()").unwrap();
        assert_eq!(b.it.display(v), "a4,b4,c4,x");
    });
}

#[test]
fn a_page_can_use_scripts_with_replace_children_and_insert_adjacent() {
    // A script that `replaceChildren` or `insertAdjacentElement` puts in the document runs once. A script that `insertAdjacentHTML` parses never runs.
    big(|| {
        let doc = "<div id=a></div><div id=b></div><div id=c></div><script>
            window.out = []; function sc(n) { var s = document.createElement('script'); s.textContent = 'window.out.push(\"' + n + '\")'; return s }
            document.getElementById('a').replaceChildren(sc('r'), 'x', sc('s'));
            document.getElementById('b').insertAdjacentElement('beforeend', sc('e'));
            document.getElementById('c').insertAdjacentHTML('beforeend', '<script>window.out.push(\"h\")<\\/script>');
        </script>";
        let mut b = Browser::new(tree::parse(doc), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: "http://example.test/p/page".into(), client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        let v = b.run_script("window.out.join()").unwrap();
        assert_eq!(b.it.display(v), "r,s,e");
    });
}

#[test]
fn inserting_a_fragment_is_one_mutation_record() {
    let mut b = browser("<template id=t><p>1</p><b>2</b><i>3</i></template><div id=host></div>");
    js(&mut b, "var recs = []; new MutationObserver(function (r) { recs.push(r.length + ':' + r[0].addedNodes.length) }).observe(document.getElementById('host'), { childList: true });");
    js(&mut b, "document.getElementById('host').appendChild(document.getElementById('t').content.cloneNode(true))");
    b.drain().unwrap();
    assert_eq!(js(&mut b, "recs.join()"), "1:3");
}

#[test]
fn formdata_from_a_form_is_bounded_by_the_entry_cap() {
    let mut b = browser("<form id=f></form>");
    assert_eq!(js(&mut b, "var f = document.getElementById('f'); f.innerHTML = '<input name=a>'.repeat(65537); new FormData(f)"), format!("limit:{:?}", LimitKind::Host("formdata-entries")));
}

#[test]
fn formdata_from_a_form_charges_the_step_budget() {
    let mut b = Browser::new(tree::parse("<!doctype html><body><form id=f></form></body>"), BrowserLimits::default(), EvalLimits { max_steps: 300_000, ..Default::default() }, HeapLimits::default()).unwrap();
    js(&mut b, "var f = document.getElementById('f'); for (var i = 0; i < 200; i++) f.appendChild(document.createElement('input'))");
    let r = js(&mut b, "for (var i = 0; i < 100000; i++) new FormData(f)");
    assert!(r.starts_with("limit:"), "{r}");
}

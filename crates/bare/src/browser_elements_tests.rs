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
topic_test!(nodecmp_cases, "nodecmp", 126);
topic_test!(docnode_cases, "docnode", 179);
topic_test!(tplhtml_cases, "tplhtml", 46);
// `template.innerHTML` as a setter, and `noscript` markup (`tools/fixtures/ui/{tplctx,noscript}-cases.txt`).
topic_test!(tplctx_cases, "tplctx", 37);
topic_test!(noscript_cases, "noscript", 38);
// The "in template" insertion mode: table parts directly in a template, nested templates, and the contexts of `template.innerHTML` (`tplmodes-cases.txt`).
topic_test!(tplmodes_cases, "tplmodes", 112);

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
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLAnchorElement.prototype).sort().join()"), "constructor,hash,host,hostname,href,origin,pathname,port,protocol,relList,search,toString");
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLAreaElement.prototype).sort().join()"), "constructor,hash,host,hostname,href,origin,pathname,port,protocol,relList,search,toString");
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
fn template_content_belongs_to_the_inert_document() {
    // As in Chrome: the content is owned by a document of its own. The nodes stay in the one arena.
    let mut b = browser("<template id=t><p>x</p></template>");
    assert_eq!(js(&mut b, "var c = document.getElementById('t').content; [c.ownerDocument === document, c.firstChild.ownerDocument === c.ownerDocument, c.ownerDocument.defaultView].join()"), "false,true,");
}

#[test]
fn a_template_is_still_inert_for_styles_and_scripts() {
    let mut b = browser("<template id=t><script>window.ran = 1</script><style>body { display: none }</style></template><p id=p>shown</p>");
    assert_eq!(js(&mut b, "typeof window.ran + ',' + getComputedStyle(document.body).display"), "undefined,block");
    assert_eq!(js(&mut b, "document.getElementById('t').content.querySelectorAll('script').length + ',' + document.scripts.length"), "1,0");
}

#[test]
fn document_forms_lists_the_forms_in_tree_order() {
    let mut b = browser("<form id=a></form><div><form id=b></form></div><template><form></form></template>");
    assert_eq!(js(&mut b, "document.forms.length + ',' + document.forms[1].id + ',' + (document.forms[0] === document.getElementById('a'))"), "2,b,true");
}

#[test]
fn document_images_links_anchors_and_embeds_list_their_elements_in_tree_order() {
    let mut b = browser("<img id=i1><a id=l1 href=/x>x</a><a id=n1 name=top>n</a><div><area id=l2 href=/y><img id=i2></div><a id=none>no href</a><embed id=e1><template><img><a href=/z></a></template>");
    let mut list = |k: &str| js(&mut b, &format!("Array.prototype.map.call(document.{k}, function (e) {{ return e.id }}).join()"));
    assert_eq!(list("images"), "i1,i2");
    assert_eq!(list("links"), "l1,l2");
    assert_eq!(list("anchors"), "n1");
    assert_eq!(list("embeds"), "e1");
    drop(list);
    assert_eq!(js(&mut b, "document.images.length + ',' + (document.images[0] === document.getElementById('i1'))"), "2,true");
}

#[test]
fn a_step_limit_never_leaves_a_clear_of_the_children_half_done() {
    // `textContent = ` and `replaceChildren` charge their work before they change the tree: the tree is the old one or the new one.
    const BUILD: &str = "var d = document.getElementById('d'); for (var i = 0; i < 3000; i++) d.appendChild(document.createElement('i'));";
    let mut b = browser("<div id=d></div>");
    js(&mut b, BUILD);
    let cost = b.it.stats().steps;
    for stmt in ["d.textContent = 'x'", "d.replaceChildren(document.createElement('b'))", "d.replaceChildren()"] {
        // enough for the build, short of the 3000 steps that the clear needs
        let html = "<!doctype html><html><body><div id=d></div></body></html>";
        let limits = EvalLimits { max_steps: cost + 1_500, ..Default::default() };
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), limits, HeapLimits::default()).expect("browser");
        assert_eq!(js(&mut b, &format!("{BUILD} {stmt}")), "limit:Steps", "{stmt}");
        assert_eq!(js(&mut b, "d.childNodes.length + ',' + d.firstChild.nodeName"), "3000,I", "{stmt}");
    }
}

#[test]
fn document_title_reads_and_sets_the_title_element() {
    let mut b = Browser::new(tree::parse("<!doctype html><html><head><title>  Hello \n  <b>x</b> world </title><title>second</title></head><body></body></html>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    // The first title element; its text with ASCII white space stripped and collapsed. A <title> holds text only, so `<b>` is text.
    assert_eq!(js(&mut b, "document.title"), "Hello <b>x</b> world");
    assert_eq!(js(&mut b, "document.title = 'A  title'; document.title + '|' + document.querySelector('title').textContent + '|' + document.querySelectorAll('title').length"), "A title|A  title|2");
    assert_eq!(js(&mut b, "document.title = 7; document.title"), "7");
    // No title element: the setter makes one in the head, the getter gives "".
    let mut b = browser("<p>x</p>");
    assert_eq!(js(&mut b, "document.title"), "");
    assert_eq!(js(&mut b, "document.title = 'made'; document.head.firstChild.nodeName + ':' + document.title"), "TITLE:made");
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
    // a fragment goes into the document node under the Document rules (the page has a root element, so one more element is refused); a shadow root does not
    assert_eq!(js(&mut b, "document.appendChild(document.getElementById('t').content.cloneNode(true))"), "throw:HierarchyRequestError:Failed to execute 'appendChild' on 'Node': Only one element on document allowed.");
    assert_eq!(js(&mut b, "document.appendChild(document.getElementById('host').shadowRoot)"), want);
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

#[test]
fn canvas_context_surface() {
    let mut b = browser("");
    assert_eq!(js(&mut b, "var c=document.createElement('canvas'); var x=c.getContext('2d'); x.fillRect(0,0,2,2); typeof x.measureText('hello').width"), "number");
    assert_eq!(js(&mut b, "c.getContext('2d') === x"), "true");
    assert_eq!(js(&mut b, "c.getContext('webgl') === null"), "true");
    assert_eq!(js(&mut b, "c.toDataURL().slice(0,22)"), "data:image/png;base64,");
}

#[test]
fn svg_geometry_surface() {
    let mut b = browser("<svg id='s' width='20' height='30' viewBox='0 0 20 30'><rect id='r' x='2' y='3' width='10' height='15'/></svg>");
    assert_eq!(js(&mut b, "var s=document.getElementById('s'); s.width.baseVal.value + ':' + s.viewBox.baseVal.height"), "20:30");
    assert_eq!(js(&mut b, "var r=document.getElementById('r'); var box=r.getBBox(); box.x+':'+box.y+':'+box.width+':'+box.height"), "2:3:10:15");
    assert_eq!(js(&mut b, "r.getCTM().a"), "1");
}

#[test]
fn document_write_surface() {
    let mut b = browser("<p>old</p>");
    assert_eq!(js(&mut b, "document.open(); document.write('<p id=written>hello</p>'); document.close(); document.getElementById('written').textContent"), "hello");
}

#[test]
fn svg_path_metrics_surface() {
    let mut b = browser("<svg><path id='p' d='M 0 0 L 30 40'/></svg>");
    assert_eq!(js(&mut b,"var p=document.getElementById('p'); p.getTotalLength()"),"50");
    assert_eq!(js(&mut b,"var q=p.getPointAtLength(25); q.x+':'+q.y"),"15:20");
}

#[test]
fn document_write_parser_surface() {
    let mut b = browser("<p>old</p>");
    assert_eq!(js(&mut b,"document.write('<p id=a>a</p>'); document.writeln('<p id=b>b</p>'); document.close(); document.body.textContent"),"ab\n");
}

#[test]
fn svg_animated_string_surface() {
    let mut b=browser("<svg id=s class=logo></svg>");
    assert_eq!(js(&mut b,"var s=document.getElementById('s'); (s.className instanceof SVGAnimatedString) + ':' + s.className.baseVal"),"true:logo");
}

#[test]
fn indirect_eval_scope_surface() {
    let mut b=browser("");
    assert_eq!(js(&mut b,"var global=5; function f(){var global=9;return (0,eval)('global')} f()"),"5");
    assert_eq!(js(&mut b,"window.eval('var made=7; let hidden=8; made'); made + ':' + typeof hidden"),"7:undefined");
    assert_eq!(js(&mut b,"window.eval('\"use strict\"; var privateValue=1'); typeof privateValue"),"undefined");
    assert_eq!(js(&mut b,"window.eval(42)"),"42");
    assert_eq!(js(&mut b,"window.eval('this') === window"),"true");
    assert_eq!(js(&mut b,"window.eval('let retained=3; (function(){return retained})')()"),"3");
    assert_eq!(js(&mut b,"function scopeRestore(){let local=4;try{window.eval('throw new Error()')}catch(e){}return local} scopeRestore()"),"4");
    assert_eq!(js(&mut b,"function dynamicScope(){let global=9;return new Function('return global')()} dynamicScope()"),"5");
}

#[test]
fn svg_interface_surface() {
    let mut b=browser("");
    assert_eq!(js(&mut b,"var p=document.createElementNS('http://www.w3.org/2000/svg','path'); (p instanceof SVGPathElement) + ':' + (p instanceof SVGGeometryElement) + ':' + (p instanceof SVGGraphicsElement)"),"true:true:true");
}

#[test]
fn indirect_eval_keeps_the_call_budget_surface() {
    let mut b=browser("");
    assert_eq!(js(&mut b,"function recur(){return window.eval('recur()')} recur()"),"limit:CallDepth");
}

#[test]
fn observer_target_element_brand_surface() {
    let mut b=browser("<div id=t></div>");
    assert_eq!(js(&mut b,"var el=document.getElementById('t'); Object.prototype.toString.call(el)"),"[object HTMLElement]");
    assert_eq!(js(&mut b,"var ref=Object.prototype.toString.call(el)==='[object Object]'?new Proxy(el,{}):el; new IntersectionObserver(function(){}).observe(ref); 'ok'"),"ok");
}

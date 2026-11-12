//! Tests of the web API gaps that stopped real sites in `bare-drive`: `performance` entries, `document.createEvent`, and the
//! `HTML*Element` interfaces of the tags that bare parses. Each test runs a real script in a real realm.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let html = "<body><p id=p>x</p><span id=s></span><table id=t><tr><td id=c>1</td></tr></table><ul><li id=l></li></ul><h2 id=h></h2><div id=dv></div></body>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
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

#[test]
fn performance_marks_and_measures_form_a_timeline() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var a = performance.mark('a')");
        assert_eq!(js(&mut b, "[a.name, a.entryType, a.startTime === performance.now(), a.duration].join()"), "a,mark,true,0");
        b.advance_time(50.0, 1_000_000).unwrap();
        js(&mut b, "performance.mark('b')");
        assert_eq!(js(&mut b, "var m = performance.measure('ab', 'a', 'b'); [m.name, m.entryType, m.duration, m.startTime === a.startTime].join()"), "ab,measure,50,true");
        assert_eq!(js(&mut b, "performance.getEntries().map(function (e) { return e.name }).join()"), "a,ab,b");
        assert_eq!(js(&mut b, "performance.getEntriesByType('mark').length + ',' + performance.getEntriesByType('measure').length"), "2,1");
        assert_eq!(js(&mut b, "performance.getEntriesByName('a').length + ',' + performance.getEntriesByName('a', 'measure').length"), "1,0");
        assert_eq!(js(&mut b, "performance.measure('o', { start: 10, end: 30 }).duration"), "20");
        assert_eq!(js(&mut b, "performance.measure('now').duration === performance.now()"), "true");
        assert_eq!(js(&mut b, "performance.measure('x', 'nope')"), "throw:SyntaxError:Failed to execute 'measure' on 'Performance': The mark 'nope' does not exist.");
        assert_eq!(js(&mut b, "performance.clearMarks('a'); performance.getEntriesByType('mark').map(function (e) { return e.name }).join()"), "b");
        assert_eq!(js(&mut b, "performance.clearMarks(); performance.clearMeasures(); performance.getEntries().length"), "0");
        assert_eq!(js(&mut b, "performance.getEntriesByType('resource').length + ',' + performance.getEntriesByType('navigation').length + ',' + Array.isArray(performance.getEntriesByType('paint'))"), "0,0,true");
    });
}

#[test]
fn create_event_makes_an_uninitialized_event_that_init_sets() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var e = document.createEvent('Event'); e.initEvent('foo', true, true); [e instanceof Event, e.type, e.bubbles, e.cancelable].join()"), "true,foo,true,true");
        assert_eq!(js(&mut b, "var n = 0; document.getElementById('p').addEventListener('foo', function (x) { n++; x.preventDefault() }); var r = document.getElementById('p').dispatchEvent(e); n + ',' + r"), "1,false");
        assert_eq!(js(&mut b, "var h = document.createEvent('HTMLEvents'); h.initEvent('bar', false, false); h.type + ',' + (h instanceof Event)"), "bar,true");
        assert_eq!(js(&mut b, "var c = document.createEvent('CustomEvent'); c.initCustomEvent('baz', true, false, { a: 1 }); [c instanceof CustomEvent, c.type, c.bubbles, c.detail.a].join()"), "true,baz,true,1");
        assert_eq!(js(&mut b, "var m = document.createEvent('MouseEvent'); m.initEvent('click', true, true); [m instanceof MouseEvent, m.type].join()"), "true,click");
        assert_eq!(js(&mut b, "var k = document.createEvent('KeyboardEvent'); [k instanceof KeyboardEvent, k.type === ''].join()"), "true,true");
        assert_eq!(js(&mut b, "document.createEvent('uievent') instanceof Event"), "true");
        assert_eq!(js(&mut b, "document.createEvent('Nope')"), "throw:NotSupportedError:Failed to execute 'createEvent' on 'Document': The provided event type ('Nope') is invalid.");
        assert_eq!(js(&mut b, "document.createEvent()"), "throw:TypeError:Failed to execute 'createEvent' on 'Document': 1 argument required, but only 0 present.");
    });
}

#[test]
fn html_element_constructors_exist_for_the_tags_bare_makes() {
    big(|| {
        let mut b = browser();
        for (id, ctor) in [("s", "HTMLSpanElement"), ("p", "HTMLParagraphElement"), ("t", "HTMLTableElement"), ("c", "HTMLTableCellElement"), ("l", "HTMLLIElement"), ("h", "HTMLHeadingElement"), ("dv", "HTMLDivElement")] {
            let r = js(&mut b, &format!("var el = document.getElementById('{id}'); [el instanceof {ctor}, el instanceof HTMLElement, el.constructor === {ctor}, Object.prototype.toString.call(el)].join()"));
            assert_eq!(r, format!("true,true,true,[object {ctor}]"), "{id}");
        }
        assert_eq!(js(&mut b, "var u = document.createElement('ul'); [u instanceof HTMLUListElement, u instanceof HTMLOListElement, document.createElement('ol') instanceof HTMLOListElement].join()"), "true,false,true");
        assert_eq!(js(&mut b, "[typeof HTMLDivElement, HTMLDivElement.name, HTMLSpanElement.prototype instanceof HTMLElement, 'HTMLTableRowElement' in window].join()"), "function,HTMLDivElement,true,true");
        assert_eq!(js(&mut b, "document.createElement('section').constructor === HTMLElement"), "true");
        assert_eq!(js(&mut b, "document.createElement('h5') instanceof HTMLHeadingElement"), "true");
        assert_eq!(js(&mut b, "new HTMLSpanElement()"), "throw:TypeError:Illegal constructor");
        assert_eq!(js(&mut b, "document.createElement('span').constructor === document.getElementById('s').constructor"), "true");
    });
}

/// `<meta>` reflects `name`, `content`, `httpEquiv` and `media`, also before a page names `HTMLMetaElement`. Cause: GitHub reads
/// `meta[name=visitor-payload].content` after a soft navigation and passed `undefined` to `atob`, so the issue page did not render.
#[test]
fn meta_reflects_its_attributes() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var m = document.createElement('meta'); m.setAttribute('name', 'v'); m.setAttribute('content', 'eyJhIjoxfQ=='); m.setAttribute('http-equiv', 'refresh'); [m.name, m.content, m.httpEquiv, m.media === '', JSON.parse(atob(m.content)).a].join()");
        assert_eq!(r, "v,eyJhIjoxfQ==,refresh,true,1");
        let r = js(&mut b, "m.content = 'x'; m.name = 'n'; m.httpEquiv = 'h'; m.media = 'print'; [m.getAttribute('content'), m.getAttribute('name'), m.getAttribute('http-equiv'), m.getAttribute('media')].join()");
        assert_eq!(r, "x,n,h,print");
        assert_eq!(js(&mut b, "[m instanceof HTMLMetaElement, m.constructor === HTMLMetaElement, 'content' in document.createElement('div')].join()"), "true,true,false");
    });
}

/// Each expected string is what Node 22 prints for the same expression.
#[test]
fn url_class_reads_schemes_other_than_http_and_https() {
    big(|| {
        let mut b = browser();
        for (expr, want) in [
            ("var u = new URL('/a/../b?x=1#h', 'sveltekit-internal://'); [u.href, u.pathname, u.search, u.hash, u.protocol, u.host, u.hostname, u.origin].join('|')", "sveltekit-internal:///b?x=1#h|/b|?x=1|#h|sveltekit-internal:|||null"),
            ("var u = new URL('mailto:a@b.c?s=1'); [u.href, u.pathname, u.search, u.protocol, u.host, u.origin].join('|')", "mailto:a@b.c?s=1|a@b.c|?s=1|mailto:||null"),
            ("var u = new URL('data:text/plain,hi%20there#f'); [u.href, u.pathname, u.hash, u.origin].join('|')", "data:text/plain,hi%20there#f|text/plain,hi%20there|#f|null"),
            ("new URL('about:blank').href", "about:blank"),
            ("var u = new URL('blob:https://a.com/uuid'); [u.href, u.pathname, u.origin].join('|')", "blob:https://a.com/uuid|https://a.com/uuid|https://a.com"),
            ("var u = new URL('foo://h:8080/p/q?z'); [u.href, u.port, u.host, u.pathname, u.search].join('|')", "foo://h:8080/p/q?z|8080|h:8080|/p/q|?z"),
            ("var u = new URL('wss://Example.com:443/x'); [u.href, u.origin, u.port].join('|')", "wss://example.com/x|wss://example.com|"),
            ("new URL('rel', 'foo://h/a/b').href", "foo://h/a/rel"),
            ("new URL('../rel?q#z', 'foo://h/a/b/c').href", "foo://h/a/rel?q#z"),
            ("new URL('#x', 'mailto:a@b').href", "mailto:a@b#x"),
            ("new URL('//other/p', 'foo://h/a').href", "foo://other/p"),
            ("new URL('sveltekit-internal://').href", "sveltekit-internal://"),
            ("new URL('javascript:void(0)').href", "javascript:void(0)"),
            ("new URL('foo:/bar').href", "foo:/bar"),
            ("new URL('foo://h?q').href", "foo://h?q"),
            ("URL.canParse('mailto:x') + ',' + URL.canParse('x', 'mailto:a@b')", "true,false"),
            ("var u = new URL('/p', 'foo://h'); u.search = 'a=1'; u.hash = 'z'; u.pathname = '/q/../r'; u.href", "foo://h/r?a=1#z"),
            ("new URL('x', 'mailto:a@b')", "throw:TypeError:Failed to construct 'URL': Invalid URL"),
        ] {
            assert_eq!(js(&mut b, expr), want, "{expr}");
        }
    });
}

#[test]
fn fetch_reads_data_urls_and_refuses_other_schemes_for_one_request() {
    use crate::browser_fetch::FetchLimits;
    use crate::browser_fetch_tests::{go, mk};
    big(|| {
        let mut b = mk(Some("https://example.test/".into()), FetchLimits::default());
        assert_eq!(
            go(&mut b, "var out = []; fetch('data:text/plain;base64,aGk=').then(function (r) { out.push(r.status, r.ok, r.headers.get('content-type'), r.url); return r.text() }).then(function (t) { out.push(t) });"),
            "200|true|text/plain|data:text/plain;base64,aGk=|hi"
        );
        assert_eq!(go(&mut b, "out = []; fetch('data:,a%20b#frag').then(function (r) { out.push(r.headers.get('content-type')); return r.text() }).then(function (t) { out.push(t) });"), "text/plain;charset=US-ASCII|a b");
        assert_eq!(go(&mut b, "out = []; fetch('data:application/json,{\"x\":1}').then(function (r) { return r.json() }).then(function (j) { out.push(j.x) });"), "1");
        assert_eq!(go(&mut b, "out = []; fetch(new URL('data:text/plain,url')).then(function (r) { return r.text() }).then(function (t) { out.push(t) });"), "url");
        // Other schemes: the one request fails with a TypeError; the script goes on.
        assert_eq!(
            go(&mut b, "out = []; ['ftp://h/x', 'mailto:a@b', 'blob:https://example.test/u', 'data:nocomma'].forEach(function (u) { fetch(u).catch(function (e) { out.push(e.name + ':' + e.message) }) }); out.push('after');"),
            "after|TypeError:Failed to fetch|TypeError:Failed to fetch|TypeError:Failed to fetch|TypeError:Failed to fetch"
        );
    });
}

#[test]
fn performance_measure_validates_options() {
    big(|| {
        let mut b = browser();
        for options in ["{start: 1, end: 2}, 'end'", "{duration: 1}", "{detail: null}", "{start: 1, end: 2, duration: 1}"] {
            assert_eq!(js(&mut b, &format!("try {{ performance.measure('x', {options}); false }} catch (e) {{ e instanceof TypeError }}")), "true", "{options}");
        }
        js(&mut b, "performance.mark('end', {startTime: 20})");
        assert_eq!(js(&mut b, "performance.measure('x', {}, 'end').duration"), "20");
    });
}

#[test]
fn legacy_performance_objects_are_stable() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[typeof performance.timing, typeof performance.navigation].join()"), "object,object");
        assert_eq!(js(&mut b, "var t = performance.timing; t === performance.timing && t.navigationStart > 0 && t.loadEventEnd >= t.navigationStart"), "true");
        assert_eq!(js(&mut b, "[performance.navigation.type, performance.navigation.redirectCount, performance.navigation.TYPE_NAVIGATE, Object.prototype.toString.call(t)].join()"), "0,0,0,[object PerformanceTiming]");
    });
}

#[test]
fn document_type_has_a_distinct_node_brand() {
    big(|| {
        let mut b = Browser::new(tree::parse("<!doctype html><body>x</body>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "var dt = document.firstChild; [dt instanceof DocumentType, dt instanceof Node, document.body instanceof DocumentType, dt.name, dt.publicId, dt.systemId].join()"), "true,true,false,html,,");
        assert_eq!(js(&mut b, "try { new DocumentType() } catch(e) { e.name }"), "TypeError");
    });
}

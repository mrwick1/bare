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

/// A realm made from `html`, for the element member tests below.
fn page(html: &str) -> Browser {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

/// `table.rows` and the other table members, before the page names any `HTML*Element` global. Cause: a page read `table.rows.length` and
/// got a `TypeError` on `undefined`.
#[test]
fn table_members_read_and_change_rows_and_cells() {
    big(|| {
        let mut b = page("<table id=t><caption>c</caption><tfoot><tr id=f><td>f</td></tr></tfoot><tbody><tr id=r1><td id=a>1</td><th id=h colspan=3 rowspan=0>2</th></tr><tr id=r2><td>3</td></tr></tbody><thead><tr id=hd><th>h</th></tr></thead></table><table id=e></table>");
        assert_eq!(js(&mut b, "var t = document.getElementById('t'); [t.rows.length, t.rows[0].id, t.rows[1].id, t.rows[3].id, t.rows.item(2).id, t.rows === t.rows].join()"), "4,hd,r1,f,r2,true");
        assert_eq!(js(&mut b, "[t.tBodies.length, t.tHead.localName, t.tFoot.id || t.tFoot.localName, t.caption.textContent, t.rows instanceof HTMLCollection].join()"), "1,thead,tfoot,c,true");
        assert_eq!(js(&mut b, "var r = document.getElementById('r1'); [r.cells.length, r.rowIndex, r.sectionRowIndex, document.getElementById('r2').sectionRowIndex, document.getElementById('f').rowIndex].join()"), "2,1,0,1,3");
        assert_eq!(js(&mut b, "var h = document.getElementById('h'); [h.cellIndex, h.colSpan, h.rowSpan, document.getElementById('a').colSpan, document.getElementById('a').rowSpan, document.createElement('td').cellIndex].join()"), "1,3,0,1,1,-1");
        assert_eq!(js(&mut b, "h.colSpan = 0; h.rowSpan = 70000; [h.getAttribute('colspan'), h.colSpan, h.rowSpan, h.headers, h.scope].join()"), "0,1,65534,,");
        // The collection is live.
        assert_eq!(js(&mut b, "var rows = t.rows; var n = t.insertRow(1); n.id = 'n'; [rows.length, rows[1].id, n.parentNode.localName, n.rowIndex].join()"), "5,n,tbody,1");
        assert_eq!(js(&mut b, "t.insertRow(-1).id = 'z'; [rows[5].id, rows[5].parentNode.localName].join()"), "z,tfoot");
        assert_eq!(js(&mut b, "t.insertRow(99)"), "throw:IndexSizeError:Failed to execute 'insertRow' on 'HTMLTableElement': The index provided (99) is outside the range [-1, 6].");
        assert_eq!(js(&mut b, "t.deleteRow(1); t.deleteRow(-1); [rows.length, rows[1].id, document.getElementById('z')].join()"), "4,r1,");
        assert_eq!(js(&mut b, "t.deleteRow(9)"), "throw:IndexSizeError:Failed to execute 'deleteRow' on 'HTMLTableElement': The index provided (9) is outside the range [-1, 4).");
        // Cells.
        assert_eq!(js(&mut b, "var c = r.insertCell(1); c.textContent = 'x'; [r.cells.length, r.cells[1].textContent, c.localName, r.insertCell().cellIndex].join()"), "3,x,td,3");
        assert_eq!(js(&mut b, "r.deleteCell(0); r.deleteCell(-1); [r.cells.length, r.cells[0].textContent].join()"), "2,x");
        // Sections.
        assert_eq!(js(&mut b, "var s = t.tBodies[0]; var sr = s.insertRow(0); sr.id = 's0'; [s.rows.length, s.rows[0].id, t.rows[1].id].join()"), "3,s0,s0");
        assert_eq!(js(&mut b, "s.deleteRow(0); s.rows.length"), "2");
        // An empty table: insertRow makes a tbody; the create and delete methods.
        assert_eq!(js(&mut b, "var e = document.getElementById('e'); e.insertRow().id = 'q'; [e.tBodies.length, e.rows.length, e.firstChild.localName].join()"), "1,1,tbody");
        assert_eq!(js(&mut b, "var th = e.createTHead(); [th === e.createTHead(), e.firstChild === th, e.createTFoot() === e.lastChild, e.createCaption() === e.firstChild, e.createTBody() === e.tBodies[1]].join()"), "true,true,true,true,true");
        assert_eq!(js(&mut b, "e.deleteTHead(); e.deleteTFoot(); e.deleteCaption(); [e.tHead, e.tFoot, e.caption, e.children.length].join()"), ",,,2");
        assert_eq!(js(&mut b, "HTMLTableElement.prototype.insertRow.call(document.body)"), "throw:TypeError:Illegal invocation");
    });
}

/// Attribute reflection of the tags without a module of their own, before any `HTML*Element` global is read.
#[test]
fn plain_tags_reflect_their_attributes() {
    big(|| {
        let mut b = page("<ol id=o start=3 reversed type=a><li id=l value=7>x</li></ol><details id=dt open name=g></details><time id=tm datetime=2026-01-02></time><data id=da value=v></data><picture><source id=so src=/a.webp type=image/webp srcset=a.webp media=(min-width:1px) sizes=10px></picture>");
        assert_eq!(js(&mut b, "var o = document.getElementById('o'); [o.start, o.reversed, o.type, document.createElement('ol').start].join()"), "3,true,a,1");
        // `li` gets its prototype at the switch only (`has_members`).
        assert_eq!(js(&mut b, "var l = document.getElementById('l'); [l.value, typeof HTMLLIElement, l.value, document.createElement('li').value].join()"), ",function,7,0");
        assert_eq!(js(&mut b, "o.start = 5; o.reversed = false; [o.getAttribute('start'), o.hasAttribute('reversed')].join()"), "5,false");
        assert_eq!(js(&mut b, "var dt = document.getElementById('dt'); [dt.open, dt.name, document.getElementById('tm').dateTime, document.getElementById('da').value].join()"), "true,g,2026-01-02,v");
        assert_eq!(js(&mut b, "dt.open = false; dt.hasAttribute('open')"), "false");
        assert_eq!(js(&mut b, "var so = document.getElementById('so'); [so.type, so.srcset, so.media, so.sizes, so.src.endsWith('/a.webp')].join()"), "image/webp,a.webp,(min-width:1px),10px,true");
        assert_eq!(js(&mut b, "[document.documentElement.version, document.createElement('base').target].join('|')"), "|");
        assert_eq!(js(&mut b, "'start' in document.createElement('div')"), "false");
    });
}

/// `progress` and `meter` clamp their numbers as the standard says.
#[test]
fn progress_and_meter_clamp_their_values() {
    big(|| {
        let mut b = page("<progress id=p value=3 max=4></progress><progress id=q></progress><meter id=m value=9 min=2 max=5 low=1 high=4></meter>");
        assert_eq!(js(&mut b, "var p = document.getElementById('p'), q = document.getElementById('q'); [p.value, p.max, p.position, q.value, q.max, q.position].join()"), "3,4,0.75,0,1,-1");
        assert_eq!(js(&mut b, "p.value = 9; p.max = -1; [p.getAttribute('value'), p.max, p.value].join()"), "9,4,4");
        assert_eq!(js(&mut b, "var m = document.getElementById('m'); [m.value, m.min, m.max, m.low, m.high, m.optimum].join()"), "5,2,5,2,4,3.5");
        assert_eq!(js(&mut b, "m.value = 3; m.value"), "3");
    });
}

/// `dialog`: `open`, `show`, `showModal`, `close` with `returnValue` and the `close` event.
#[test]
fn dialog_opens_and_closes() {
    big(|| {
        let mut b = page("<dialog id=d></dialog>");
        assert_eq!(js(&mut b, "var d = document.getElementById('d'), n = 0; d.addEventListener('close', function () { n++ }); [d.open, d.returnValue === ''].join()"), "false,true");
        assert_eq!(js(&mut b, "d.show(); [d.open, d.hasAttribute('open')].join()"), "true,true");
        assert_eq!(js(&mut b, "d.close('ok'); [d.open, d.returnValue, n].join()"), "false,ok,1");
        assert_eq!(js(&mut b, "d.close('again'); [d.returnValue, n].join()"), "ok,1");
        assert_eq!(js(&mut b, "d.showModal(); d.showModal(); d.open"), "true");
        assert_eq!(js(&mut b, "d.close(); d.returnValue = 'x'; [d.open, d.returnValue, n].join()"), "false,x,2");
        assert_eq!(js(&mut b, "d.show(); d.showModal()"), "throw:InvalidStateError:Failed to execute 'showModal' on 'HTMLDialogElement': The dialog is already open as a non-modal dialog, and therefore cannot be opened as a modal dialog.");
    });
}

/// The reflections that the elements with a module of their own missed: `a`, `img`, `input`, `button`, `form`, `link`, `iframe`,
/// `fieldset`, `output`.
#[test]
fn module_tags_reflect_the_attributes_pages_read() {
    big(|| {
        let mut b = page("<a id=a href=/x rel=noopener target=_blank hreflang=en type=text/html ping=/p>link <b>text</b></a><img id=i usemap=#m ismap crossorigin=use-credentials>\
            <form id=f action=/go method=POST enctype=text/plain target=t name=nf accept-charset=utf-8><fieldset id=fs name=fn disabled><input id=in name=n placeholder=Search min=1 max=9 step=2 pattern=[a-z]+ autocomplete=off multiple maxlength=5 minlength=2 size=0><output id=o name=on>4</output></fieldset><button id=bt value=v>b</button><select id=s></select><input type=image id=im></form>\
            <link id=l media=print type=text/css integrity=i hreflang=fr disabled><iframe id=fr src=/f srcdoc=hi allow=fullscreen width=10 height=20 loading=lazy referrerpolicy=no-referrer></iframe>");
        assert_eq!(js(&mut b, "var a = document.getElementById('a'); [a.rel, a.target, a.hreflang, a.type, a.ping, a.text].join('|')"), "noopener|_blank|en|text/html|/p|link text");
        assert_eq!(js(&mut b, "a.text = 'new'; a.target = 'x'; [a.textContent, a.getAttribute('target')].join()"), "new,x");
        assert_eq!(js(&mut b, "var i = document.getElementById('i'); [i.useMap, i.isMap, i.crossOrigin, document.createElement('img').crossOrigin].join()"), "#m,true,use-credentials,");
        assert_eq!(js(&mut b, "i.crossOrigin = 'zz'; i.crossOrigin + ',' + (document.createElement('img').crossOrigin === null)"), "anonymous,true");
        assert_eq!(js(&mut b, "var n = document.getElementById('in'); [n.placeholder, n.min, n.max, n.step, n.pattern, n.autocomplete, n.multiple, n.maxLength, n.minLength, n.size].join()"), "Search,1,9,2,[a-z]+,off,true,5,2,20");
        assert_eq!(js(&mut b, "var e = document.createElement('input'); [e.maxLength, e.minLength, e.size].join()"), "-1,-1,20");
        assert_eq!(js(&mut b, "n.placeholder = 'p'; n.maxLength = 3; [n.getAttribute('placeholder'), n.getAttribute('maxlength')].join()"), "p,3");
        assert_eq!(js(&mut b, "n.maxLength = -2"), "throw:IndexSizeError:Failed to set the 'maxLength' property on 'HTMLInputElement': The value provided (-2) is negative.");
        assert_eq!(js(&mut b, "document.getElementById('bt').value"), "v");
        assert_eq!(js(&mut b, "var f = document.getElementById('f'); [f.action.endsWith('/go'), f.method, f.enctype, f.target, f.name, f.acceptCharset, f.autocomplete].join()"), "true,post,text/plain,t,nf,utf-8,on");
        assert_eq!(js(&mut b, "var g = document.createElement('form'); [g.method, g.enctype].join()"), "get,application/x-www-form-urlencoded");
        assert_eq!(js(&mut b, "[f.elements.length, f.length, f.elements[0].id, f.elements[1].id, f.elements[2].id, f.elements[3].id, f.elements[4].id, f.elements.item(5)].join()"), "5,5,fs,in,o,bt,s,");
        assert_eq!(js(&mut b, "f.elements === f.elements"), "true");
        assert_eq!(js(&mut b, "var fs = document.getElementById('fs'); [fs.name, fs.disabled, fs.type, fs.form === f].join()"), "fn,true,fieldset,true");
        assert_eq!(js(&mut b, "var o = document.getElementById('o'); [o.name, o.type, o.value, o.form === f].join()"), "on,output,4,true");
        assert_eq!(js(&mut b, "o.value = '5'; o.textContent"), "5");
        assert_eq!(js(&mut b, "var l = document.getElementById('l'); [l.media, l.type, l.integrity, l.hreflang, l.disabled].join()"), "print,text/css,i,fr,true");
        assert_eq!(js(&mut b, "var fr = document.getElementById('fr'); [fr.src.endsWith('/f'), fr.srcdoc, fr.allow, fr.width, fr.height, fr.loading, fr.referrerPolicy].join()"), "true,hi,fullscreen,10,20,lazy,no-referrer");
    });
}

/// The tags whose interface has members of its own get their prototype from the first wrapper (`has_members` in `browser_html.rs`); the
/// others share `HTMLElement.prototype` until a page names an `HTML*Element` global.
#[test]
fn tags_with_members_get_their_prototype_from_the_first_wrapper() {
    big(|| {
        let mut b = page("<p>x</p>");
        let own = "base canvas data details dialog fieldset html meta meter ol output progress source table thead tbody tfoot td th time tr";
        let r = js(&mut b, &format!("var h = Object.getPrototypeOf(document.createElement('section')); '{own}'.split(' ').filter(function (t) {{ var p = Object.getPrototypeOf(document.createElement(t)); return p === h || Object.getOwnPropertyNames(p).length < 2 }}).join()"));
        assert_eq!(r, "");
        let plain = "div span p ul li br hr pre h1 head title";
        let r = js(&mut b, &format!("'{plain}'.split(' ').filter(function (t) {{ return Object.getPrototypeOf(document.createElement(t)) !== h }}).join()"));
        assert_eq!(r, "");
    });
}

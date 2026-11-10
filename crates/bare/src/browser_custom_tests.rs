//! Tests of `browser_custom`: custom elements. The Chrome cases first (`tools/fixtures/ui/custom-cases.txt`, 49 cases, `custom-form-cases.txt`, 19 cases, and
//! `custom-page-cases.txt`, 15 cases whose scripts run while the page loads), then what Chrome cannot show here: the cost of a page that
//! defines nothing, exceptions of callbacks, failed constructors and the collector.

use crate::browser::*;
use crate::browser_loader::*;
use crate::browser_topic_tests::{parse, topic_test};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;

topic_test!(chrome_cases, "custom", 49);
topic_test!(chrome_form_cases, "custom-form", 20);

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// The answer of this host for a case whose scripts are in the page (`run_page`), in the format of the Chrome file.
fn run_loaded(k: &crate::browser_topic_tests::Case) -> String {
    let html = format!("<!doctype html><html><head></head><body>{}</body></html>", k.html);
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let r = b.run_page(PageConfig { url: "http://example.test/p".into(), client: Client::new(), limits: PageLimits::default(), fetch: None });
    assert_eq!(r.stop, Stop::Settled, "{r:?}");
    let reader = b.run_script(&k.script).unwrap();
    let v = b.it.call(reader, Value::Undefined, &[]).unwrap();
    b.it.display(v).replace('\n', "\\n")
}

#[test]
fn chrome_page_cases() {
    let (cases, chrome) = (include_str!("../../../tools/fixtures/ui/custom-page-cases.txt"), include_str!("../../../tools/fixtures/ui/chrome-custom-page.txt"));
    let chrome: std::collections::HashMap<&str, &str> = chrome.lines().filter_map(|l| l.split_once('\t')).collect();
    let cases = parse(cases);
    assert_eq!(cases.len(), 16);
    let bad: Vec<String> = big(move || {
        let chrome: std::collections::HashMap<String, String> = chrome.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        cases.iter().filter_map(|k| {
            let got = run_loaded(k);
            let want = &chrome[&k.name];
            (&got != want).then(|| format!("{}: chrome {want:?} bare {got:?}", k.name))
        }).collect()
    });
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn a_page_that_defines_nothing_has_no_watcher_and_no_microtask() {
    let mut b = page("<x-a id=a></x-a>");
    js(&mut b, "var a = document.getElementById('a'); a.appendChild(document.createElement('x-b')); a.setAttribute('x', '1'); a.cloneNode(true); a.innerHTML = '<x-c></x-c>'; a.remove()");
    assert_eq!(b.it.pending_jobs(), 0);
    assert!(!b.document().borrow().has_watcher());
    // The global is an accessor until the first read (the lazy install).
    assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(window, 'customElements').get"), "function");
    assert_eq!(js(&mut b, "typeof customElements.get"), "function");
    // `customElements` stays an accessor with no setter, as in Chrome.
    assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(window, 'customElements').value + typeof Object.getOwnPropertyDescriptor(window, 'customElements').set"), "undefinedundefined");
    // A read does not start the machinery: only a definition does.
    assert!(!b.document().borrow().has_watcher());
    js(&mut b, "customElements.define('x-q', class extends HTMLElement {})");
    assert!(b.document().borrow().has_watcher());
}

#[test]
fn a_throwing_callback_is_reported_and_the_next_one_runs() {
    let mut b = page("<div id=p></div>");
    js(&mut b, "var log = []; customElements.define('x-a', class extends HTMLElement { connectedCallback() { log.push('c ' + this.id); throw new TypeError('boom') } disconnectedCallback() { log.push('d ' + this.id) } })");
    assert_eq!(js(&mut b, "var p = document.getElementById('p'); p.innerHTML = '<x-a id=a></x-a><x-a id=b></x-a>'; log.join()"), "c a,c b");
    let r = b.take_reports();
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].message.lines().next().unwrap(), "TypeError: boom");
    assert!(r[0].message.contains("\n    at "), "{r:?}");
    assert_eq!(js(&mut b, "p.innerHTML = ''; log.join()"), "c a,c b,d a,d b");
}

#[test]
fn a_constructor_that_throws_fails_the_upgrade_and_is_reported() {
    let mut b = page("<x-a id=a></x-a><x-a id=b></x-a>");
    assert_eq!(js(&mut b, "var n = 0; customElements.define('x-a', class K extends HTMLElement { constructor() { super(); if (n++ == 0) throw new Error('first') } }); var a = document.getElementById('a'), c = document.getElementById('b'); [a.constructor.name, c.constructor.name, a.matches(':defined'), c.matches(':defined')].join()"), "K,K,false,true");
    let r = b.take_reports();
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].message.lines().next().unwrap(), "Error: first");
    assert!(r[0].message.contains("\n    at "), "{r:?}");
    // A failed element stays as it is: a second upgrade does nothing.
    assert_eq!(js(&mut b, "customElements.upgrade(document.body); n"), "2");
}

#[test]
fn create_element_with_a_bad_constructor_reports_and_still_makes_an_element() {
    let mut b = page("");
    js(&mut b, "customElements.define('x-a', class extends HTMLElement { constructor() { super(); this.setAttribute('q', '1') } })");
    assert_eq!(js(&mut b, "var e = document.createElement('x-a'); e.localName + ' ' + e.attributes.length + ' ' + (e.constructor === HTMLUnknownElement)"), "x-a 0 true");
    let r = b.take_reports();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].message.starts_with("NotSupportedError"), "{r:?}");
    assert!(r[0].message.contains("\n    at "), "{r:?}");
    js(&mut b, "customElements.define('x-b', class extends HTMLElement { constructor() { return document.createElement('div') } })");
    assert_eq!(js(&mut b, "document.createElement('x-b').localName"), "x-b");
    // An element of another name is a NotSupportedError, as the standard says (Chrome reports it with an empty message).
    assert!(b.take_reports()[0].message.starts_with("NotSupportedError"));
}

#[test]
fn definitions_and_elements_survive_a_collection() {
    let mut b = page("<div id=p></div>");
    js(&mut b, "var log = []; customElements.define('x-a', class extends HTMLElement { constructor() { super(); log.push('ctor') } connectedCallback() { log.push('c') } static get observedAttributes() { return ['a'] } attributeChangedCallback(n, o, v) { log.push(n + v) } })");
    b.collect(&[]).unwrap();
    js(&mut b, "var p = document.getElementById('p'); p.innerHTML = '<x-a a=1></x-a>'; p.firstChild.setAttribute('a', '2')");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "p.firstChild.setAttribute('a', '3'); log.join()"), "ctor,a1,c,a2,a3");
    assert_eq!(js(&mut b, "Object.getPrototypeOf(p.firstChild) === customElements.get('x-a').prototype"), "true");
}

#[test]
fn form_values_internals_and_states_survive_a_collection() {
    let mut b = page("<form id=f><x-f name=n></x-f><x-f name=m></x-f></form>");
    js(&mut b, "customElements.define('x-f', class extends HTMLElement { static get formAssociated() { return true } constructor() { super(); this.i = this.attachInternals() } })");
    js(&mut b, "var fm = document.getElementById('f'), x = fm.children; x[0].i.setFormValue(new File(['a'], 'a.txt')); var d = new FormData(); d.append('k', 'v'); x[1].i.setFormValue(d); x[0].i.states.add('s')");
    b.collect(&[]).unwrap();
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "Array.from(new FormData(fm)).map(function (e) { return e[0] + '=' + (typeof e[1] === 'string' ? e[1] : e[1].name) }).join()"), "n=a.txt,k=v");
    assert_eq!(js(&mut b, "x[0].i.states.has('s') + ' ' + x[0].matches(':state(s)') + ' ' + (x[0].i.states === x[0].i.states)"), "true true true");
}

/// A page with Lit (`BARE_LIT_JS`: a classic bundle that defines `my-counter`; `BARE_LIT_PRE`: a script to run before it), run by hand: `BARE_LIT_JS=... cargo test -p bare lit_page -- --ignored --nocapture`.
#[test]
#[ignore]
fn lit_page() {
    let js_src = std::fs::read_to_string(std::env::var("BARE_LIT_JS").expect("BARE_LIT_JS")).expect("bundle");
    let pre = std::env::var("BARE_LIT_PRE").unwrap_or_default();
    let html = format!("<!doctype html><html><head></head><body><my-counter id=c></my-counter><script>{pre}</script><script>try {{ {js_src}\n}} catch (e) {{ window.__err = e.name + ': ' + e.message + '\\n' + e.stack }}</script></body></html>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let r = b.run_page(PageConfig { url: "http://example.test/p".into(), client: Client::new(), limits: PageLimits::default(), fetch: None });
    eprintln!("{r:?}");
    for q in [
        "String(window.__lit)",
        "String(window.__err)",
        "var c = document.getElementById('c'); String(c.constructor.name) + ' ' + (c.shadowRoot && c.shadowRoot.mode)",
        "c.shadowRoot ? c.shadowRoot.innerHTML : 'no shadow root'",
        "c.shadowRoot ? String(c.shadowRoot.adoptedStyleSheets && c.shadowRoot.adoptedStyleSheets.length) + ' ' + c.shadowRoot.querySelectorAll('style').length : ''",
        "String(c.count) + ' ' + String(c.isUpdatePending) + ' ' + String(c.hasUpdated)",
        "c.shadowRoot.querySelector('button').click(); String(c.count)",
        "c.shadowRoot.querySelector('button').dispatchEvent(new MouseEvent('click', { bubbles: true })); String(c.count)",
        "c.setAttribute('count', '5'); c.updateComplete.then(function () { window.__after3 = c.count + '|' + c.shadowRoot.querySelector('button').textContent }); 1",
        "String(window.__after3)",
        "var e2 = document.createElement('my-counter'); document.body.appendChild(e2); e2.updateComplete.then(function () { window.__after4 = e2.shadowRoot.innerHTML.length }); 1",
        "String(window.__after4)",
        "var cc = document.getElementById('c'); cc.remove(); document.body.appendChild(cc); String(cc.isConnected)",
        "String(Object.keys(window).filter(function (k) { return /lit|Lit/.test(k) }).join()) + ' ' + String(window.litHtmlVersions) + ' ' + String(window.litElementVersions) + ' ' + String(window.reactiveElementVersions)",

        "c.updateComplete.then(function () { window.__after = c.shadowRoot.querySelector('button').textContent }); 1",
        "String(window.__after)",
        "c.label = 'taps'; 1",
        "c.updateComplete.then(function () { window.__after2 = c.shadowRoot.querySelector('button').textContent }); 1",
        "String(window.__after2)",
    ] {
        let v = b.run_script(q).map(|v| b.it.display(v));
        let _ = b.drain();
        eprintln!("{q}\n  => {v:?}");
    }
}

#[test]
fn a_whendefined_promise_that_nothing_holds_is_collected_and_still_resolves() {
    let mut b = page("");
    js(&mut b, "var n = 0; customElements.whenDefined('x-a').then(function () { n++ })");
    b.collect(&[]).unwrap();
    js(&mut b, "customElements.define('x-a', class extends HTMLElement {})");
    b.drain().unwrap();
    assert_eq!(js(&mut b, "n"), "1");
}

#[test]
fn the_parser_does_not_run_a_constructor_in_a_template() {
    let mut b = page("<template id=t><x-a></x-a></template>");
    js(&mut b, "var n = 0; customElements.define('x-a', class extends HTMLElement { constructor() { super(); n++ } })");
    assert_eq!(js(&mut b, "document.getElementById('t').content.firstChild.constructor.name + n"), "HTMLElement0");
}

#[test]
fn upgrade_candidates_follow_document_order_across_shadow_roots() {
    let mut b = page("<x-a id=a><x-b id=b></x-b></x-a><x-b id=c></x-b>");
    js(&mut b, "var log = []; var h = document.getElementById('a'); var s = h.attachShadow({ mode: 'open' }); var d = document.createElement('x-b'); d.id = 'inshadow'; s.appendChild(d)");
    assert_eq!(js(&mut b, "var mk = function (n) { return class extends HTMLElement { constructor() { super(); log.push(n + ':' + this.id) } } }; customElements.define('x-b', mk('B')); customElements.define('x-a', mk('A')); log.join()"), "B:inshadow,B:b,B:c,A:a");
}

#[test]
fn many_definitions_and_deep_trees_are_bounded() {
    let mut b = page("");
    assert_eq!(js(&mut b, "for (var i = 0; i < 5000; i++) customElements.define('x-n' + i, class extends HTMLElement {}); 1"), "1");
    assert_eq!(js(&mut b, "var d = document.createElement('div'), c = d; for (var i = 0; i < 300; i++) { var e = document.createElement('x-n1'); c.appendChild(e); c = e } document.body.appendChild(d); document.body.removeChild(d); 1"), "1");
}

#[test]
fn define_inside_the_reads_of_define_is_refused() {
    let mut b = page("");
    let src = "var r; var C = class extends HTMLElement { attributeChangedCallback() {} static get observedAttributes() { try { customElements.define('x-in', class extends HTMLElement {}) } catch (e) { r = e.name + ': ' + e.message } return new Set(['a', 'b']) } }; customElements.define('x-out', C); r";
    assert_eq!(js(&mut b, src), "NotSupportedError: Failed to execute 'define' on 'CustomElementRegistry': an element definition is already being processed");
    // The flag is gone after the call, and the iterable gave its two names.
    assert_eq!(js(&mut b, "customElements.define('x-in', class extends HTMLElement {}); customElements.get('x-in') !== undefined"), "true");
}

#[test]
fn an_is_value_that_createelement_kept_upgrades_the_element_at_insertion() {
    let mut b = page("");
    assert_eq!(js(&mut b, "var e = document.createElement('button', { is: 'x-btn' }); e.getAttribute('is') + ' ' + e.matches(':defined')"), "null true");
    assert_eq!(js(&mut b, "var log = []; var B = class extends HTMLButtonElement { connectedCallback() { log.push('c') } }; customElements.define('x-btn', B, { extends: 'button' }); document.body.appendChild(e); (e instanceof B) + ' ' + log.join()"), "true c");
    assert_eq!(js(&mut b, "var f = document.createElement('button'); document.body.appendChild(f); f instanceof B"), "false");
}

#[test]
fn toggle_attribute_validates_the_name_and_skips_a_no_change() {
    let mut b = page("<p id=p></p>");
    assert_eq!(js(&mut b, "var p = document.getElementById('p'); [p.toggleAttribute('A'), p.hasAttribute('a'), p.toggleAttribute('a', true), p.toggleAttribute('a', false), p.toggleAttribute('a', false), Element.prototype.toggleAttribute.length].join()"), "true,true,true,false,false,1");
    assert_eq!(js(&mut b, "try { p.toggleAttribute('1 a') } catch (e) { e.name }"), "InvalidCharacterError");
}

#[test]
fn a_callback_that_defines_and_creates_elements_runs_each_reaction_once() {
    let mut b = page("<div id=p><x-a id=a></x-a></div>");
    let src = "var log = []; customElements.define('x-a', class extends HTMLElement { connectedCallback() { log.push('c' + this.id); if (this.id == 'a') { var n = document.createElement('x-a'); n.id = 'n'; this.parentNode.appendChild(n) } } disconnectedCallback() { log.push('d' + this.id) } }); document.getElementById('a').remove(); log.join()";
    assert_eq!(js(&mut b, src), "ca,cn,da");
}

#[test]
fn the_registry_is_branded_and_define_checks_its_arguments_before_state() {
    let mut b = page("");
    assert_eq!(js(&mut b, "try { customElements.define.call({}, 'x-a', class extends HTMLElement {}) } catch (e) { e.name + ': ' + e.message }"), "TypeError: Illegal invocation");
    assert_eq!(js(&mut b, "try { customElements.upgrade(5) } catch (e) { e.name + ': ' + e.message }"), "TypeError: Failed to execute 'upgrade' on 'CustomElementRegistry': parameter 1 is not of type 'Node'.");
    assert_eq!(js(&mut b, "customElements.whenDefined('Bad').then(null, function (e) { globalThis.w = e.name }); 1"), "1");
    b.drain().unwrap();
    assert_eq!(js(&mut b, "w"), "SyntaxError");
}

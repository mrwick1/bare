//! Tests of `browser_img.rs` (`HTMLImageElement`, `Image`) and `browser_impl.rs` (`document.implementation`). The page that downloads the
//! pictures is not here: `crates/drive/tests/images.rs` runs the whole path. These tests call `img_requests` and `img_settled` as the page does.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn browser(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><html><head></head><body>{body}</body></html>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn the_constructors_of_the_page_exist() {
    let mut b = browser("");
    assert_eq!(js(&mut b, "[typeof Image, typeof Option, typeof Audio, typeof HTMLImageElement].join()"), "function,function,function,function");
}

#[test]
fn image_makes_a_detached_img() {
    let mut b = browser("<img id=a width=7 alt=x>");
    let r = js(&mut b, "var i = new Image(3, 4); [i instanceof HTMLImageElement, i instanceof HTMLElement, i.tagName, i.isConnected, i.width, i.height, i.getAttribute('width'), i.complete, i.naturalWidth, i.naturalHeight, i.src === '', i.onload, i.onerror, Image.prototype === HTMLImageElement.prototype, Object.prototype.toString.call(i)].join()");
    assert_eq!(r, "true,true,IMG,false,3,4,3,true,0,0,true,,,true,[object HTMLImageElement]");
    assert_eq!(js(&mut b, "var j = new Image(); [j.hasAttribute('width'), j.width].join()"), "false,0");
    // An `<img>` of the page has the same interface.
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); [a instanceof HTMLImageElement, a.width, a.alt, a.complete].join()"), "true,7,x,true");
    assert!(js(&mut b, "new HTMLImageElement()").starts_with("throw:TypeError:Failed to construct 'HTMLImageElement'"));
    assert_eq!(js(&mut b, "typeof Image.prototype.decode"), "function");
}

#[test]
fn src_is_reflected_and_asks_the_page() {
    let mut b = browser("");
    assert_eq!(js(&mut b, "var i = new Image(); i.src = 'data:image/png;base64,AA=='; [i.complete, i.getAttribute('src'), i.src, i.currentSrc].join()"), "false,data:image/png;base64,AA==,data:image/png;base64,AA==,data:image/png;base64,AA==");
    assert_eq!(b.img_requests().len(), 1);
    assert!(b.img_requests().is_empty());
    // `setAttribute` asks too; an empty value does not.
    js(&mut b, "var k = new Image(); k.setAttribute('src', 'data:image/png;base64,AA=='); var e = new Image(); e.src = '';");
    assert_eq!(b.img_requests().len(), 1);
    // The same element twice is one request.
    js(&mut b, "i.src = 'data:image/png;base64,AB=='; i.src = 'data:image/png;base64,AC==';");
    assert_eq!(b.img_requests().len(), 1);
}

#[test]
fn load_and_error_fire_in_a_task_and_end_complete() {
    let mut b = browser("");
    js(&mut b, "var log = []; var i = new Image(); i.onload = function (e) { log.push('load:' + e.type + ':' + (e.target === i) + ':' + i.complete); }; i.onerror = function () { log.push('error'); }; i.addEventListener('load', function () { log.push('listener'); }); i.src = 'data:image/png;base64,AA==';");
    let n = b.img_requests()[0];
    assert_eq!(js(&mut b, "log.join()"), "");
    b.img_settled(n, true);
    b.advance_time(10.0, 100).unwrap();
    assert_eq!(js(&mut b, "log.join()"), "load:load:true:true,listener");
    js(&mut b, "i.src = 'data:image/png;base64,AB==';");
    let n = b.img_requests()[0];
    b.img_settled(n, false);
    b.advance_time(10.0, 100).unwrap();
    assert_eq!(js(&mut b, "log.join()"), "load:load:true:true,listener,error");
}

#[test]
fn decode_follows_the_download() {
    let mut b = browser("");
    js(&mut b, "var out = []; var i = new Image(); i.src = 'data:image/png;base64,AA=='; i.decode().then(function () { out.push('ok'); }, function (e) { out.push(e.name); }); var j = new Image(); j.decode().catch(function (e) { out.push('j:' + e.name); });");
    b.advance_time(10.0, 100).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "j:EncodingError");
    let n = b.img_requests()[0];
    b.img_settled(n, false);
    b.advance_time(10.0, 100).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "j:EncodingError,EncodingError");
    js(&mut b, "var k = new Image(); k.src = 'data:image/png;base64,AA=='; k.decode().then(function () { out.push('k'); });");
    let n = b.img_requests()[0];
    b.img_settled(n, true);
    b.advance_time(10.0, 100).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "j:EncodingError,EncodingError,k");
}

#[test]
fn a_page_without_images_does_not_make_the_interface() {
    let mut b = browser("<p>x</p>");
    js(&mut b, "document.querySelector('p').textContent");
    assert!(b.dom.img.made.get().is_none());
}

#[test]
fn create_html_document_makes_a_detached_document() {
    let mut b = browser("<p id=p>page</p>");
    let r = js(&mut b, "var d2 = document.implementation.createHTMLDocument('T'); [d2 === document, d2.nodeType, d2.head.firstChild.tagName + d2.head.firstChild.textContent, d2.documentElement.tagName, d2.head.tagName, d2.body.tagName, d2.doctype.nodeName, d2.documentElement.isConnected, d2.defaultView, d2.body.ownerDocument === d2, d2.documentElement.ownerDocument === d2].join()");
    assert_eq!(r, "false,9,TITLET,HTML,HEAD,BODY,html,false,,true,true");
    let r = js(&mut b, "d2.body.innerHTML = '<ul><li class=a>one</li><li>two</li></ul><script>window.ran = 1<\\/script>'; [d2.querySelectorAll('li').length, d2.querySelector('.a').textContent, d2.body.firstChild.tagName, typeof window.ran, document.getElementById('p') !== null, d2.getElementById('p'), document.querySelectorAll('li').length].join()");
    assert_eq!(r, "2,one,UL,undefined,true,,0");
    // No title argument: no `title` element.
    assert_eq!(js(&mut b, "var d3 = document.implementation.createHTMLDocument(); [d3.head.childNodes.length, document.implementation === d3.implementation, document.implementation.hasFeature()].join()"), "0,true,true");
    // The page title is not the title of the new document.
    assert_eq!(js(&mut b, "document.implementation.createHTMLDocument('').head.childNodes.length"), "1");
    // Nodes move into the page.
    js(&mut b, "document.body.appendChild(d2.body.firstChild);");
    assert_eq!(js(&mut b, "[document.querySelectorAll('li').length, document.body.lastChild.ownerDocument === document, document.body.lastChild.isConnected].join()"), "2,true,true");
    assert!(js(&mut b, "document.implementation.createHTMLDocument.call({})").starts_with("throw:TypeError"));
}

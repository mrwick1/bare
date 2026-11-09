//! Tests of `browser_img.rs` (`Image`, `decode()`, the events). The page that downloads the
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
    assert!(js(&mut b, "new HTMLImageElement()").starts_with("throw:TypeError"));
    assert_eq!(js(&mut b, "typeof Image.prototype.decode"), "function");
}

#[test]
fn src_is_reflected_and_asks_the_page() {
    let mut b = browser("");
    assert_eq!(js(&mut b, "var i = new Image(); i.src = 'http://x.test/a.png'; [i.complete, i.getAttribute('src'), i.src, i.currentSrc].join()"), "false,http://x.test/a.png,http://x.test/a.png,http://x.test/a.png");
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

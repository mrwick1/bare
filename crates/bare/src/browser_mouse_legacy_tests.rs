//! Tests of the legacy members of `UIEvent` and `MouseEvent`: `which`, `sourceCapabilities`, `layerX`, `layerY`, `fromElement`, `toElement`
//! and `initMouseEvent` (`browser_mouse_legacy.rs`). The values come from UI Events, CSSOM View and Chrome's legacy members. Nothing here
//! was compared with a real browser.

use crate::browser::*;
use bare_html::tree;
use crate::eng_old::OldFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_js::value::Value;

const PAGE: &str = "<!doctype html><html><head></head><body style='margin:0'><div id=a style='position:relative;margin-left:30px;margin-top:20px;width:100px;height:100px'>\
<div id=b style='width:50px;height:50px'></div></div><div id=c>c</div></body></html>";

fn browser() -> Browser {
    Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(&format!("var $ = function (s) {{ return document.getElementById(s); }};{src}")) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn which_of_a_mouse_event_is_the_button_plus_one_for_press_events() {
    let mut b = browser();
    assert_eq!(js(&mut b, "['mousedown', 'mouseup', 'click'].map(function (t) { return new MouseEvent(t, {button: 2}).which; }).join()"), "3,3,3");
    assert_eq!(js(&mut b, "new MouseEvent('mousedown').which"), "1");
    assert_eq!(js(&mut b, "new MouseEvent('mousemove', {button: 1}).which"), "0");
    assert_eq!(js(&mut b, "new MouseEvent('mouseover').which"), "0");
}

#[test]
fn which_of_a_keyboard_event_is_the_key_code_and_a_plain_ui_event_has_none() {
    let mut b = browser();
    assert_eq!(js(&mut b, "new KeyboardEvent('keydown', {keyCode: 65}).which"), "65");
    assert_eq!(js(&mut b, "'which' in MouseEvent.prototype && 'which' in KeyboardEvent.prototype && 'which' in InputEvent.prototype"), "true");
    assert_eq!(js(&mut b, "new InputEvent('input').which"), "0");
}

#[test]
fn source_capabilities_is_null() {
    let mut b = browser();
    assert_eq!(js(&mut b, "[new MouseEvent('click').sourceCapabilities, new KeyboardEvent('keydown').sourceCapabilities].join()"), ",");
    assert_eq!(js(&mut b, "new MouseEvent('click').sourceCapabilities === null && 'sourceCapabilities' in InputEvent.prototype"), "true");
}

#[test]
fn from_element_and_to_element_follow_the_type() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var o = new MouseEvent('mouseover', {relatedTarget: $('c'), bubbles: true}); var r; $('b').addEventListener('mouseover', function (e) { r = [e.fromElement.id, e.toElement.id].join(); }); $('b').dispatchEvent(o); r"), "c,b");
    assert_eq!(js(&mut b, "var o = new MouseEvent('mouseout', {relatedTarget: $('c')}); var r; $('b').addEventListener('mouseout', function (e) { r = [e.fromElement.id, e.toElement.id].join(); }); $('b').dispatchEvent(o); r"), "b,c");
    assert_eq!(js(&mut b, "var r; $('b').addEventListener('mousemove', function (e) { r = [e.fromElement, e.toElement].join(); }); $('b').dispatchEvent(new MouseEvent('mousemove', {relatedTarget: $('c')})); r"), ",");
    assert_eq!(js(&mut b, "new MouseEvent('mouseover').fromElement"), "null");
    assert_eq!(js(&mut b, "new MouseEvent('mouseover').toElement"), "null");
}

#[test]
fn layer_xy_are_page_coordinates_without_a_positioned_ancestor() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var r; $('c').addEventListener('click', function (e) { r = [e.layerX, e.layerY].join(); }); $('c').dispatchEvent(new MouseEvent('click', {clientX: 12, clientY: 130})); r"), "12,130");
}

#[test]
fn layer_xy_are_relative_to_the_nearest_positioned_ancestor() {
    let mut b = browser();
    // #a is at (30, 20) and position:relative, so a click at the client point (40, 50) is at (10, 30) inside it.
    assert_eq!(js(&mut b, "var r; $('b').addEventListener('click', function (e) { r = [e.layerX, e.layerY].join(); }); $('b').dispatchEvent(new MouseEvent('click', {clientX: 40, clientY: 50})); r"), "10,30");
    assert_eq!(js(&mut b, "new MouseEvent('click', {clientX: 7, clientY: 9}).layerX"), "7");
}

#[test]
fn init_mouse_event_sets_the_fields_before_dispatch() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var e = document.createEvent('MouseEvents'); e.initMouseEvent('click', true, true, window, 3, 1, 2, 3, 4, true, false, true, false, 2, $('c')); [e.type, e.bubbles, e.cancelable, e.view === window, e.detail, e.screenX, e.screenY, e.clientX, e.clientY, e.ctrlKey, e.altKey, e.shiftKey, e.metaKey, e.button, e.relatedTarget.id].join()"), "click,true,true,true,3,1,2,3,4,true,false,true,false,2,c");
    assert_eq!(js(&mut b, "var f = new MouseEvent('click'); f.initMouseEvent('mouseup', false, false, null, 0, 0, 0, 0, 0, false, false, false, false, 0, null); f.type + ',' + f.view + ',' + f.relatedTarget"), "mouseup,null,null");
    assert_eq!(js(&mut b, "var r; $('b').addEventListener('click', function (e) { r = e.clientX + ',' + e.pageX + ',' + e.isTrusted; }); var g = new MouseEvent('x'); g.initMouseEvent('click', true, true, window, 0, 0, 0, 5, 6, false, false, false, false, 0, null); $('b').dispatchEvent(g); r"), "5,5,false");
}

#[test]
fn init_mouse_event_does_nothing_while_the_event_dispatches() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var r; $('b').addEventListener('click', function (e) { e.initMouseEvent('keyup', false, false, null, 0, 9, 9, 9, 9, true, true, true, true, 1, null); r = [e.type, e.clientX, e.ctrlKey].join(); }); $('b').dispatchEvent(new MouseEvent('click', {clientX: 4})); r"), "click,4,false");
}

#[test]
fn init_mouse_event_needs_a_mouse_event_and_arguments() {
    let mut b = browser();
    assert_eq!(js(&mut b, "new MouseEvent('click').initMouseEvent()"), "throw:TypeError:Failed to execute 'initMouseEvent' on 'MouseEvent': 1 argument required, but only 0 present.");
    assert_eq!(js(&mut b, "MouseEvent.prototype.initMouseEvent.call(new Event('x'), 'click')"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "MouseEvent.prototype.initMouseEvent.length"), "15");
}

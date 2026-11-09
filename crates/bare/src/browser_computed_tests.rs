//! Tests of `browser_computed`: `window.getComputedStyle` over the native cascade. Expectations come from CSSOM, CSS
//! Cascade and the HTML rendering section, read by hand. Node has no DOM, so it is not an oracle here.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use crate::browser_computed::ComputedLimits;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(chrome_cases, "computed", 107);

const PAGE: &str = "<!doctype html><html id=h><head><style>.r { direction: rtl } #c { color: red }</style></head><body id=b><div id=a class=x><p id=p>t</p></div><span id=s dir=rtl><i id=i></i></span></body></html>";

fn at(page: &str) -> Browser {
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn root_direction_shape() {
    let mut b = at(PAGE);
    // The shape of the cached bundle: `getComputedStyle(document.documentElement).direction`.
    assert_eq!(js(&mut b, "typeof getComputedStyle + ',' + getComputedStyle.length + ',' + (window.getComputedStyle === getComputedStyle)"), "function,1,true");
    assert_eq!(js(&mut b, "getComputedStyle(document.documentElement).direction"), "ltr");
    assert_eq!(js(&mut b, "getComputedStyle(document.documentElement).getPropertyValue('direction')"), "ltr");
    assert_eq!(js(&mut b, "Object.keys(window).indexOf('getComputedStyle') >= 0"), "true");
    // Another script, another call: nothing is kept in JS data.
    assert_eq!(js(&mut b, "document.documentElement.setAttribute('dir', 'rtl'); getComputedStyle(document.documentElement).direction"), "rtl");
    assert_eq!(js(&mut b, "document.documentElement.setAttribute('dir', 'ltr'); getComputedStyle(document.documentElement).direction"), "ltr");
}

#[test]
fn direction_inheritance_css_and_dir_attribute() {
    let mut b = at(PAGE);
    let d = "var g = function (id) { return getComputedStyle(document.getElementById(id)).direction; };";
    js(&mut b, d);
    // The `dir` attribute is a rule of the browser sheet and is inherited.
    assert_eq!(js(&mut b, "g('s') + g('i') + g('a')"), "rtlrtlltr");
    // The page beats the browser sheet: a class rule over `dir`, and the `style` attribute over both.
    assert_eq!(js(&mut b, "document.getElementById('s').className = 'r'; g('s')"), "rtl");
    assert_eq!(js(&mut b, "document.getElementById('s').setAttribute('style', 'direction: ltr'); g('s') + g('i')"), "ltrltr");
    // A class changes the style of the child through inheritance.
    assert_eq!(js(&mut b, "document.getElementById('a').className = 'x r'; g('a') + g('p')"), "rtlrtl");
    // An invalid `dir` is the same as none.
    assert_eq!(js(&mut b, "document.getElementById('a').className = ''; document.getElementById('a').setAttribute('dir', 'sideways'); g('a') + g('p')"), "ltrltr");
    // Inline CSSOM works with the native property.
    assert_eq!(js(&mut b, "var e = document.getElementById('p'); e.style.direction = 'rtl'; e.style.cssText + '|' + e.style.direction + '|' + g('p')"), "direction: rtl;|rtl|rtl");
    assert_eq!(js(&mut b, "e.style.direction = 'auto'; e.style.direction"), "rtl");
}

#[test]
fn dir_auto_and_bdi_are_gated_not_guessed() {
    let mut b = at("<!doctype html><html><body><div id=a dir=auto><p id=p>x</p></div><div dir=auto><i dir=ltr id=l></i></div><bdi id=bd>x</bdi><span id=n>y</span></body></html>");
    let d = |id: &str| format!("getComputedStyle(document.getElementById('{id}')).direction");
    let gate = "unsupported:direction depends on dir=auto or bdi (the text decides it)";
    assert_eq!(js(&mut b, &d("a")), gate);
    assert_eq!(js(&mut b, &d("p")), gate);
    assert_eq!(js(&mut b, &d("bd")), gate);
    // An explicit `dir` below an auto one decides it, and other elements are not affected.
    assert_eq!(js(&mut b, &d("l")), "ltr");
    assert_eq!(js(&mut b, &d("n")), "ltr");
    // Other properties of an auto element are fine.
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('p')).display"), "block");
}

#[test]
fn held_object_follows_class_style_and_style_element_changes() {
    let mut b = at(PAGE);
    js(&mut b, "var a = document.getElementById('a'), cs = getComputedStyle(a); 0");
    assert_eq!(js(&mut b, "cs.color + '|' + cs.display"), "rgb(0, 0, 0)|block");
    assert_eq!(js(&mut b, "a.id = 'c'; cs.color"), "rgb(255, 0, 0)");
    assert_eq!(js(&mut b, "a.style.color = 'blue'; cs.color"), "rgb(0, 0, 255)");
    assert_eq!(js(&mut b, "a.removeAttribute('style'); a.id = 'a'; cs.color"), "rgb(0, 0, 0)");
    // Embedded `<style>` text.
    assert_eq!(js(&mut b, "var st = document.querySelector('style'); st.textContent = '.x { display: flex; color: green }'; cs.display + '|' + cs.color"), "flex|rgb(0, 128, 0)");
    // A new `<style>` element, and one that is removed.
    assert_eq!(js(&mut b, "var n = document.createElement('style'); n.textContent = '#a { display: inline }'; document.head.appendChild(n); cs.display"), "inline");
    assert_eq!(js(&mut b, "document.head.removeChild(n); cs.display"), "flex");
    // The page's rules beat nothing of the browser's `display:none` for `head`.
    assert_eq!(js(&mut b, "getComputedStyle(document.head).display"), "none");
}

#[test]
fn appended_sheets_add_to_the_rules_and_an_earlier_sheet_rebuilds_them() {
    let mut b = at("<!doctype html><html><head><style>@layer a, b; @layer b { #a { color: red } } #p { display: inline }</style></head><body><div id=a></div><p id=p></p></body></html>");
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')), ps = getComputedStyle(document.getElementById('p')); 0");
    assert_eq!(js(&mut b, "cs.color + '|' + ps.display"), "rgb(255, 0, 0)|inline");
    let builds = b.computed_stats().builds;
    // Added at the end: a later sheet wins a tie, and a layer that is new comes after the layers that exist.
    let add = "var n = document.createElement('style'); n.textContent = '@layer a { #a { color: blue } } #p { display: flex }'; document.head.appendChild(n); cs.color + '|' + ps.display";
    assert_eq!(js(&mut b, add), "rgb(255, 0, 0)|flex");
    assert_eq!(b.computed_stats().builds, builds + 1);
    // Added before the first sheet: the rules are made again, in the order of the page.
    let first = "var m = document.createElement('style'); m.textContent = '#p { display: grid }'; document.head.insertBefore(m, document.head.firstChild); cs.color + '|' + ps.display";
    assert_eq!(js(&mut b, first), "rgb(255, 0, 0)|flex");
    assert_eq!(js(&mut b, "document.head.removeChild(n); cs.color + '|' + ps.display"), "rgb(255, 0, 0)|inline");
}

#[test]
fn viewport_changes_reach_the_held_object() {
    let mut b = at("<!doctype html><html><head><style>@media (max-width: 500px) { #a { direction: rtl } }</style></head><body><div id=a></div></body></html>");
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); 0");
    assert_eq!(js(&mut b, "cs.direction"), "ltr");
    b.set_viewport(400.0, 800.0).unwrap();
    assert_eq!(js(&mut b, "cs.direction"), "rtl");
    b.set_viewport(1280.0, 800.0).unwrap();
    assert_eq!(js(&mut b, "cs.direction"), "ltr");
}

#[test]
fn brand_and_conversion_order() {
    let mut b = at(PAGE);
    let tp = |r: String| r.starts_with("throw:TypeError");
    assert!(tp(js(&mut b, "getComputedStyle()")));
    assert!(tp(js(&mut b, "getComputedStyle({})")));
    assert!(tp(js(&mut b, "getComputedStyle(document.createTextNode('x'))")));
    assert!(tp(js(&mut b, "getComputedStyle(document)")));
    assert!(tp(js(&mut b, "getComputedStyle(null)")));
    // The element is checked before the pseudo-element argument is converted (no user code runs).
    assert_eq!(js(&mut b, "var ran = 0; try { getComputedStyle({}, { toString() { ran++; return '' } }); } catch (e) {} ran"), "0");
    // A valid element: the second argument is converted (user code runs once) and an empty result is the element's style.
    assert_eq!(js(&mut b, "ran = 0; getComputedStyle(document.body, { toString() { ran++; return '' } }).display + ran"), "block1");
    assert_eq!(js(&mut b, "[undefined, null, '', 'x'].map(function (p) { return getComputedStyle(document.body, p).display; }).join()"), "block,block,block,block");
    // The brand of the declaration: the inline declaration and the computed one do not mix.
    js(&mut b, "var cs = getComputedStyle(document.body), is = document.body.style; var P = CSSStyleDeclaration.prototype; var CP = Object.getPrototypeOf(cs); 0");
    assert_eq!(js(&mut b, "cs instanceof CSSStyleDeclaration && CP !== P && Object.getPrototypeOf(CP) === P"), "true");
    assert_eq!(js(&mut b, "P.getPropertyValue.call(cs, 'color')"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "CP.getPropertyValue.call(is, 'color')"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "CP.getPropertyValue.call({}, 'color')"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(CP, 'direction').get.call(is)"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(P, 'direction').get.call(cs)"), "throw:TypeError:Illegal invocation");
    // The brand is checked before the argument is converted.
    assert_eq!(js(&mut b, "ran = 0; try { CP.getPropertyValue.call({}, { toString() { ran++; return 'color' } }); } catch (e) {} ran"), "0");
    // The argument count is checked (`getPropertyValue` needs one).
    assert!(tp(js(&mut b, "cs.getPropertyValue()")));
    // Conversion runs user code once, with no state borrowed: the callback can change the page.
    assert_eq!(js(&mut b, "cs.getPropertyValue({ toString() { document.body.style.color = 'red'; return 'color'; } })"), "rgb(255, 0, 0)");
    // Not a constructor.
    assert!(tp(js(&mut b, "new CP.constructor()")));
}

const PSEUDO_PAGE: &str = "<!doctype html><html><head><style>#e { color: rgb(255, 0, 0); font-size: 20px; margin: 10px; width: 50px; border: 2px solid; display: block; position: relative; --x: 7 } #e::before { content: 'x'; color: blue }</style></head><body><div id=e>t</div></body></html>";

#[test]
fn a_pseudo_element_has_the_inherited_values_of_its_element_and_the_initial_rest() {
    let mut b = at(PSEUDO_PAGE);
    js(&mut b, "var e = document.getElementById('e');");
    for p in ["::before", "::after", ":before", ":after", "::first-line", ":first-line", "::first-letter", ":first-letter", "::marker", "::placeholder", "::selection", "::BEFORE", "::backdrop", "::-webkit-scrollbar"] {
        let src = format!("var cs = getComputedStyle(e, '{p}'); [cs.getPropertyValue('content'), cs.display, cs.color, cs.fontSize, cs.marginTop, cs.width, cs.borderTopWidth, cs.position].join('|')");
        assert_eq!(js(&mut b, &src), "normal|inline|rgb(255, 0, 0)|20px|0px|auto|0px|static", "{p}");
    }
    // The object is a computed declaration: shorthands, custom properties (they inherit), `length`, no writes.
    js(&mut b, "var cs = getComputedStyle(e, '::after');");
    assert_eq!(js(&mut b, "[cs.getPropertyValue('margin'), cs.getPropertyValue('content'), cs.getPropertyValue('--x'), cs.getPropertyValue('font-size'), cs.length === getComputedStyle(e).length, cs instanceof CSSStyleDeclaration].join('|')"), "0px|normal|7|20px|true|true");
    assert_eq!(js(&mut b, "try { cs.color = 'red'; 'no' } catch (err) { err.name }"), "NoModificationAllowedError");
    // Each read is live: no snapshot.
    assert_eq!(js(&mut b, "e.style.color = 'rgb(0, 128, 0)'; e.style.fontSize = '30px'; cs.color + '|' + cs.fontSize"), "rgb(0, 128, 0)|30px");
    // An element that is not rendered, or not connected, still gives a style or an empty declaration, and nothing throws.
    assert_eq!(js(&mut b, "e.style.display = 'none'; getComputedStyle(e, '::before').display"), "inline");
    assert_eq!(js(&mut b, "var d = document.createElement('div'); var c = getComputedStyle(d, '::before'); c.display + '|' + c.length"), "|0");
}

#[test]
fn a_string_that_is_not_a_pseudo_element_gives_an_empty_declaration() {
    let mut b = at(PSEUDO_PAGE);
    js(&mut b, "var e = document.getElementById('e');");
    // CSSOM: a string that starts with a colon and does not parse as a pseudo-element gives an empty read-only declaration.
    for p in ["::nope", ":hover", "::before(", "::", ":", ":marker", "::before::after", "::before x"] {
        let src = format!("var cs = getComputedStyle(e, '{p}'); [cs.display, cs.color, cs.length, cs.getPropertyValue('content')].join('|')");
        assert_eq!(js(&mut b, &src), "||0|", "{p}");
    }
    // An argument that does not start with a colon is ignored, as before.
    assert_eq!(js(&mut b, "getComputedStyle(e, 'before').display"), "block");
}

#[test]
fn writes_are_no_modification_allowed() {
    let mut b = at(PAGE);
    js(&mut b, "var cs = getComputedStyle(document.body); var out = []; function t(f) { try { f(); out.push('no throw'); } catch (e) { out.push(e.name + ':' + e.code + ':' + (e instanceof DOMException)); } }");
    let nm = "NoModificationAllowedError:7:true";
    js(&mut b, "t(function () { cs.setProperty('color', 'red'); }); t(function () { cs.removeProperty('color'); }); t(function () { cs.cssText = 'color: red'; }); t(function () { cs.color = 'red'; }); t(function () { cs.direction = 'rtl'; }); t(function () { cs.margin = '1px'; }); t(function () { cs['background-color'] = 'red'; }); t(function () { cs.cssFloat = 'left'; }); 0");
    assert_eq!(js(&mut b, "out.join()"), [nm; 8].join(","));
    // Arguments are converted first (user code runs), then the write is refused. The state does not change.
    assert_eq!(js(&mut b, "var ran = 0; t(function () { cs.setProperty({ toString() { ran++; return 'color' } }, { toString() { ran++; return 'red' } }); }); out.pop() + ran"), format!("{nm}2"));
    assert_eq!(js(&mut b, "cs.color"), "rgb(0, 0, 0)");
    assert_eq!(js(&mut b, "t(function () { cs.setProperty(); }); out.pop().slice(0, 9)"), "TypeError");
    // `getPropertyPriority` is always empty (no declaration is important).
    assert_eq!(js(&mut b, "cs.getPropertyPriority('color') + '|'"), "|");
}

#[test]
fn used_values_that_layout_does_not_keep_are_gated_and_layout_limits_end_the_read() {
    // A percent padding of a grid item needs the grid area, and an auto margin of an item of a wrapping flex container needs the free space of its line.
    let mut b = at("<body style='margin:0'><div style='display:grid;width:200px'><i id=g style='display:block;padding-left:10%'></i></div><div style='display:flex;flex-wrap:wrap'><i id=f style='display:block;margin-left:auto'></i></div></body>");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('g')).paddingLeft"), "unsupported:a percent padding or margin of a grid item (the grid area is not kept)");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('f')).marginLeft"), "unsupported:an auto margin of a grid item or of an item of a wrapping flex container (the free space of its line is not kept)");
    // A px value needs no layout, so it is never gated.
    let mut b = at("<body style='margin:0'><div style='display:grid;width:200px'><i id=g style='display:block;padding-left:10px;margin-left:5px'></i></div></body>");
    assert_eq!(js(&mut b, "var g = getComputedStyle(document.getElementById('g')); g.paddingLeft + g.marginLeft + g.width"), "10px5px185px");
    // The layout limits of the geometry API stop a read that needs layout, and a read that needs none.
    let mut b = at(&format!("<body><div id=t></div>{}</body>", "<p></p>".repeat(50)));
    b.set_geometry_limits(crate::browser_geometry::GeometryLimits { max_nodes: 20, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('t')).width"), lim("geometry-nodes"));
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('t')).display"), "block");
}

/// What floating-ui reads when a Radix Select opens (`getComputedStyle` of the trigger and its ancestors): no read may stop the page.
#[test]
fn the_reads_of_a_floating_popper_do_not_stop() {
    let mut b = at("<body style='margin:0'><div id=sc style='overflow:auto;height:100px;width:300px;position:relative'><div style='padding:5%;display:flex'><button id=t type=button style='position:relative;margin:auto 0 0 10%;width:50%;border:2px solid;box-sizing:border-box'>x</button></div></div><div id=pop style='position:fixed;left:0;top:0;transform:none'></div></body>");
    let all = "['width','height','marginTop','marginLeft','paddingLeft','paddingTop','top','left','right','bottom','position','overflow','overflowX','overflowY','direction','display','boxSizing','borderTopWidth','borderLeftWidth','transform','translate','perspective','containerType','backdropFilter','filter','willChange','contain'].map(function (k) { return k + '=' + cs[k]; }).join()";
    for id in ["t", "sc", "pop"] {
        js(&mut b, &format!("var cs = getComputedStyle(document.getElementById('{id}')); 0"));
        let r = js(&mut b, all);
        assert!(!r.starts_with("unsupported") && !r.starts_with("limit"), "{id}: {r}");
        assert!(!r.contains("undefined") || r.contains("transform=undefined"), "{id}: {r}");
    }
    js(&mut b, "var cs = getComputedStyle(document.getElementById('t')); 0");
    assert_eq!(js(&mut b, "cs.getPropertyValue('width') + ' ' + cs.getPropertyValue('transform') + '|' + cs.getPropertyValue('will-change') + '|' + cs.position"), "135px ||relative");
}

#[test]
fn supported_names_only_and_gates_for_unavailable_values() {
    let mut b = at(PAGE);
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); 0");
    assert_eq!(js(&mut b, "['direction', 'display', 'position', 'color', 'fontSize', 'marginTop', 'cssFloat', 'webkitBorderHorizontalSpacing', 'opacity', 'zIndex', 'borderTopLeftRadius'].map(function (k) { return k in cs; }).join()"), "true,true,true,true,true,true,true,true,true,true,true");
    // Not supported by the engine: absent, `getPropertyValue` gives "" (CSSOM), no fake value.
    assert_eq!(js(&mut b, "['animation', 'transform', 'WebkitAnimation'].map(function (k) { return k in cs; }).join()"), "false,false,false");
    assert_eq!(js(&mut b, "cs.getPropertyValue('transform') + '|' + cs.getPropertyValue('nope') + '|'"), "||");
    // Computed values that are known without layout.
    assert_eq!(js(&mut b, "cs.getPropertyValue('Display') + cs.position + cs.visibility + cs.fontWeight + cs.fontSize + cs.borderTopWidth + cs.paddingLeft"), "blockstaticvisible40016px0px0px");
    assert_eq!(js(&mut b, "cs.getPropertyValue('DIRECTION')"), "ltr");
    // Used values come from layout: the box size, the `auto` margins and the offsets of a positioned box.
    assert_eq!(js(&mut b, "cs.width + ' ' + cs.height + ' ' + cs.getPropertyValue('margin-top')"), "1264px 22px 0px");
    // `top` of a static element is its computed value `auto`; a relative one with a px offset is that offset.
    assert_eq!(js(&mut b, "cs.top"), "auto");
    assert_eq!(js(&mut b, "document.getElementById('a').style.position = 'relative'; cs.top"), "0px");
    assert_eq!(js(&mut b, "document.getElementById('a').style.bottom = '5px'; cs.top"), "-5px");
    // A display:none element: the resolved value is the computed one.
    assert_eq!(js(&mut b, "var h = getComputedStyle(document.head); h.display + h.width + h.marginTop"), "noneauto0px");
    // The engine reads these only loosely: no text is made up.
    assert_eq!(js(&mut b, "cs.alignItems + '|' + cs.gridTemplateColumns + '|' + cs.textDecoration"), "normal|none|none");
    // Shorthands are built from longhands with the existing serializer.
    assert_eq!(js(&mut b, "document.getElementById('a').style.position = ''; cs.getPropertyValue('padding') + '|' + cs.getPropertyValue('border-width')"), "0px|0px");
    // Custom properties: the computed text, inherited.
    assert_eq!(js(&mut b, "document.body.style.setProperty('--gap', ' 8px '); cs.getPropertyValue('--gap') + '|' + cs.getPropertyValue('--none') + '|'"), "8px||");
    // `length` and `item` list sorted supported longhands, then custom properties.
    assert_eq!(js(&mut b, "cs.length > 50 && cs.item(0) === '-webkit-border-horizontal-spacing' && cs.item(cs.length) === '' && cs.item(cs.length - 1)"), "--gap");
    assert_eq!(js(&mut b, "cs.cssText"), "unsupported:computed cssText (CSSOM and browsers differ; not faked)");
    assert_eq!(js(&mut b, "cs.parentRule"), "null");
}

#[test]
fn elements_without_a_style_say_so() {
    let mut b = at("<!doctype html><html><head><title id=t>x</title></head><body><template id=tp><p id=q></p></template><div id=a></div></body></html>");
    // Detached: an empty declaration (CSSOM): no values and no length.
    assert_eq!(js(&mut b, "var d = document.createElement('div'); var cs = getComputedStyle(d); cs.display + '|' + cs.direction + '|' + cs.length + '|' + cs.item(0) + '|' + cs.getPropertyValue('--x')"), "||0||");
    // Attached later: the same object now has values.
    assert_eq!(js(&mut b, "document.body.appendChild(d); cs.display + cs.length"), format!("block{}", bare_css::values::Property::COUNT));
    // Inside `display:none`: the computed values, as in Chrome.
    assert_eq!(js(&mut b, "getComputedStyle(document.head).display"), "none");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('t')).display"), "none");
    assert_eq!(js(&mut b, "var e = document.createElement('i'); document.head.appendChild(e); getComputedStyle(e).display"), "inline");
    // Inside a `<template>`: Chrome keeps the content in another document, so the declaration is empty (it was recorded with Chrome 155).
    assert_eq!(js(&mut b, "var q = document.getElementById('tp').content.firstChild; var t = getComputedStyle(q); t.display + '|' + t.length + '|' + t.position"), "|0|");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('tp')).display"), "none");
}

const HIDDEN: &str = "<!doctype html><html id=r><head><style>
  :root { --r: 7px; font-size: 20px }
  #h1 { display: none; color: rgb(1, 2, 3); font-size: 10px; --x: hello; position: relative; width: 50%; --y: 3px }
  #c1 { position: sticky; width: 40%; height: 2em; margin-top: 3px; top: 5px; padding-left: 10%; font-size: 2em; padding-top: var(--y) }
  #g1 { color: inherit; --z: var(--x) there; width: var(--r); margin-left: 1rem }
  #c2 { display: none; position: absolute } #h1 > span > b { font-weight: 700 } #c1 + #c2 { z-index: 4 } body #h1 span:first-child { line-height: 9px }
  #n1 { display: none; color: rgb(9, 9, 9) } #n2 { position: fixed }
  #dc { display: contents; color: rgb(4, 5, 6) } #dcc { width: 30px }
  #hid { color: rgb(7, 8, 9) } #hidc { position: absolute }
  #v { width: 120px; height: 20px; margin-top: 4px; position: relative; top: 2px }
</style></head><body>
<div id=v>v</div>
<div id=h1><span id=c1>c1<b id=g1>g1</b></span><span id=c2>c2</span></div>
<div id=h2 style='display:none'><div id=n1><div id=n2>n</div></div></div>
<div id=dc><p id=dcc>x</p></div>
<div id=hid hidden><p id=hidc>x</p></div>
</body></html>";

/// Values recorded with Chrome 155 (`tools/ui/chrome_hidden_style.mjs`, `tools/fixtures/ui/chrome-hidden-styles.txt`).
#[test]
fn elements_inside_display_none_have_computed_values() {
    let mut b = at(HIDDEN);
    js(&mut b, "var g = function (id, p) { return getComputedStyle(document.getElementById(id)).getPropertyValue(p); };");
    // The stop of the staging login: Next.js `shouldSkipElement` reads `position` of an element that is not rendered.
    let next = "function shouldSkipElement(e) { return ['sticky', 'fixed'].includes(getComputedStyle(e).position); }";
    js(&mut b, next);
    assert_eq!(js(&mut b, "shouldSkipElement(document.getElementById('c1')) + ',' + shouldSkipElement(document.getElementById('g1'))"), "true,false");
    // Chrome: position, display, inherited color, `em`, `rem`, percent and `auto` stay computed values.
    assert_eq!(js(&mut b, "g('c1', 'position') + ' ' + g('c1', 'display') + ' ' + g('c1', 'color') + ' ' + g('c1', 'width') + ' ' + g('c1', 'height')"), "sticky inline rgb(1, 2, 3) 40% 40px");
    assert_eq!(js(&mut b, "g('c1', 'margin-top') + ' ' + g('c1', 'top') + ' ' + g('c1', 'padding-left') + ' ' + g('c1', 'padding-top') + ' ' + g('c1', 'font-size')"), "3px 5px 10% 3px 20px");
    assert_eq!(js(&mut b, "g('g1', 'position') + ' ' + g('g1', 'color') + ' ' + g('g1', 'width') + ' ' + g('g1', 'margin-left') + ' ' + g('g1', 'font-size')"), "static rgb(1, 2, 3) 7px 20px 20px");
    // Custom properties and `var()` inherit through the hidden subtree.
    assert_eq!(js(&mut b, "g('g1', '--x') + '|' + g('g1', '--z') + '|' + g('h1', '--y')"), "hello|hello there|3px");
    // Selectors with ancestors and siblings match inside the hidden subtree.
    assert_eq!(js(&mut b, "g('g1', 'font-weight') + ' ' + g('c2', 'z-index') + ' ' + g('c1', 'line-height') + ' ' + g('c2', 'line-height')"), "700 4 9px normal");
    // Nested `display:none`, and a descendant of it.
    assert_eq!(js(&mut b, "g('c2', 'display') + ' ' + g('c2', 'position') + ' ' + g('c2', 'width')"), "none absolute auto");
    assert_eq!(js(&mut b, "g('n1', 'display') + ' ' + g('n2', 'display') + ' ' + g('n2', 'color') + ' ' + g('n2', 'position') + ' ' + g('n2', 'width')"), "none block rgb(9, 9, 9) fixed auto");
    // `display:contents` is not hidden (the engine has no `contents` value: Chrome says `contents` for `#dc`, here it reads as the `div` default).
    assert_eq!(js(&mut b, "g('dcc', 'display') + ' ' + g('dcc', 'color') + ' ' + g('dcc', 'position')"), "block rgb(4, 5, 6) static");
    // The `hidden` attribute.
    assert_eq!(js(&mut b, "g('hid', 'display') + ' ' + g('hidc', 'display') + ' ' + g('hidc', 'color') + ' ' + g('hidc', 'position')"), "none block rgb(7, 8, 9) absolute");
    // Deep inside `display:none` (40 levels): the style comes down the whole way.
    js(&mut b, "var dp = document.createElement('div'); dp.id = 'dp'; dp.style.cssText = 'display:none;color:rgb(5,6,7)'; dp.innerHTML = '<div>'.repeat(40) + '<div id=dpe style=\\'position:fixed;width:10%\\'>e</div>' + '</div>'.repeat(40); document.body.appendChild(dp);");
    assert_eq!(js(&mut b, "g('dp', 'display') + ' ' + g('dpe', 'position') + ' ' + g('dpe', 'color') + ' ' + g('dpe', 'display') + ' ' + g('dpe', 'width')"), "none fixed rgb(5, 6, 7) block 10%");
    // A rendered element still needs layout for its used values.
    assert_eq!(js(&mut b, "g('v', 'position') + ' ' + g('v', 'top') + ' ' + g('v', 'width') + ' ' + g('v', 'margin-top')"), "relative 2px 120px 4px");
    // The object is live: the next read sees a change inside the hidden subtree and the end of `display:none`.
    assert_eq!(js(&mut b, "var cs = getComputedStyle(document.getElementById('c1')); document.getElementById('c1').style.position = 'fixed'; cs.position"), "fixed");
    assert_eq!(js(&mut b, "document.getElementById('h1').style.color = 'rgb(9, 8, 7)'; getComputedStyle(document.getElementById('g1')).color"), "rgb(9, 8, 7)");
    assert_eq!(js(&mut b, "document.getElementById('h1').style.display = 'block'; getComputedStyle(document.getElementById('c1')).width"), "512px", "c1 is `position: fixed` here, so a box: 40% of the 1280px window");
    assert_eq!(js(&mut b, "document.getElementById('h1').style.display = ''; g('c1', 'width')"), "40%");
}

#[test]
fn gc_detach_and_slot_reuse() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var keep = (function () { var d = document.createElement('div'); d.className = 'r'; return getComputedStyle(d); })(); 0");
        b.collect(&[]).unwrap();
        // The declaration keeps its detached element alive, and the element keeps its state.
        assert_eq!(js(&mut b, "keep.length"), "0");
        assert_eq!(js(&mut b, "keep.direction"), "");
        assert_eq!(b.computed_stats().objects, 1);
        js(&mut b, "keep = null; 0");
        b.collect(&[]).unwrap();
        assert_eq!(b.computed_stats().objects, 0);
        // Many short-lived objects are pruned; new objects (and other host objects) do not inherit a brand from a dead slot.
        js(&mut b, "for (var i = 0; i < 50; i++) getComputedStyle(document.body); 0");
        b.collect(&[]).unwrap();
        assert_eq!(b.computed_stats().objects, 0);
        js(&mut b, "var fresh = []; for (var i = 0; i < 50; i++) fresh.push(document.createElement('div').style, {}, new Event('x')); 0");
        assert_eq!(js(&mut b, "var P = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(getComputedStyle(document.body)), 'display').get; fresh.map(function (o) { try { P.call(o); return 'ok'; } catch (e) { return e.name; } }).filter(function (n) { return n !== 'TypeError'; }).length"), "0");
        // A live one is kept by JS (WeakMap key) and its element is kept too.
        js(&mut b, "var wm = new WeakMap(); var k = getComputedStyle(document.getElementById('a')); wm.set(k, 7); (function () { wm.set(getComputedStyle(document.body), 1); })(); 0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "wm.get(k) + k.display"), "7block");
        assert_eq!(b.computed_stats().objects, 1);
    });
}

#[test]
fn computed_objects_are_not_same_object_and_bound() {
    let mut b = at(PAGE);
    assert_eq!(js(&mut b, "getComputedStyle(document.body) === getComputedStyle(document.body)"), "false");
    b.collect(&[]).unwrap();
    b.set_computed_limits(ComputedLimits { max_objects: 3, ..Default::default() });
    assert_eq!(js(&mut b, "var a = [getComputedStyle(document.body), getComputedStyle(document.body), getComputedStyle(document.body)]; 0"), "0");
    assert_eq!(js(&mut b, "getComputedStyle(document.body)"), lim("computed-objects"));
    // No JS runs after a limit: the limit is the result of the call.
    assert_eq!(js(&mut b, "1"), "1");
}

use crate::browser_computed::{SheetPart, StyleAvailability};

fn part(text: &str) -> SheetPart {
    SheetPart { text: text.into(), layer: None }
}

fn missing(b: &Browser) -> Vec<String> {
    match b.style_availability().unwrap() {
        StyleAvailability::Ready => vec![],
        StyleAvailability::MissingExternal { hrefs, .. } => hrefs,
    }
}

const EXT: &str = "<!doctype html><html><head><link rel=stylesheet href='a.css'><style>#a { color: green }</style><link rel=stylesheet href='b.css'></head><body><div id=a></div></body></html>";

#[test]
fn external_sheets_are_ordered_and_missing_ones_are_gated() {
    let mut b = at(EXT);
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); 0");
    let gate = "unsupported:computed style needs an external stylesheet that the embedder has not supplied (Browser::style_availability lists them)";
    assert_eq!(missing(&b), ["a.css", "b.css"]);
    assert_eq!(js(&mut b, "cs.color"), gate);
    assert_eq!(js(&mut b, "cs.direction"), gate);
    assert_eq!(b.computed_stats().builds, 0);
    b.set_external_stylesheet("a.css", vec![part("#a { color: red; display: flex }")]).unwrap();
    assert_eq!(missing(&b), ["b.css"]);
    assert_eq!(js(&mut b, "cs.color"), gate);
    b.set_external_stylesheet("b.css", vec![part("#a { color: blue }")]).unwrap();
    assert_eq!(b.style_availability().unwrap(), StyleAvailability::Ready);
    // Order: a, inline, b. The later one wins the tie.
    assert_eq!(js(&mut b, "cs.color + '|' + cs.display"), "rgb(0, 0, 255)|flex");
    // Move b before the inline sheet: now the inline one is later.
    assert_eq!(js(&mut b, "var h = document.head; h.insertBefore(h.children[2], h.children[1]); cs.color"), "rgb(0, 128, 0)");
    // The text of a supplied sheet changes: the held object follows.
    b.set_external_stylesheet("a.css", vec![part("#a { color: red; display: inline }")]).unwrap();
    assert_eq!(js(&mut b, "cs.display"), "inline");
    // A layered part loses to an unlayered rule, whatever the order.
    b.set_external_stylesheet("b.css", vec![SheetPart { text: "#a { color: blue }".into(), layer: Some("x".into()) }]).unwrap();
    assert_eq!(js(&mut b, "h.appendChild(h.children[1]); cs.color"), "rgb(0, 128, 0)");
    // Removing a supplied sheet makes the link missing again, never "no rules".
    assert!(b.remove_external_stylesheet("a.css"));
    assert!(!b.remove_external_stylesheet("a.css"));
    assert_eq!(missing(&b), ["a.css"]);
    assert_eq!(js(&mut b, "cs.color"), gate);
    // A failed sheet is supplied as empty by the embedder's choice.
    b.set_external_stylesheet("a.css", vec![]).unwrap();
    assert_eq!(js(&mut b, "cs.display"), "block");
}

/// Without `set_pending_sheets` a missing sheet is a gate (above). With it, a sheet that is not supplied is still loading: it adds no
/// rules, `style_availability` still lists it, and a link keeps the sheet that it had.
#[test]
fn a_pending_sheet_adds_no_rules_when_the_driver_loads_it_later() {
    let mut b = at(EXT);
    b.set_external_stylesheet("a.css", vec![part("#a { display: flex }")]).unwrap();
    b.set_external_stylesheet("b.css", vec![part("#a { color: blue }")]).unwrap();
    assert_eq!(b.style_availability().unwrap(), StyleAvailability::Ready);
    b.set_pending_sheets(true);
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'new.css'); document.head.appendChild(l); 0");
    assert_eq!(missing(&b), ["new.css"]);
    assert_eq!(js(&mut b, "cs.color + '|' + cs.display"), "rgb(0, 0, 255)|flex");
    // The new link follows the supplied sheet at once.
    b.set_external_stylesheet("new.css", vec![part("#a { color: red }")]).unwrap();
    assert_eq!(js(&mut b, "cs.color"), "rgb(255, 0, 0)");
    // An old link keeps its sheet while its new `href` loads. `style_availability` is the driver's pass that binds the links.
    assert_eq!(b.style_availability().unwrap(), StyleAvailability::Ready);
    js(&mut b, "document.head.children[0].setAttribute('href', 'c.css'); 0");
    assert_eq!(missing(&b), ["c.css"]);
    assert_eq!(js(&mut b, "cs.display"), "flex");
    // If the sheet that the link had is gone, the read is a gate again.
    assert!(b.remove_external_stylesheet("a.css"));
    assert!(js(&mut b, "cs.display").starts_with("unsupported:computed style needs an external stylesheet"));
    // A second driver with `false` is strict again.
    b.set_pending_sheets(false);
    js(&mut b, "document.head.children[0].setAttribute('href', 'b.css'); 0");
    assert_eq!(js(&mut b, "cs.display"), "block");
}

#[test]
fn only_real_sources_are_required() {
    let mut b = at("<!doctype html><html><head><link rel=stylesheet href='m.css' media='print'><link rel='alternate stylesheet' href='alt.css'><link rel=stylesheet><link rel=icon href=i.png><link rel=stylesheet href='  '></head><body><div id=a></div></body></html>");
    assert_eq!(missing(&b), Vec::<String>::new());
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), "block");
    // `media` matches the viewport: a narrow window needs it.
    assert_eq!(js(&mut b, "document.querySelector('[media]').setAttribute('media', 'screen'); 0"), "0");
    assert_eq!(missing(&b), ["m.css"]);
    assert_eq!(js(&mut b, "document.querySelector('[media]').setAttribute('media', '(max-width: 100px)'); getComputedStyle(document.body).display"), "block");
    // `href` and `rel` changes are seen.
    js(&mut b, "var l = document.querySelector('link[rel=icon]'); l.setAttribute('rel', 'stylesheet'); 0");
    assert_eq!(missing(&b), ["i.png"]);
    js(&mut b, "l.setAttribute('href', 'j.css'); 0");
    assert_eq!(missing(&b), ["j.css"]);
    assert_eq!(js(&mut b, "l.parentNode.removeChild(l); getComputedStyle(document.body).display"), "block");
}

#[test]
fn inline_import_is_a_required_external_sheet_before_its_own_text() {
    let mut b = at("<!doctype html><html><head><style>@import url(imp.css); #a { color: green }</style></head><body><div id=a></div></body></html>");
    assert_eq!(missing(&b), ["imp.css"]);
    b.set_external_stylesheet("imp.css", vec![part("#a { color: red; display: flex }")]).unwrap();
    assert_eq!(js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); cs.color + '|' + cs.display"), "rgb(0, 128, 0)|flex");
}

#[test]
fn rule_set_is_reused_until_an_input_changes() {
    let mut b = at(EXT);
    b.set_external_stylesheet("a.css", vec![part("#a { color: red }")]).unwrap();
    b.set_external_stylesheet("b.css", vec![]).unwrap();
    js(&mut b, "var cs = getComputedStyle(document.getElementById('a')); cs.color; cs.display; cs.direction; 0");
    // The document does not change between the reads: one cascade.
    assert_eq!((b.computed_stats().builds, b.computed_stats().computes), (1, 1));
    // Class and attribute changes do not rebuild the rules.
    js(&mut b, "document.getElementById('a').className = 'q'; cs.color; 0");
    assert_eq!(b.computed_stats().builds, 1);
    js(&mut b, "document.querySelector('style').textContent = '#a { color: blue }'; cs.color; 0");
    assert_eq!(b.computed_stats().builds, 2);
    b.set_viewport(300.0, 300.0).unwrap();
    js(&mut b, "cs.color; 0");
    assert_eq!(b.computed_stats().builds, 3);
}

/// The text that the native cascade gives, written without the code under test.
fn native(html: &str, ext: &[&str], id: &str, p: bare_css::values::Property) -> String {
    use bare_css::cascade::{Origin, RuleSet, UA_CSS, sources, Source};
    use bare_css::values::{Length, Value as V};
    let mut doc = tree::parse(html);
    let mut rules = RuleSet::new((1280.0, 800.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    let mut ext = ext.iter();
    for s in sources(&doc, (1280.0, 800.0)) {
        match s {
            Source::Inline(t) => rules.add(&mut doc, &t, Origin::Author),
            Source::Link(_) => rules.add(&mut doc, ext.next().unwrap(), Origin::Author),
        }
    }
    let styles = rules.compute(&doc);
    let el = doc.children(doc.root()).flat_map(|c| std::iter::once(c).chain(doc.children(c))).chain(doc.children(doc.root()).flat_map(|c| doc.children(c).flat_map(|g| doc.children(g)))).find(|&n| doc.attr(n, bare_html::dom::Atom::ID) == Some(id));
    let el = el.or_else(|| {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attr(n, bare_html::dom::Atom::ID) == Some(id) { return Some(n); }
            stack.extend(doc.children(n));
        }
        None
    }).unwrap();
    match styles.style(el).unwrap().get(p) {
        V::Keyword(k) => k.name().to_string(),
        V::Number(n) => format!("{n}"),
        V::Length(Length::Px(x)) => format!("{x}px"),
        V::Color(c) if c.a == 255 => format!("rgb({}, {}, {})", c.r, c.g, c.b),
        V::Color(c) => format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, (c.a as f32 / 255.0 * 1000.0).round() / 1000.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn values_agree_with_the_native_cascade() {
    use bare_css::values::Property::*;
    let html = "<!doctype html><html dir=rtl><head><style>p { color: #336699; font-size: 20px; font-weight: bold } .k { display: inline-block; background-color: rgba(1, 2, 3, .5); position: absolute } div { border: 2px solid red; padding: 3px 4px } #q { direction: ltr }</style><link rel=stylesheet href=e.css></head><body><div id=d><p id=p class=k>x</p><b id=q>y</b></div><table id=t><tr><td id=c>z</td></tr></table></body></html>";
    let e = "p { color: orange } #p { visibility: hidden; font-size: 2em } td { text-align: right; padding: 0 }";
    let mut b = at(html);
    b.set_external_stylesheet("e.css", vec![part(e)]).unwrap();
    let cases: &[(&str, bare_css::values::Property, &str)] = &[
        ("d", Display, "display"), ("p", Display, "display"), ("p", Position, "position"), ("p", TextColor, "color"),
        ("p", BackgroundColor, "background-color"), ("p", FontSize, "font-size"), ("p", FontWeight, "font-weight"), ("p", Visibility, "visibility"),
        ("d", BorderTopWidth, "border-top-width"), ("d", BorderLeftStyle, "border-left-style"), ("d", BorderRightColor, "border-right-color"),
        ("d", PaddingLeft, "padding-left"), ("d", PaddingTop, "padding-top"), ("html_", Direction, "direction"), ("q", Direction, "direction"),
        ("p", Direction, "direction"), ("c", TextAlign, "text-align"), ("c", PaddingLeft, "padding-left"), ("c", Display, "display"), ("t", BorderSpacingH, "-webkit-border-horizontal-spacing"),
        ("q", FontWeight, "font-weight"), ("q", TextColor, "color"),
    ];
    for &(id, p, name) in cases {
        let id = if id == "html_" { "d" } else { id };
        let want = native(html, &[e], id, p);
        let got = js(&mut b, &format!("getComputedStyle(document.getElementById('{id}')).getPropertyValue('{name}')"));
        assert_eq!(got, want, "{id} {name}");
    }
    // The named getter and the dashed form are the same read.
    assert_eq!(js(&mut b, "var c = getComputedStyle(document.getElementById('p')); c.fontSize + c['font-size'] + c.getPropertyValue('font-size')"), "32px32px32px");
}

#[test]
fn caps_are_uncatchable_and_checked_before_the_expensive_work() {
    // computed-nodes
    let mut b = at(PAGE);
    b.set_computed_limits(ComputedLimits { max_nodes: 5, ..Default::default() });
    assert_eq!(js(&mut b, "var ran = 0; try { getComputedStyle(document.body).display; ran = 1; } catch (e) { ran = 2; } ran"), lim("computed-nodes"));
    assert_eq!(js(&mut b, "ran"), "0");
    assert_eq!(b.computed_stats().builds, 0);
    assert_eq!(b.style_availability().unwrap(), crate::browser_computed::StyleAvailability::Ready);
    // Excess page sources are dropped before parsing. Supplied sheets retain their caps.
    let mut b = at("<style>a{}</style><style>b{}</style><div id=a></div>");
    b.set_computed_limits(ComputedLimits { max_sources: 1, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), "block");
    let mut b = at(&format!("<style>{}</style><div id=a></div>", "a{color:red}".repeat(1000)));
    b.set_computed_limits(ComputedLimits { max_sheet_bytes: 1000, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), "block");
    assert_eq!(b.computed_stats().builds, 1);
    // computed-rules: checked after the build (bounded by the bytes), before the cascade runs
    let mut b = at("<style>a{} b{} c{color:red} d{color:red} e{color:red}</style><div id=a></div>");
    b.set_computed_limits(ComputedLimits { max_rules: 3, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), lim("computed-rules"));
    assert_eq!(b.computed_stats().computes, 0);
    // computed-work: nodes x rules, before the cascade runs
    let mut b = at(PAGE);
    b.set_computed_limits(ComputedLimits { max_work: 100, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.body).display"), lim("computed-work"));
    assert_eq!(b.computed_stats().computes, 0);
    // The step budget is charged before the cascade runs.
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 30, ..Default::default() }, HeapLimits::default()).unwrap();
    assert_eq!(js(&mut b, "var ran = 0; try { getComputedStyle(document.body).display; ran = 1; } catch (e) { ran = 2; } ran"), "limit:Steps");
    assert_eq!(b.computed_stats().computes, 0);
    // external sheets: count, bytes, and no change on an error
    let mut b = at(PAGE);
    b.set_computed_limits(ComputedLimits { max_external: 2, max_external_bytes: 30, max_sheet_bytes: 30, ..Default::default() });
    b.set_external_stylesheet("a", vec![part("x{}")]).unwrap();
    b.set_external_stylesheet("b", vec![part("y{}")]).unwrap();
    assert_eq!(ext_err(b.set_external_stylesheet("c", vec![])), lim("computed-external"));
    b.set_external_stylesheet("a", vec![part("x{} x{} x{}")]).unwrap();
    assert_eq!(ext_err(b.set_external_stylesheet("b", vec![part(&"y{}".repeat(20))])), lim("computed-sources"));
    assert_eq!(ext_err(b.set_external_stylesheet("b", vec![part("y{} y{} y{} y{} y{} y{}")])), lim("computed-external-bytes"));
    let s = b.computed_stats();
    assert_eq!((s.external_sheets, s.external_bytes), (2, 1 + 11 + 1 + 3));
    assert!(b.remove_external_stylesheet("a"));
    assert_eq!(b.computed_stats().external_bytes, 4);
    let mut b = at(PAGE);
    b.set_computed_limits(ComputedLimits { max_sources: 1, ..Default::default() });
    assert_eq!(ext_err(b.set_external_stylesheet("a", vec![part("x{}"), part("y{}")])), lim("computed-sources"));
}

fn ext_err(r: Result<(), Error>) -> String {
    match r {
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        other => format!("{other:?}"),
    }
}

/// Load model for the capped runs (`SCALE=1|10|100`, `--ignored`, see the report). 200 x SCALE elements, 20 x SCALE rules, 40 reads
/// that each rebuild the style of the whole document, held objects that follow a class change and a `<style>` change.
#[test]
#[ignore]
fn load_model() {
    let scale: usize = std::env::var("SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || {
        let mut css = String::new();
        for i in 0..20 * scale {
            css.push_str(&format!(".c{i} {{ color: #{:06x}; direction: {} }} div.c{i} > p {{ font-size: {}px }}\n", i * 977 % 0xffffff, if i % 2 == 0 { "rtl" } else { "ltr" }, 10 + i % 20));
        }
        let mut body = String::new();
        for i in 0..200 * scale {
            body.push_str(&format!("<div id=d{i} class='c{} c{}'><p id=p{i}>x</p></div>", i % (20 * scale), (i * 7) % (20 * scale)));
        }
        let page = format!("<!doctype html><html><head><style>{css}</style></head><body>{body}</body></html>");
        let started = std::time::Instant::now();
        let mut b = Browser::new(tree::parse(&page), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let held = if scale >= 100 { 4 } else { 40 };
        let read = format!("var held = [], n = 0; for (var i = 0; i < {held}; i++) {{ var e = document.getElementById('p' + (i * 5)); held.push(getComputedStyle(e)); n += held[i].direction.length + held[i].fontSize.length; }} n");
        // The default `computed-work` bound (nodes x rules) refuses the largest model before the cascade runs, by design.
        if scale >= 100 {
            assert_eq!(js(&mut b, &read), lim("computed-work"));
            if std::env::var("REFUSE_ONLY").is_ok() {
                assert_eq!(b.computed_stats().computes, 0);
                println!("scale {scale}: refused before the cascade, {:?}", started.elapsed());
                return;
            }
            b.set_computed_limits(ComputedLimits { max_work: 1 << 36, ..Default::default() });
            js(&mut b, "held = []; 0");
        }
        assert_eq!(js(&mut b, &read), (7 * held).to_string());
        assert_eq!(js(&mut b, "document.getElementById('d0').className = 'c1'; held[0].direction"), "ltr");
        assert_eq!(js(&mut b, "document.querySelector('style').textContent = '#d0 { direction: rtl }'; held[0].direction"), "rtl");
        let s = b.computed_stats();
        println!("scale {scale}: nodes {}, builds {}, computes {}, {:?}", b.stats().nodes, s.builds, s.computes, started.elapsed());
        b.collect(&[]).unwrap();
        assert_eq!(b.computed_stats().objects, held);
    }).unwrap().join().unwrap();
}

#[test]
fn cascade_work_is_reserved_before_the_cascade_runs() {
    big(|| {
        let sheet = (0..100).map(|i| format!(".c{i}{{color:red}}")).collect::<String>();
        let page = format!("<style>{sheet}</style><body>{}<p id=q>x</p>", "<div></div>".repeat(100));
        let mut b = Browser::new(tree::parse(&page), BrowserLimits::default(), EvalLimits {max_steps: 5000, ..Default::default()}, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('q')).color"), "limit:Steps");
        assert_eq!(b.computed_stats().computes, 0);
    });
}

#[test]
fn computed_names_are_sorted_and_include_live_custom_properties() {
    let mut b = at("<body><p id=p style='--z: red; --a: blue'>x</p>");
    let count = bare_css::values::Property::COUNT;
    assert_eq!(js(&mut b, &format!("var cs=getComputedStyle(document.getElementById('p')); var names=[]; for(var i=0;i<{count};i++) names.push(cs.item(i)); [names.join()===names.slice().sort().join(),cs.length==={count}+2,cs.item({count}).slice(0,2)==='--'].join()")), "true,true,true");
}

#[test]
fn reads_of_a_hidden_element_keep_both_cascades() {
    let mut b = at("<!doctype html><html><head><style>.h { display: none }</style></head><body><div class=h><p id=p>t</p></div><p id=q>u</p></body></html>");
    // The value, the full cascades and the partial ones (`ComputedStats::restyles`).
    let n = |b: &mut Browser, s: &str| (js(b, s), b.computed_stats().computes, b.computed_stats().restyles);
    // The first read of a hidden element styles the page, then only the way down to it.
    assert_eq!(n(&mut b, "var p = document.getElementById('p'), q = document.getElementById('q'); getComputedStyle(p).display"), ("block".into(), 1, 1));
    // With no change, more reads of it and of a shown element use the cascade again. Main did two more cascades for each read of `p`.
    assert_eq!(n(&mut b, "getComputedStyle(p).display + getComputedStyle(q).display + getComputedStyle(p).color"), ("blockblockrgb(0, 0, 0)".into(), 1, 3));
    // A class change restyles `q` only.
    assert_eq!(n(&mut b, "q.className = 'h'; getComputedStyle(q).display + getComputedStyle(p).display"), ("noneblock".into(), 1, 5));
}

#[test]
fn a_data_state_toggle_needs_no_full_cascade() {
    // A form pseudo-class in the sheet, as in Tailwind (`disabled:`). Before, any attribute change gave a full cascade then.
    let mut b = at("<!doctype html><html><head><style>button:disabled { opacity: .5 } [data-state=open] { color: red }</style></head>\
        <body><form><button id=t data-state=closed>x</button></form></body></html>");
    let n = |b: &mut Browser, s: &str| (js(b, s), b.computed_stats().computes, b.computed_stats().restyles);
    assert_eq!(n(&mut b, "var t = document.getElementById('t'); getComputedStyle(t).color"), ("rgb(0, 0, 0)".into(), 1, 0));
    assert_eq!(n(&mut b, "t.dataset.state = 'open'; getComputedStyle(t).color"), ("rgb(255, 0, 0)".into(), 1, 1));
    assert_eq!(n(&mut b, "t.disabled = true; getComputedStyle(t).opacity"), ("0.5".into(), 1, 2));
    // The type of a button can change the default button of its form, another element: a full cascade.
    assert_eq!(n(&mut b, "t.type = 'reset'; getComputedStyle(t).color"), ("rgb(255, 0, 0)".into(), 2, 2));
}

#[test]
fn styles_are_reused_until_the_document_changes() {
    let mut b = at("<!doctype html><html><head><style>.w { width: 50px } p:empty { display: none } x-a:defined { color: green } input:focus { color: red }</style></head>\
        <body><div id=a style='width: 10px'>t</div><p id=p></p><x-a id=x></x-a><input id=i></body></html>");
    // The value, the full cascades and the partial ones (`ComputedStats::restyles`).
    let n = |b: &mut Browser, s: &str| (js(b, s), b.computed_stats().computes, b.computed_stats().restyles);
    js(&mut b, "var a = document.getElementById('a'), p = document.getElementById('p'), x = document.getElementById('x'); 0");
    assert_eq!(n(&mut b, "a.getBoundingClientRect().width + a.getBoundingClientRect().width + getComputedStyle(a).width"), ("2010px".into(), 1, 0));
    // An attribute, a tree change and a text edit each restyle the elements that they reach.
    assert_eq!(n(&mut b, "a.className = 'w'; a.removeAttribute('style'); a.getBoundingClientRect().width"), ("50".into(), 1, 1));
    assert_eq!(n(&mut b, "p.appendChild(document.createTextNode('')); getComputedStyle(p).display"), ("none".into(), 1, 2));
    assert_eq!(n(&mut b, "p.firstChild.data = 'q'; getComputedStyle(p).display"), ("block".into(), 1, 3));
    assert_eq!(n(&mut b, "getComputedStyle(p).display"), ("block".into(), 1, 3));
    // The focus moves no tree counter.
    assert_eq!(n(&mut b, "var i = document.getElementById('i'); getComputedStyle(i).color"), ("rgb(0, 0, 0)".into(), 1, 3));
    assert_eq!(n(&mut b, "i.focus(); getComputedStyle(i).color"), ("rgb(255, 0, 0)".into(), 1, 4));
    // A custom element that is defined matches `:defined`, and no tree counter moves.
    assert_eq!(n(&mut b, "getComputedStyle(x).color"), ("rgb(0, 0, 0)".into(), 1, 4));
    assert_eq!(n(&mut b, "customElements.define('x-a', class extends HTMLElement {}); getComputedStyle(x).color"), ("rgb(0, 128, 0)".into(), 2, 4));
}

#[test]
fn excess_shadow_sources_do_not_abort_page() {
    let mut b = at("<div id=a></div><div id=b></div>");
    b.set_computed_limits(ComputedLimits { max_sources: 1, ..Default::default() });
    js(&mut b, "document.getElementById('a').attachShadow({mode:'open'}).innerHTML='<style>:host{color:red}</style>'; document.getElementById('b').attachShadow({mode:'open'}).innerHTML='<style>:host{color:blue}</style>'");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).color"), "rgb(255, 0, 0)");
}

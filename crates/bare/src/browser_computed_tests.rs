//! Tests of `browser_computed`: `window.getComputedStyle` over the native cascade. Expectations come from CSSOM, CSS
//! Cascade and the HTML rendering section, read by hand. Node has no DOM, so it is not an oracle here.

use crate::browser::*;
use crate::browser_computed::ComputedLimits;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

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

#[test]
fn pseudo_elements_are_gated() {
    let mut b = at(PAGE);
    let gate = "unsupported:getComputedStyle of a pseudo-element (the engine has no pseudo-element styles)";
    assert_eq!(js(&mut b, "getComputedStyle(document.body, '::before')"), gate);
    assert_eq!(js(&mut b, "getComputedStyle(document.body, ':after')"), gate);
    assert_eq!(js(&mut b, "getComputedStyle(document.body, '::nope')"), gate);
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
    // Used values (layout) are gated, not guessed: `auto` width, margin, positioned offsets.
    let used = "unsupported:the resolved value is the used value, which needs layout";
    assert_eq!(js(&mut b, "cs.width"), used);
    assert_eq!(js(&mut b, "cs.getPropertyValue('margin-top')"), used);
    assert_eq!(js(&mut b, "cs.height"), used);
    // `top` of a static element is its computed value `auto`; a positioned one needs layout.
    assert_eq!(js(&mut b, "cs.top"), "auto");
    assert_eq!(js(&mut b, "document.getElementById('a').style.position = 'relative'; cs.top"), used);
    // A display:none element: the resolved value is the computed one.
    assert_eq!(js(&mut b, "var h = getComputedStyle(document.head); h.display + h.width + h.marginTop"), "noneauto0px");
    // The engine reads these only loosely: no text is made up.
    assert_eq!(js(&mut b, "cs.alignItems"), "unsupported:this property is read only approximately by the engine (no faithful computed text)");
    assert_eq!(js(&mut b, "cs.gridTemplateColumns"), "unsupported:this property is read only approximately by the engine (no faithful computed text)");
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
    // Inside `display:none`, the engine has no style for it: gate.
    assert_eq!(js(&mut b, "getComputedStyle(document.head).display"), "none");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('t')).display"), "unsupported:an element inside display:none has no computed style in the engine");
    assert_eq!(js(&mut b, "var e = document.createElement('i'); document.head.appendChild(e); getComputedStyle(e).display"), "unsupported:an element inside display:none has no computed style in the engine");
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
    assert_eq!((b.computed_stats().builds, b.computed_stats().computes), (1, 3));
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
    // computed-sources: sheet count, and bytes (before anything is parsed or built)
    let mut b = at("<style>a{}</style><style>b{}</style><div id=a></div>");
    b.set_computed_limits(ComputedLimits { max_sources: 1, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), lim("computed-sources"));
    let mut b = at(&format!("<style>{}</style><div id=a></div>", "a{color:red}".repeat(1000)));
    b.set_computed_limits(ComputedLimits { max_sheet_bytes: 1000, ..Default::default() });
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).display"), lim("computed-sources"));
    assert_eq!(b.computed_stats().builds, 0);
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

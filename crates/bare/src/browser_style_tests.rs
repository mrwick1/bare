//! Tests of `browser_style`: `HTMLElement.style` / `CSSStyleDeclaration` over the native `style` attribute.
//! Expectations come from the CSSOM and HTML standards, read by hand. Node has no DOM, so it is not an oracle here.

use crate::browser::*;
use crate::browser_style::StyleLimits;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=a style='color: red; margin: 0'></div><p id=b></p></body></html>";

fn at(page: &str) -> Browser {
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
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

/// The shape of React's vendor-prefix probe: `style` must be an object so that `in` does not throw.
#[test]
fn react_prefix_probe_shape() {
    let mut b = at(PAGE);
    assert_eq!(js(&mut b, "typeof document.createElement('div').style"), "object");
    assert_eq!(js(&mut b, "var sb = document.createElement('div').style; ('WebkitAnimation' in sb) + ',' + ('animationend' in sb)"), "false,false");
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", bare_js::eval::LimitKind::Host(name))
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const A: &str = "var a = document.getElementById('a'), p = document.getElementById('b'), s = a.style;";

#[test]
fn same_object_and_brand_checks() {
    let mut b = at("<!doctype html><svg id=sv></svg><div id=a></div>");
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); a.style === a.style"), "true");
    assert_eq!(js(&mut b, "a.style !== document.createElement('div').style"), "true");
    assert_eq!(js(&mut b, "a.style instanceof CSSStyleDeclaration && a.style.constructor === CSSStyleDeclaration"), "true");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'style'); typeof d.get + typeof d.set + d.enumerable + d.configurable"), "functionfunctiontruetrue");
    assert_eq!(js(&mut b, "'style' in Element.prototype"), "false");
    assert_eq!(js(&mut b, "d.get.call(document.createTextNode('x'))"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "d.get.call({})"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "d.get.call(document.getElementById('sv'))"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "document.getElementById('sv').style"), "undefined");
    // The accessor of another interface does not work on the declaration, and the reverse.
    assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(CSSStyleDeclaration.prototype, 'cssText').get.call(a)"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(CSSStyleDeclaration.prototype, 'color').get.call({})"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "CSSStyleDeclaration.prototype.color"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "CSSStyleDeclaration.prototype.setProperty.call(a, 'color', 'red')"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "a.style.hasOwnProperty('color')"), "false");
}

#[test]
fn constructor_gates() {
    let mut b = at(PAGE);
    // Like every host interface object here (`Node`, `DOMTokenList`): not a constructor, and no instance can be made.
    for src in ["new CSSStyleDeclaration()", "CSSStyleDeclaration()", "class X extends CSSStyleDeclaration {}; new X()"] {
        let r = js(&mut b, src);
        assert!(r.starts_with("throw:TypeError"), "{src}: {r}");
    }
    assert_eq!(js(&mut b, "new CSSStyleDeclaration.prototype.constructor()").starts_with("throw:TypeError"), true);
    assert_eq!(js(&mut b, "Object.getPrototypeOf(CSSStyleDeclaration.prototype) === Object.prototype"), "true");
}

#[test]
fn reflects_the_attribute_both_ways() {
    let mut b = at(PAGE);
    js(&mut b, A);
    assert_eq!(js(&mut b, "s.cssText"), "color: red; margin: 0;");
    assert_eq!(js(&mut b, "s.length + ',' + s.item(0) + ',' + s.item(1) + ',[' + s.item(2) + '],[' + s.item(-1) + ']'"), "2,color,margin,[],[]");
    assert_eq!(js(&mut b, "s.getPropertyValue('COLOR') + s.color + s.margin + '|' + s.width"), "redred0|");
    // The attribute text is untouched until something writes.
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "color: red; margin: 0");
    // setAttribute from outside is seen by the same object.
    js(&mut b, "a.setAttribute('style', 'width: 5px')");
    assert_eq!(js(&mut b, "s.cssText + s.length + '[' + s.color + ']'"), "width: 5px;1[]");
    js(&mut b, "s.setProperty('height', '2px')");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "width: 5px; height: 2px;");
    js(&mut b, "a.removeAttribute('style')");
    assert_eq!(js(&mut b, "s.length + '[' + s.cssText + ']' + a.hasAttribute('style')"), "0[]false");
    js(&mut b, "s.color = 'blue'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "color: blue;");
}

#[test]
fn set_property_rules() {
    let mut b = at(PAGE);
    js(&mut b, A);
    // Invalid value, unsupported property, bad priority: nothing changes, nothing throws, the attribute text stays as it was.
    js(&mut b, "a.setAttribute('style', 'width:5px;;junk')");
    for call in [
        "s.setProperty('width', 'zzz')",
        "s.setProperty('width', '1px; height: 2px')",
        "s.setProperty('width', '1px !important')",
        "s.setProperty('width', '1px', 'bogus')",
        "s.setProperty('animation', 'x 1s')",
        "s.setProperty('nonsense', '1')",
        "s.setProperty('', '1')",
        "s.setProperty('width', '}')",
        "s.width = 'zzz'",
        "s.animation = 'x'",
        "s.removeProperty('height')",
        "s.removeProperty('animation')",
    ] {
        assert_eq!(js(&mut b, &format!("{call}; a.getAttribute('style')")), "width:5px;;junk", "{call}");
    }
    assert_eq!(js(&mut b, "s.removeProperty('height') + '|' + s.removeProperty('nope')"), "|");
    // Priority.
    js(&mut b, "s.cssText = ''");
    js(&mut b, "s.setProperty('width', '1px', 'IMPORTANT')");
    assert_eq!(js(&mut b, "a.getAttribute('style') + s.getPropertyPriority('width') + '|' + s.getPropertyPriority('height')"), "width: 1px !important;important|");
    js(&mut b, "s.width = '2px'");
    assert_eq!(js(&mut b, "a.getAttribute('style') + '[' + s.getPropertyPriority('width') + ']'"), "width: 2px;[]");
    js(&mut b, "s.setProperty('width', '3px', null)");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "width: 3px;");
    js(&mut b, "s.setProperty('width', '4px', undefined)");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "width: 4px;");
    // Empty value and null remove.
    assert_eq!(js(&mut b, "s.setProperty('width', ''); a.getAttribute('style')"), "");
    js(&mut b, "s.width = '1px'; s.height = '2px'");
    assert_eq!(js(&mut b, "s.width = null; a.getAttribute('style')"), "height: 2px;");
    assert_eq!(js(&mut b, "s.height = ''; a.getAttribute('style') + s.length"), "0");
    // The property name is case-insensitive, the value is kept as written.
    assert_eq!(js(&mut b, "s.setProperty('COLOR', 'RED'); a.getAttribute('style')"), "color: RED;");
    // Argument counts.
    assert_eq!(js(&mut b, "s.setProperty('a')"), "throw:TypeError:Failed to execute 'setProperty' on 'CSSStyleDeclaration': 2 arguments required, but only 1 present.");
    assert_eq!(js(&mut b, "s.getPropertyValue()"), "throw:TypeError:Failed to execute 'getPropertyValue' on 'CSSStyleDeclaration': 1 argument required, but only 0 present.");
    assert_eq!(js(&mut b, "s.removeProperty()"), "throw:TypeError:Failed to execute 'removeProperty' on 'CSSStyleDeclaration': 1 argument required, but only 0 present.");
    assert_eq!(js(&mut b, "s.item()"), "throw:TypeError:Failed to execute 'item' on 'CSSStyleDeclaration': 1 argument required, but only 0 present.");
    // A change on an element with no style attribute makes one; a no-op does not.
    assert_eq!(js(&mut b, "p.style.removeProperty('color'); p.style.setProperty('animation', 'x'); p.style.setProperty('color', 'zzz'); p.getAttribute('style')"), "null");
    assert_eq!(js(&mut b, "p.style.color = 'blue'; p.getAttribute('style')"), "color: blue;");
    assert_eq!(js(&mut b, "p.style.removeProperty('color') + '|' + p.getAttribute('style')"), "blue|");
}

#[test]
fn custom_properties() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = ''; s.setProperty('--Foo', '  bar baz ')");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "--Foo: bar baz;");
    assert_eq!(js(&mut b, "s.getPropertyValue('--Foo') + '|' + s.getPropertyValue('--foo') + '|' + s.length + s.item(0)"), "bar baz||1--Foo");
    js(&mut b, "s.setProperty('--foo', '1')");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "--Foo: bar baz; --foo: 1;");
    assert_eq!(js(&mut b, "s.setProperty('--x', 'a; b: c'); a.getAttribute('style')"), "--Foo: bar baz; --foo: 1;");
    assert_eq!(js(&mut b, "s.setProperty('--x', 'a !important'); a.getAttribute('style')"), "--Foo: bar baz; --foo: 1;");
    assert_eq!(js(&mut b, "s.removeProperty('--Foo') + '|' + a.getAttribute('style')"), "bar baz|--foo: 1;");
    // `--` alone and `--a b` are not names.
    assert_eq!(js(&mut b, "s.setProperty('--', '1'); s.setProperty('--a b', '1'); a.getAttribute('style')"), "--foo: 1;");
    // var() in a normal property is accepted by the engine.
    assert_eq!(js(&mut b, "s.width = 'var(--w)'; s.width"), "var(--w)");
    assert_eq!(js(&mut b, "s.width = 'var(--w'; s.width"), "var(--w)");
}

#[test]
fn order_and_duplicates() {
    let mut b = at(PAGE);
    js(&mut b, A);
    let cases = [
        ("color: red; margin: 0; color: blue", "color: blue; margin: 0;"),
        ("color: red !important; color: blue", "color: red !important;"),
        ("color: blue; color: red !important", "color: red !important;"),
        ("COLOR: Red", "color: Red;"),
        ("color: red; oops; width: zzz; margin: 0 1px 2px 3px 4px; padding: 1px", "color: red; padding: 1px;"),
        ("margin-top: 1px; margin: 0", "margin-top: 1px; margin: 0;"),
        ("margin-top: 1px; margin: 0; margin-top: 2px", "margin: 0; margin-top: 2px;"),
        ("", ""),
        (";;;", ""),
    ];
    for (input, want) in cases {
        js(&mut b, &format!("s.cssText = {input:?}"));
        assert_eq!(js(&mut b, "s.cssText"), want, "{input}");
        assert_eq!(js(&mut b, "a.getAttribute('style')"), want, "{input}");
    }
    // Replacing a shorthand's longhands.
    js(&mut b, "s.cssText = 'margin-top: 1px; margin: 0'");
    js(&mut b, "s.margin = '5px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "margin: 5px;");
    js(&mut b, "s.marginTop = '9px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "margin: 5px; margin-top: 9px;");
    js(&mut b, "s.cssText = 'margin-top: 1px; margin: 0; color: red'; s.marginTop = '9px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "margin: 0; color: red; margin-top: 9px;");
    // Removing a shorthand removes the longhands it covers; a longhand does not edit the shorthand.
    js(&mut b, "s.cssText = 'margin-top: 1px; margin: 0; color: red'");
    assert_eq!(js(&mut b, "s.removeProperty('margin-top') + '|' + a.getAttribute('style')"), "1px|margin: 0; color: red;");
    assert_eq!(js(&mut b, "s.removeProperty('margin') + '|' + a.getAttribute('style')"), "0|color: red;");
    // In-place replacement keeps the position.
    js(&mut b, "s.cssText = 'color: red; width: 1px; height: 2px'; s.width = '5px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "color: red; width: 5px; height: 2px;");
}

#[test]
fn named_properties_and_feature_detection() {
    let mut b = at(PAGE);
    js(&mut b, A);
    assert_eq!(js(&mut b, "['color','marginTop','margin-top','margin','cssFloat','float','WebkitBorderHorizontalSpacing','webkitBorderHorizontalSpacing','-webkit-border-horizontal-spacing','backgroundColor','gridTemplateColumns','placeItems'].map(function(k){return k in s}).join()"), "true,true,true,true,true,true,true,true,true,true,true,true");
    assert_eq!(js(&mut b, "['WebkitAnimation','webkitAnimation','animation','transition','animationend','AnimationEvent','MozAnimation','transform','opacity','foo','length2'].map(function(k){return k in s}).join()"), "false,false,false,false,false,false,false,false,false,false,false");
    // An unsupported name stays a plain JS property: it does not reach the attribute.
    assert_eq!(js(&mut b, "var before = a.getAttribute('style'); s.animation = 'x 1s'; s.animation + (a.getAttribute('style') === before)"), "x 1strue");
    js(&mut b, "s.cssText = ''; delete s.animation");
    assert_eq!(js(&mut b, "s.cssFloat = 'left'; s.float + '|' + s.cssFloat + '|' + a.getAttribute('style')"), "left|left|float: left;");
    assert_eq!(js(&mut b, "s['margin-top'] = '3px'; s.marginTop + '|' + a.getAttribute('style')"), "3px|float: left; margin-top: 3px;");
    assert_eq!(js(&mut b, "var g = Object.getOwnPropertyDescriptor(CSSStyleDeclaration.prototype, 'marginTop'); g.get.name + ',' + g.set.name + ',' + g.set.length + ',' + g.enumerable"), "get marginTop,set marginTop,1,true");
    assert_eq!(js(&mut b, "Object.keys(CSSStyleDeclaration.prototype).indexOf('color') >= 0"), "true");
    // Two elements do not share state.
    assert_eq!(js(&mut b, "p.style.color = 'blue'; s.color + '|' + p.style.color"), "|blue");
}

/// Every supported property has its own accessor pair: set through one name, read back through the css name, and nothing else changes.
#[test]
fn every_property_has_its_own_slot() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, A);
        let mut names: Vec<&str> = bare_css::values::Property::ALL.iter().map(|p| p.name()).collect();
        names.extend(bare_css::values::SHORTHANDS);
        assert!(names.len() > 100);
        for css in names {
            let mut camel = String::new();
            let mut up = false;
            for c in css.chars() {
                if c == '-' { up = true } else if up { camel.push(c.to_ascii_uppercase()); up = false } else { camel.push(c) }
            }
            let r = js(&mut b, &format!("s.cssText = ''; s['{camel}'] = 'inherit'; s.cssText + '|' + s['{camel}'] + '|' + s.getPropertyValue('{css}')"));
            assert_eq!(r, format!("{css}: inherit;|inherit|inherit"), "{camel}");
            let r = js(&mut b, &format!("s.cssText = ''; s.setProperty('{css}', 'initial'); s['{camel}'] + '|' + s.length"));
            assert_eq!(r, "initial|1", "{css}");
        }
    });
}

#[test]
fn css_text_and_forwarding_setter() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = 'color:red;;bad;margin:0;width:zzz;animation:x 1s'");
    assert_eq!(js(&mut b, "s.cssText + '|' + a.getAttribute('style')"), "color: red; margin: 0;|color: red; margin: 0;");
    js(&mut b, "s.cssText = ''");
    assert_eq!(js(&mut b, "a.getAttribute('style') === ''"), "true");
    js(&mut b, "a.style = 'height: 3px'");
    assert_eq!(js(&mut b, "s.cssText + (a.style === s)"), "height: 3px;true");
    js(&mut b, "a.style = null");
    assert_eq!(js(&mut b, "s.cssText + '|' + a.getAttribute('style')"), "|");
    js(&mut b, "s.cssText = 'color: red'; s.cssText = null");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "");
    // cssText on an element that had no attribute makes one, even when empty (the standard always sets it).
    assert_eq!(js(&mut b, "p.style.cssText = ''; p.getAttribute('style') === ''"), "true");
}

#[test]
fn conversions_run_before_the_block_is_read() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = ''");
    // A valueOf that changes the attribute runs before the block is read, so the change is not lost.
    js(&mut b, "s.setProperty('color', { toString: function () { a.setAttribute('style', 'width: 1px'); return 'red'; } })");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "width: 1px; color: red;");
    // A throwing conversion changes nothing.
    assert_eq!(js(&mut b, "try { s.setProperty('color', { toString: function () { throw new Error('boom'); } }); 'no' } catch (e) { e.message + a.getAttribute('style') }"), "boomwidth: 1px; color: red;");
    assert_eq!(js(&mut b, "s.color = { toString: function () { return 'blue'; } }; s.color"), "blue");
    // Lone surrogates cannot be stored.
    let r = js(&mut b, "s.setProperty('color', '\\uD800')");
    assert!(r.starts_with("unsupported:"), "{r}");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "width: 1px; color: blue;");
}

#[test]
fn native_cascade_sees_mutations() {
    use bare_css::cascade::{Origin, RuleSet, UA_CSS};
    use bare_css::values::{Property, Value};
    let mut b = at(PAGE);
    js(&mut b, A);
    let id = |b: &mut Browser, sel: &str| b.query(sel).unwrap().unwrap();
    let styled = |b: &mut Browser, sel: &str, f: &dyn Fn(&bare_css::cascade::ComputedStyle) -> String| {
        let n = id(b, sel);
        let doc = b.document();
        let mut doc = doc.borrow_mut();
        let mut rules = RuleSet::new((1000.0, 500.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, "#a { margin-left: 1px }", Origin::Author);
        let styles = rules.compute(&doc);
        styles.style(n).map_or("none".to_string(), f)
    };
    let ml = |s: &bare_css::cascade::ComputedStyle| {
        if s.get(Property::Display) == Value::Keyword(bare_css::values::Keyword::None) { "none".to_string() } else { format!("{}", s.px(Property::MarginLeft)) }
    };
    assert_eq!(styled(&mut b, "#a", &ml), "0"); // the attribute `margin: 0` beats the selector
    js(&mut b, "s.marginLeft = '7px'");
    assert_eq!(styled(&mut b, "#a", &ml), "7");
    js(&mut b, "a.setAttribute('style', 'margin-left: 3px')");
    assert_eq!(styled(&mut b, "#a", &ml), "3");
    js(&mut b, "s.removeProperty('margin-left')");
    assert_eq!(styled(&mut b, "#a", &ml), "1"); // the selector shows again
    js(&mut b, "s.margin = '4px 5px'");
    assert_eq!(styled(&mut b, "#a", &ml), "5");
    js(&mut b, "s.setProperty('margin-left', '6px', 'important'); s.margin = '2px'");
    assert_eq!(styled(&mut b, "#a", &ml), "2");
    js(&mut b, "s.display = 'none'");
    assert_eq!(styled(&mut b, "#a", &ml), "none");
    js(&mut b, "s.display = ''");
    assert_eq!(styled(&mut b, "#a", &ml), "2");
}

#[test]
fn gc_keeps_and_prunes_declarations() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var keep = (function () { var d = document.createElement('div'); return d.style; })();");
        js(&mut b, "keep.color = 'red'; 0");
        b.collect(&[]).unwrap();
        // The declaration keeps its detached element, and the element keeps its attribute state.
        assert_eq!(js(&mut b, "keep.cssText"), "color: red;");
        assert_eq!(b.style_stats().objects, 1);
        js(&mut b, "keep = null; 0");
        b.collect(&[]).unwrap();
        assert_eq!(b.style_stats().objects, 0);
        // SameObject while it lives, across a collection.
        js(&mut b, "var x = document.body.style; x.color = 'red'; 0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.body.style === x && x.cssText"), "color: red;");
        // WeakMap values keyed by a declaration live with it and die with it.
        js(&mut b, "var wm = new WeakMap(); var k = document.createElement('div').style; wm.set(k, { n: 7 }); (function () { wm.set(document.createElement('div').style, { n: 1 }); })(); 0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "wm.get(k).n"), "7");
        assert_eq!(b.style_stats().objects, 2); // body's and k's
        // Slot reuse: new declarations do not inherit anything.
        js(&mut b, "for (var i = 0; i < 20; i++) { var e = document.createElement('div'); e.style.color = 'red'; } 0");
        b.collect(&[]).unwrap();
        js(&mut b, "var fresh = []; for (var i = 0; i < 20; i++) fresh.push(document.createElement('div').style.cssText); 0");
        assert_eq!(js(&mut b, "fresh.join('')"), "");
        // An attached element's declaration is kept by its element, which the document keeps.
        js(&mut b, "document.body.appendChild(document.createElement('i')).style.color = 'red'; 0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.body.lastChild.style.cssText"), "color: red;");
    });
}

#[test]
fn caps_are_uncatchable_and_checked_before_growth() {
    // style-objects
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_objects: 3, ..Default::default() });
    assert_eq!(js(&mut b, "var r = 'none'; try { for (var i = 0; i < 5; i++) document.createElement('div').style; } catch (e) { r = 'caught'; } finally { r = 'finally'; } r"), lim("style-objects"));
    assert_eq!(b.style_stats().objects, 3);
    // style-bytes: a block, a value, a cssText
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_bytes: 64, ..Default::default() });
    js(&mut b, A);
    js(&mut b, &format!("a.setAttribute('style', 'color:red;{}width:1px')", " ".repeat(100)));
    assert_eq!(js(&mut b, "try { s.length } catch (e) { 'caught' }"), lim("style-bytes"));
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_bytes: 64, ..Default::default() });
    js(&mut b, A);
    assert_eq!(js(&mut b, &format!("try {{ s.setProperty('--x', '{}') }} catch (e) {{ 'caught' }}", "y".repeat(100))), lim("style-bytes"));
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_bytes: 64, ..Default::default() });
    js(&mut b, A);
    assert_eq!(js(&mut b, &format!("try {{ s.cssText = '--x:{}' }} catch (e) {{ 'caught' }}", "y".repeat(100))), lim("style-bytes"));
    // style-decls
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_decls: 3, ..Default::default() });
    js(&mut b, A);
    js(&mut b, "s.cssText = 'color:red; width:1px; height:1px'");
    assert_eq!(js(&mut b, "try { s.marginTop = '1px' } catch (e) { 'caught' }"), lim("style-decls"));
    js(&mut b, "a.setAttribute('style', 'color:red; width:1px; height:1px; margin:0')");
    assert_eq!(js(&mut b, "try { s.length } catch (e) { 'caught' }"), lim("style-decls"));
    // The scan is charged to the step budget before it runs.
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 20_000, ..Default::default() }, HeapLimits::default()).unwrap();
    js(&mut b, A);
    js(&mut b, &format!("a.setAttribute('style', 'color:red;{}')", " ".repeat(200_000)));
    assert_eq!(js(&mut b, "s.length"), "limit:Steps");
    // The native text cap stays: writes add text to the arena, which never shrinks.
    let doc = tree::parse(PAGE);
    let cap = doc.text_bytes() + 300;
    let mut b = Browser::new(doc, BrowserLimits { max_text_bytes: cap, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).unwrap();
    js(&mut b, A);
    assert_eq!(js(&mut b, "try { for (var i = 0; i < 100; i++) s.setProperty('--v', 'value number ' + i); 'ok' } catch (e) { 'caught' }"), lim("dom-text"));
    assert!(b.stats().text_bytes <= cap);
}

#[test]
fn resource_error_stops_the_script() {
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_objects: 1, ..Default::default() });
    js(&mut b, "var log = []; document.createElement('div').style; 0");
    // `finally` and `catch` do not run for a resource error.
    assert_eq!(js(&mut b, "try { document.createElement('i').style; log.push('after') } catch (e) { log.push('catch') } finally { log.push('finally') }"), lim("style-objects"));
    assert_eq!(js(&mut b, "log.length"), "0");
}

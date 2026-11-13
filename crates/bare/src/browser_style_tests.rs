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
    // SVGElement has its own `style`; the HTML accessor still refuses an SVG receiver.
    assert_eq!(js(&mut b, "var sv = document.getElementById('sv'); sv.style === sv.style && sv.style instanceof CSSStyleDeclaration"), "true");
    assert_eq!(js(&mut b, "sv.style.marginBottom = '4px'; sv.getAttribute('style')"), "margin-bottom: 4px;");
    assert_eq!(js(&mut b, "sv.style = 'color: red'; sv.style.color + '|' + sv.getAttribute('style')"), "red|color: red;");
    assert_eq!(js(&mut b, "var sd = Object.getOwnPropertyDescriptor(SVGElement.prototype, 'style'); typeof sd.get + typeof sd.set + ('style' in Element.prototype)"), "functionfunctionfalse");
    assert_eq!(js(&mut b, "sd.get.call(a)"), "throw:TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "sd.get.call(document.createElementNS('http://www.w3.org/1998/Math/MathML', 'math'))"), "throw:TypeError:Illegal invocation");
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
    assert!(js(&mut b, "new CSSStyleDeclaration.prototype.constructor()").starts_with("throw:TypeError"));
    assert_eq!(js(&mut b, "Object.getPrototypeOf(CSSStyleDeclaration.prototype) === Object.prototype"), "true");
}

#[test]
fn reflects_the_attribute_both_ways() {
    let mut b = at(PAGE);
    js(&mut b, A);
    assert_eq!(js(&mut b, "s.cssText"), "color: red; margin: 0;");
    assert_eq!(js(&mut b, "s.length + ',' + s.item(0) + ',' + s.item(1) + ',[' + s.item(2) + '],[' + s.item(-1) + ']'"), "5,color,margin-top,[margin-right],[]");
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
        "s.setProperty('filter', 'blur(2px)')",
        "s.setProperty('nonsense', '1')",
        "s.setProperty('', '1')",
        "s.setProperty('width', '}')",
        "s.width = 'zzz'",
        "s.filter = 'x'",
        "s.removeProperty('height')",
        "s.removeProperty('filter')",
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
    assert_eq!(js(&mut b, "p.style.removeProperty('color'); p.style.setProperty('filter', 'x'); p.style.setProperty('color', 'zzz'); p.getAttribute('style')"), "null");
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
        ("color: red; oops; width: zzz; margin: 0 1px 2px 3px 4px; padding: 1px", "color: red; width: zzz; margin: 0 1px 2px 3px 4px; padding: 1px;"),
        // A later shorthand takes every longhand of an earlier one; the later longhand takes one back (CSSOM: one value per longhand).
        ("margin-top: 1px; margin: 0", "margin: 0;"),
        ("margin-top: 1px; margin: 0; margin-top: 2px", "margin: 2px 0 0;"),
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
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "margin: 9px 5px 5px;");
    js(&mut b, "s.cssText = 'margin-top: 1px; margin: 0; color: red'; s.marginTop = '9px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "margin: 9px 0 0; color: red;");
    // Removing a longhand edits the shorthand that set it; removing a shorthand removes its longhands.
    js(&mut b, "s.cssText = 'margin-top: 1px; margin: 0; color: red'");
    assert_eq!(js(&mut b, "s.removeProperty('margin-top') + '|' + a.getAttribute('style')"), "0|margin-right: 0; margin-bottom: 0; margin-left: 0; color: red;");
    assert_eq!(js(&mut b, "s.removeProperty('margin') + '|' + a.getAttribute('style')"), "|color: red;");
    js(&mut b, "s.cssText = 'margin: 0 1px; color: red'");
    assert_eq!(js(&mut b, "s.removeProperty('margin') + '|' + a.getAttribute('style')"), "0 1px|color: red;");
    // In-place replacement keeps the position.
    js(&mut b, "s.cssText = 'color: red; width: 1px; height: 2px'; s.width = '5px'");
    assert_eq!(js(&mut b, "a.getAttribute('style')"), "color: red; width: 5px; height: 2px;");
}

#[test]
fn named_properties_and_feature_detection() {
    let mut b = at(PAGE);
    js(&mut b, A);
    assert_eq!(js(&mut b, "['color','marginTop','margin-top','margin','cssFloat','float','WebkitBorderHorizontalSpacing','webkitBorderHorizontalSpacing','-webkit-border-horizontal-spacing','backgroundColor','gridTemplateColumns','placeItems'].map(function(k){return k in s}).join()"), "true,true,true,true,true,true,true,true,true,true,true,true");
    assert_eq!(js(&mut b, "['WebkitAnimation','webkitAnimation','animation','transition','animationend','AnimationEvent','MozAnimation','transform','foo','length2'].map(function(k){return k in s}).join()"), "false,false,true,true,false,false,false,false,false,false");
    // An unsupported name stays a plain JS property: it does not reach the attribute.
    assert_eq!(js(&mut b, "var before = a.getAttribute('style'); s.filter = 'x 1s'; s.filter + (a.getAttribute('style') === before)"), "x 1strue");
    js(&mut b, "s.cssText = ''; delete s.filter");
    assert_eq!(js(&mut b, "s.cssFloat = 'left'; s.float + '|' + s.cssFloat + '|' + a.getAttribute('style')"), "left|left|float: left;");
    assert_eq!(js(&mut b, "s['margin-top'] = '3px'; s.marginTop + '|' + a.getAttribute('style')"), "3px|float: left; margin-top: 3px;");
    assert_eq!(js(&mut b, "var g = Object.getOwnPropertyDescriptor(CSSStyleDeclaration.prototype, 'marginTop'); g.get.name + ',' + g.set.name + ',' + g.set.length + ',' + g.enumerable"), "get marginTop,set marginTop,1,true");
    assert_eq!(js(&mut b, "Object.keys(CSSStyleDeclaration.prototype).indexOf('color') >= 0"), "true");
    // Two elements do not share state.
    assert_eq!(js(&mut b, "p.style.color = 'blue'; s.color + '|' + p.style.color"), "|blue");
}

/// `user-select` and its `-webkit-` alias are one property. Each of the five names reads "" when unset, and any name reads a value set through another.
#[test]
fn user_select_and_its_webkit_alias_are_one_property() {
    let mut b = at(PAGE);
    js(&mut b, A);
    let names = "['userSelect','user-select','webkitUserSelect','WebkitUserSelect','-webkit-user-select']";
    assert_eq!(js(&mut b, &format!("{names}.map(function(k){{return k in s}}).join()")), "true,true,true,true,true");
    assert_eq!(js(&mut b, &format!("{names}.map(function(k){{return s[k]}}).join('|')")), "||||");
    assert_eq!(js(&mut b, &format!("s.cssText = ''; s.WebkitUserSelect = 'none'; {names}.map(function(k){{return s[k]}}).join('|') + '|' + s.cssText")), "none|none|none|none|none|user-select: none;");
    assert_eq!(js(&mut b, &format!("s.userSelect = 'text'; s['-webkit-user-select'] + '|' + s.webkitUserSelect + '|' + a.getAttribute('style')")), "text|text|user-select: text;");
    assert_eq!(js(&mut b, &format!("s.cssText = ''; s.getPropertyValue('-webkit-user-select') + '|' + s.length")), "|0");
}

/// Every supported property has its own accessor pair: set through one name, read back through the css name, and nothing else changes.
#[test]
fn every_property_has_its_own_slot() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, A);
        let mut names: Vec<&str> = bare_css::values::Property::LISTED.iter().map(|p| p.name()).collect();
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
            let n = bare_css::values::shorthand_longhands(css).map_or(1, |l| l.len());
            assert_eq!(r, format!("initial|{n}"), "{css}");
        }
    });
}

#[test]
fn css_text_and_forwarding_setter() {
    let mut b = at(PAGE);
    js(&mut b, A);
    // `bad` is not a declaration and goes; the ones the engine does not read stay as they were written.
    js(&mut b, "s.cssText = 'color:red;;bad;margin:0;width:zzz;filter:x 1s'");
    assert_eq!(js(&mut b, "s.cssText + '|' + a.getAttribute('style')"), "color: red; margin: 0; width: zzz; filter: x 1s;|color: red; margin: 0; width: zzz; filter: x 1s;");
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

// ---------------------------------------------------------------------------------------------- shorthand contract
// Expectations come from CSSOM "CSSStyleDeclaration" (longhand list, shorthand serialization) and CSS Cascade 4
// ("shorthand properties"), read by hand. Values are as written; the engine keeps no canonical form (`1px`, not `1px`).

fn attr(b: &mut Browser) -> String {
    js(b, "a.getAttribute('style')")
}

/// Defect 1: a supported shorthand is expanded into its longhands.
#[test]
fn shorthand_expands_to_longhands() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = ''; s.margin = '1px'");
    assert_eq!(js(&mut b, "[s.marginTop, s.marginRight, s.marginBottom, s.marginLeft, s.getPropertyValue('margin-top')].join()"), "1px,1px,1px,1px,1px");
    assert_eq!(js(&mut b, "s.length"), "4");
    assert_eq!(js(&mut b, "[0,1,2,3,4].map(function(i){return s.item(i)}).join()"), "margin-top,margin-right,margin-bottom,margin-left,");
    assert_eq!(js(&mut b, "s.margin"), "1px");
    // Removing one longhand leaves the other three, and the attribute still means exactly that to the cascade.
    assert_eq!(js(&mut b, "s.removeProperty('margin-top')"), "1px");
    assert_eq!(js(&mut b, "s.length + '|' + s.marginTop + '|' + s.marginLeft + '|[' + s.margin + ']'"), "3||1px|[]");
    assert_eq!(attr(&mut b), "margin-right: 1px; margin-bottom: 1px; margin-left: 1px;");
    // Setting a longhand over a shorthand collapses back (all four present, same priority).
    js(&mut b, "s.cssText = ''; s.margin = '1px 2px'; s.marginTop = '9px'");
    assert_eq!(js(&mut b, "s.margin + '|' + s.marginBottom + '|' + s.length"), "9px 2px 1px|1px|4");
    assert_eq!(attr(&mut b), "margin: 9px 2px 1px;");
    // An authored block is read the same way.
    js(&mut b, "a.setAttribute('style', 'margin: 1px 2px 3px; color: red')");
    assert_eq!(js(&mut b, "[s.marginTop, s.marginRight, s.marginBottom, s.marginLeft, s.length, s.item(3), s.item(4)].join()"), "1px,2px,3px,2px,5,margin-left,color");
    js(&mut b, "s.color = 'blue'");
    assert_eq!(attr(&mut b), "margin: 1px 2px 3px; color: blue;");
}

#[test]
fn shorthand_priority_and_overlap() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = ''; s.setProperty('margin', '1px 2px', 'important')");
    assert_eq!(js(&mut b, "[s.getPropertyPriority('margin'), s.getPropertyPriority('margin-left'), s.getPropertyPriority('padding')].join()"), "important,important,");
    // A normal longhand next to important ones: the shorthand no longer serializes (mixed priority), the longhands do.
    js(&mut b, "s.marginTop = '5px'");
    assert_eq!(js(&mut b, "[s.getPropertyPriority('margin'), s.getPropertyPriority('margin-top'), s.getPropertyPriority('margin-left'), s.margin].join('|')"), "||important|");
    js(&mut b, "s.cssText = ''; s.setProperty('margin', '1px 2px', 'important'); s.setProperty('margin-top', '5px')");
    assert_eq!(attr(&mut b), "margin-top: 5px; margin-right: 2px !important; margin-bottom: 1px !important; margin-left: 2px !important;");
    assert_eq!(js(&mut b, "s.margin + '|' + s.getPropertyPriority('margin')"), "|");
    // Overlapping shorthands: the later write owns the shared longhands.
    js(&mut b, "s.cssText = ''; s.borderWidth = '1px'; s.borderTop = '2px solid red'");
    assert_eq!(js(&mut b, "[s.borderTopWidth, s.borderRightWidth, s.borderTopStyle, s.borderTopColor, s.borderRightStyle, s.length].join()"), "2px,1px,solid,red,,6");
    assert_eq!(js(&mut b, "s.borderTop + '|[' + s.borderWidth + ']|[' + s.border + ']'"), "2px solid red|[2px 1px 1px]|[]");
    // Duplicate order: replacing keeps the first position, a new longhand goes last.
    js(&mut b, "s.cssText = 'color: red; margin: 1px; width: 2px'; s.margin = '3px'");
    assert_eq!(attr(&mut b), "color: red; margin: 3px; width: 2px;");
    js(&mut b, "s.cssText = 'color: red; margin: 1px; width: 2px'; s.marginTop = '3px'");
    assert_eq!(js(&mut b, "[0,1,2,3,4,5,6].map(function(i){return s.item(i)}).join()"), "color,margin-top,margin-right,margin-bottom,margin-left,width,");
    assert_eq!(attr(&mut b), "color: red; margin: 3px 1px 1px; width: 2px;");
}

#[test]
fn flex_grid_border_shorthands() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = ''; s.flex = '2'");
    assert_eq!(js(&mut b, "[s.flexGrow, s.flexShrink, s.flexBasis, s.flex, s.length].join()"), "2,1,0%,2,3");
    js(&mut b, "s.flexGrow = '3'");
    assert_eq!(js(&mut b, "s.flex + '|' + a.getAttribute('style')"), "3 1 0%|flex: 3 1 0%;");
    js(&mut b, "s.cssText = ''; s.flex = '0 0 auto'");
    assert_eq!(js(&mut b, "[s.flexGrow, s.flexShrink, s.flexBasis].join()"), "0,0,auto");
    js(&mut b, "s.cssText = ''; s.gridRow = '2 / span 3'");
    assert_eq!(js(&mut b, "[s.gridRowStart, s.gridRowEnd, s.length].join()"), "2,span 3,2");
    js(&mut b, "s.gridRowEnd = ''");
    assert_eq!(js(&mut b, "s.length + '|' + s.gridRowStart + '|[' + s.gridRow + ']'"), "1|2|[]");
    assert_eq!(attr(&mut b), "grid-row-start: 2;");
    js(&mut b, "s.cssText = ''; s.gridArea = '1 / 2 / 3 / 4'");
    assert_eq!(js(&mut b, "[s.gridRowStart, s.gridColumnStart, s.gridRowEnd, s.gridColumnEnd, s.gridArea].join()"), "1,2,3,4,1 / 2 / 3 / 4");
    js(&mut b, "s.cssText = ''; s.border = '1px solid red'");
    assert_eq!(js(&mut b, "[s.borderTopWidth, s.borderLeftStyle, s.borderBottomColor, s.border, s.borderTop, s.borderWidth, s.length].join()"), "1px,solid,red,1px solid red,1px solid red,1px,12");
    js(&mut b, "s.borderTopWidth = '2px'");
    assert_eq!(js(&mut b, "[s.border, s.borderTop, s.borderWidth, s.borderStyle, s.borderColor].join('|')"), "|2px solid red|2px 1px 1px|solid|red");
    assert_eq!(attr(&mut b), "border-width: 2px 1px 1px; border-style: solid; border-color: red;");
    // The unit as written survives in a longhand (the engine keeps `1cm` as pixels; the text must not).
    js(&mut b, "s.cssText = ''; s.margin = '1cm 2PX'");
    assert_eq!(js(&mut b, "s.marginTop + ',' + s.marginRight + ',' + s.marginBottom + ',' + s.marginLeft"), "1cm,2PX,1cm,2PX");
}

#[test]
fn var_and_custom_values() {
    let mut b = at(PAGE);
    js(&mut b, A);
    // A shorthand with var() has no longhand text (pending substitution): longhands read as "", the shorthand as written.
    js(&mut b, "s.cssText = ''; s.margin = 'var(--m) 2px'");
    assert_eq!(js(&mut b, "[s.margin, '[' + s.marginTop + ']', s.length].join()"), "var(--m) 2px,[],4");
    // Pending longhands keep their source while exposed text is empty.
    assert_eq!(js(&mut b, "s.marginTop = '1px'; s.marginRight"), "");
    assert_eq!(attr(&mut b), "margin-top: 1px; margin-right: ; margin-bottom: ; margin-left: ;");
    assert_eq!(js(&mut b, "s.removeProperty('margin-left')"), "");
    assert_eq!(attr(&mut b), "margin-top: 1px; margin-right: ; margin-bottom: ;");
    assert_eq!(js(&mut b, "s.removeProperty('margin')"), "");
    assert_eq!(attr(&mut b), "");
    // Custom property: name case and raw value kept, in order with the rest.
    js(&mut b, "s.cssText = '--Big: { a: b }; MARGIN: 1px; --x: 1 2'");
    assert_eq!(js(&mut b, "[s.getPropertyValue('--Big'), s.getPropertyValue('--big'), s.marginTop, s.getPropertyValue('--x'), s.length].join('|')"), "{ a: b }||1px|1 2|6");
    js(&mut b, "s.marginLeft = '4px'");
    assert_eq!(attr(&mut b), "--Big: { a: b }; margin: 1px 1px 1px 4px; --x: 1 2;");
}

#[test]
fn atomic_errors() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = 'margin: 1px; color: red'");
    let before = attr(&mut b);
    for call in ["s.margin = '1px 2px 3px 4px 5px'", "s.setProperty('margin', '1px bogus')", "s.border = '1px solid red blue'", "s.flex = 'x'", "s.setProperty('margin-top', '1px', 'nope')"] {
        assert_eq!(js(&mut b, &format!("{call}; s.length + a.getAttribute('style')")), format!("5{before}"), "{call}");
    }
}

/// Defect 2: declarations the engine does not interpret stay in the attribute through unrelated writes.
#[test]
fn uninterpreted_declarations_are_preserved() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "a.setAttribute('style', 'will-change: opacity; color: red; --k: \"a;b\"; filter: blur(2px); width: zzz')");
    assert_eq!(js(&mut b, "s.length + '|' + s.color + '|' + s.item(0) + '|' + s.item(1)"), "2|red|color|--k");
    js(&mut b, "s.height = '2px'");
    assert_eq!(attr(&mut b), "will-change: opacity; color: red; --k: \"a;b\"; filter: blur(2px); width: zzz; height: 2px;");
    js(&mut b, "s.removeProperty('color')");
    assert_eq!(attr(&mut b), "will-change: opacity; --k: \"a;b\"; filter: blur(2px); width: zzz; height: 2px;");
    // An important unknown stays important, and the cssText setter keeps what it was given the same way.
    js(&mut b, "s.cssText = 'will-change: transform !important; color: red'");
    assert_eq!(attr(&mut b), "will-change: transform !important; color: red;");
    assert_eq!(js(&mut b, "s.cssText"), "will-change: transform !important; color: red;");
    // Only unknown declarations: a no-op write leaves the attribute byte for byte.
    js(&mut b, "a.setAttribute('style', 'will-change:none;;')");
    js(&mut b, "s.setProperty('color', 'zzz'); s.removeProperty('color')");
    assert_eq!(attr(&mut b), "will-change:none;;");
}

/// Defect 3: caps run before the block is built, and one declaration list is never longer than the cap.
#[test]
fn caps_bound_the_parse_and_the_output() {
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_decls: 4, max_bytes: 1 << 20, ..Default::default() });
    js(&mut b, A);
    js(&mut b, &format!("a.setAttribute('style', '{}')", "--a:1;".repeat(100_000)));
    assert_eq!(js(&mut b, "try { s.length } catch (e) { 'caught' }"), lim("style-decls"));
    // The count includes declarations that only the native cascade understands.
    js(&mut b, "a.setAttribute('style', 'a:1;b:1;c:1;d:1;e:1')");
    assert_eq!(js(&mut b, "try { s.color = 'red' } catch (e) { 'caught' }"), lim("style-decls"));
    assert_eq!(attr(&mut b), "a:1;b:1;c:1;d:1;e:1");
    // A cssText setter over the cap fails before the attribute changes.
    js(&mut b, "a.setAttribute('style', 'color:red')");
    assert_eq!(js(&mut b, "try { s.cssText = 'a:1;b:1;c:1;d:1;e:1' } catch (e) { 'caught' }"), lim("style-decls"));
    assert_eq!(attr(&mut b), "color:red");
    // The serialized output is bounded in bytes before it is built: eight longhands of 40 bytes cannot be written under 64.
    let mut b = at(PAGE);
    b.set_style_limits(StyleLimits { max_bytes: 100, ..Default::default() });
    js(&mut b, A);
    js(&mut b, "s.margin = '1px 2px 3px 4px'");
    assert_eq!(js(&mut b, "try { s.paddingTop = '1px'; s.removeProperty('margin-top'); s.removeProperty('margin-left'); s.paddingLeft = '2px'; s.paddingRight = '1px'; s.paddingBottom = '1px'; s.width = '1px'; s.height = '1px'; 'ok' } catch (e) { 'caught' }"), lim("style-bytes"));
    assert!(attr(&mut b).len() <= 100);
}

/// Defect 4: a lone surrogate is never ignored. Every entry point that takes a string refuses it.
#[test]
fn lone_surrogates_are_refused_at_every_entry() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.cssText = 'color: red'");
    for src in [
        "s.cssText = '\\uD800'", "s.cssText = 'color:red;--x:\\uDC00'", "s.color = '\\uD800'", "s.margin = '\\uD800'", "s.setProperty('\\uD800', '1')",
        "s.setProperty('width', '1px', '\\uD800')", "s.getPropertyValue('\\uD800')", "s.getPropertyPriority('\\uD800')", "s.removeProperty('\\uD800')",
        "s.setProperty('--a', '\\uD800')", "s.setProperty('\\uD800', '')", "a.style = '\\uD800'", "s.color = {toString: function () { return 'x\\uD800y'; }}",
    ] {
        let r = js(&mut b, src);
        assert!(r.starts_with("unsupported:"), "{src}: {r}");
        assert_eq!(attr(&mut b), "color: red;", "{src}");
    }
    // A valid pair is stored and read back.
    js(&mut b, "s.setProperty('--e', '\\uD83D\\uDE00')");
    assert_eq!(js(&mut b, "s.getPropertyValue('--e').length + ',' + s.getPropertyValue('--e').charCodeAt(0) + ',' + s.getPropertyValue('--e').charCodeAt(1)"), "2,55357,56832");
}

/// Every shorthand the engine supports, with values of each shape it reads: every longhand of it is readable, as text that
/// the engine reads back to the same value, and nothing is `Unsupported`.
#[test]
fn every_shorthand_splits_into_readable_longhands() {
    use bare_css::values::{Property, shorthand_longhands};
    big(|| {
        let samples: &[(&str, &[&str])] = &[
            ("margin", &["1px", "1px 2px", "1px 2px 3px", "1px 2px 3px 4px", "auto 1em", "inherit", "calc(1px + 2%) 0", "-1cm 2pt"]),
            ("padding", &["0", "1px 2px", "5%"]),
            ("inset", &["0", "1px auto", "auto 1px 2px 3px"]),
            ("flex", &["1", "none", "auto", "2 3", "1 1 10px", "0 0 auto", "1 50%", "initial"]),
            ("flex-flow", &["row", "column wrap", "wrap-reverse", "row-reverse nowrap"]),
            ("gap", &["1px", "1px 2px", "normal"]),
            ("border-spacing", &["1px", "1px 2px"]),
            ("border-width", &["thin", "1px 2px", "thin medium thick 4px"]),
            ("border-style", &["solid", "solid dotted", "none dashed double groove"]),
            ("border-color", &["red", "red blue", "#f80 rgb(1, 2, 3) currentcolor transparent"]),
            ("border", &["1px solid red", "solid", "red", "0", "2px", "thick dotted", "none", "inherit"]),
            ("border-top", &["1px solid red", "solid", "thin"]),
            ("border-left", &["dashed blue"]),
            ("background", &["red", "url(x.png) red no-repeat", "none", "transparent"]),
            ("font", &["12px Arial", "italic bold 12px/1.5 'A B', serif", "12px/2 x", "inherit"]),
            ("list-style", &["square", "none", "disc"]),
            ("text-decoration", &["underline", "none", "underline red", "line-through dotted"]),
            ("overflow", &["hidden", "hidden auto", "scroll visible"]),
            ("grid-row", &["1", "2 / 4", "span 2", "2 / span 3", "a", "a / b", "-1 / auto"]),
            ("grid-column", &["1", "2 / 4", "span 2"]),
            ("grid-area", &["a", "1", "1 / 2", "1 / 2 / 3", "1 / 2 / 3 / 4", "a / b / c / d"]),
            ("place-items", &["center", "start end", "stretch"]),
            ("place-self", &["center", "end start"]),
            ("place-content", &["center", "start end", "space-between"]),
        ];
        let mut b = at(PAGE);
        js(&mut b, A);
        let mut seen = 0;
        for &(name, values) in samples {
            let longs = shorthand_longhands(name).unwrap_or_else(|| panic!("{name}"));
            for v in values {
                js(&mut b, "s.cssText = ''");
                let r = js(&mut b, &format!("s.setProperty({name:?}, {v:?}); s.length"));
                assert_eq!(r, longs.len().to_string(), "{name}: {v}");
                for p in longs {
                    let r = js(&mut b, &format!("s.getPropertyValue({:?})", p.name()));
                    assert!(!r.is_empty() && !r.starts_with("unsupported") && !r.starts_with("limit"), "{name}: {v} -> {} = [{r}]", p.name());
                    // The text reads back to a declaration of that one longhand.
                    js(&mut b, &format!("p.style.cssText = ''; p.style.setProperty({:?}, {r:?})", p.name()));
                    let got = js(&mut b, &format!("p.style.length + p.style.getPropertyValue({:?})", p.name()));
                    assert_eq!(got, format!("1{r}"), "{name}: {v} -> {} = [{r}] does not read back", p.name());
                    seen += 1;
                }
                // A one-longhand change keeps the others and writes something the engine accepts.
                let first = longs[0].name();
                let r = js(&mut b, &format!("s.removeProperty({first:?}); s.length + '|' + s.cssText"));
                assert!(r.starts_with(&format!("{}|", longs.len() - 1)), "{name}: {v}: {r}");
                js(&mut b, "a.style.cssText = s.cssText");
                assert_eq!(js(&mut b, "s.length"), (longs.len() - 1).to_string(), "{name}: {v}: {r}");
                let _ = Property::ALL;
            }
        }
        assert!(seen > 300, "{seen}");
    });
}

#[test]
fn authored_overlap_and_importance() {
    let mut b = at(PAGE);
    js(&mut b, A);
    let get = |b: &mut Browser, src: &str| js(b, src);
    // An earlier !important beats a later normal declaration, per longhand.
    js(&mut b, "a.setAttribute('style', 'margin: 1px !important; margin-top: 2px')");
    assert_eq!(get(&mut b, "[s.marginTop, s.getPropertyPriority('margin-top'), s.length, s.margin].join()"), "1px,important,4,1px");
    js(&mut b, "a.setAttribute('style', 'margin-top: 1px !important; margin: 2px')");
    assert_eq!(get(&mut b, "[s.marginTop, s.getPropertyPriority('margin-top'), s.marginRight, s.getPropertyPriority('margin-right'), s.margin, s.length].join()"), "1px,important,2px,,,4");
    js(&mut b, "s.color = 'red'");
    assert_eq!(attr(&mut b), "margin-top: 1px !important; margin-right: 2px; margin-bottom: 2px; margin-left: 2px; color: red;");
    // The later shorthand takes all of an earlier one; a later longhand takes one back.
    js(&mut b, "a.setAttribute('style', 'border-top: 1px solid red; border: 2px dotted blue')");
    assert_eq!(get(&mut b, "[s.length, s.borderTopWidth, s.borderTop, s.border].join('|')"), "12|2px|2px dotted blue|2px dotted blue");
    js(&mut b, "a.setAttribute('style', 'border: 2px dotted blue; border-top: 1px solid red')");
    assert_eq!(get(&mut b, "[s.length, s.borderTop, s.borderRight, '[' + s.border + ']', s.borderWidth, s.borderStyle, s.borderColor].join('|')"), "12|1px solid red|2px dotted blue|[]|1px 2px 2px|solid dotted dotted|red blue blue");
    js(&mut b, "s.color = 'red'");
    // An untouched shorthand is written as authored; the longhands that remain of the other one are grouped by side.
    assert_eq!(attr(&mut b), "border-top: 1px solid red; border-right: 2px dotted blue; border-bottom: 2px dotted blue; border-left: 2px dotted blue; color: red;");
    js(&mut b, "a.setAttribute('style', 'flex: 1; flex-basis: 10px')");
    assert_eq!(get(&mut b, "[s.flexGrow, s.flexShrink, s.flexBasis, s.flex].join()"), "1,1,10px,1 1 10px");
    js(&mut b, "a.setAttribute('style', 'grid-row: 1 / 3; grid-row-end: span 2')");
    assert_eq!(get(&mut b, "[s.gridRowStart, s.gridRowEnd, '[' + s.gridRow + ']', s.item(0), s.item(1)].join()"), "1,span 2,[],grid-row-start,grid-row-end");
    js(&mut b, "s.color = 'red'");
    assert_eq!(attr(&mut b), "grid-row-start: 1; grid-row-end: span 2; color: red;");
    // A name with an escape is written back as an escape.
    js(&mut b, "a.setAttribute('style', 'color: red; --a\\\\20 b: 1')");
    assert_eq!(get(&mut b, "s.length + ',' + s.getPropertyValue('--a b')"), "2,1");
    js(&mut b, "s.height = '1px'");
    assert_eq!(attr(&mut b), "color: red; --a\\20 b: 1; height: 1px;");
    assert_eq!(get(&mut b, "s.length + ',' + s.getPropertyValue('--a b')"), "3,1");
}

/// A value that was not written is written from the typed value: equal to the standard's value as a number, but the
/// keyword the standard gives (`medium`, `normal`) is not kept. Pinned so that it stays a known, visible difference.
#[test]
fn implied_values_come_from_the_engine() {
    let mut b = at(PAGE);
    js(&mut b, A);
    js(&mut b, "s.border = 'solid'");
    assert_eq!(js(&mut b, "s.borderTopWidth + ',' + s.borderTopColor"), "3px,currentcolor");
    js(&mut b, "s.cssText = ''; s.font = '12px Arial'");
    assert_eq!(js(&mut b, "s.fontWeight + ',' + s.fontStyle + ',' + s.lineHeight"), "400,normal,normal");
}

#[test]
fn expanded_shorthand_getters_obey_the_output_byte_cap() {
    let mut b = at("<div id=a></div>");
    js(&mut b, A);
    let value = format!("calc(1px{})", " + 0px".repeat(80));
    let style = format!("margin: {value}; margin-left: 2px");
    js(&mut b, &format!("a.setAttribute('style', '{}')", style));
    b.set_style_limits(StyleLimits { max_bytes: style.len(), ..Default::default() });
    assert_eq!(js(&mut b, "s.getPropertyValue('margin')"), lim("style-bytes"));
    assert_eq!(js(&mut b, "a.getAttribute('style')"), style);
}

/// The kept parse (`Block`) is invisible: a run in which every call finds the attribute changed (a trailing space, so the text
/// differs and the call parses) gives the same answers as a run that keeps parses. Debug builds also compare each kept parse
/// with a fresh one (`style_read`).
#[test]
fn kept_parse_is_invisible() {
    const NAMES: &[&str] = &["width", "margin", "margin-top", "padding", "border", "border-width", "color", "--x", "flex", "overflow", "gap", "display", "Margin-Left"];
    const VALUES: &[&str] = &["1px", "2px 3px", "red", "0 auto", "1px solid red", "inherit", "3px 4px 5px 6px", "bogus", "1px; color: blue", "", "10", "1 1 0", "1px !important"];
    let run = |perturb: bool| {
        let mut b = at("<!doctype html><div id=a style='color: red; margin: 0'></div><div id=b></div>");
        js(&mut b, "var e = [document.getElementById('a'), document.getElementById('b')];");
        let (mut seed, mut log, mut kept) = (12345u64, String::new(), 0);
        let mut next = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        for _ in 0..600 {
            let (el, name, value) = (next(2), NAMES[next(NAMES.len())], VALUES[next(VALUES.len())]);
            let op = match next(9) {
                0 | 1 => format!("s.setProperty('{name}', '{value}', '{}')", ["", "important"][next(2)]),
                2 => format!("s['{}'] = '{value}'", name.replace(|c: char| c == '-', "").to_lowercase()),
                3 => format!("s.removeProperty('{name}')"),
                4 => format!("s.cssText = '{name}: {value}; {}: {};'", NAMES[next(NAMES.len())], VALUES[next(VALUES.len())]),
                5 => format!("x.setAttribute('style', '{name}: {value}')"),
                6 => format!("s.getPropertyValue('{name}') + s.getPropertyPriority('{name}')"),
                7 => "s.cssText + s.length + s.item(1)".to_string(),
                _ => format!("s.getPropertyValue('{name}')"),
            };
            let pre = if perturb { "x.setAttribute('style', (x.getAttribute('style') || '') + ' ');" } else { "" };
            let r = js(&mut b, &format!("var x = e[{el}], s = x.style; {pre} {op}"));
            log.push_str(&format!("{el} {op} => {r}\n"));
            kept = b.style_stats().hits;
        }
        log.push_str(&js(&mut b, "e[0].style.cssText + '|' + e[1].style.cssText"));
        (log, kept)
    };
    let (plain, kept) = run(false);
    let (parsed, none) = run(true);
    assert!(kept > 100 && none < kept / 2, "hits {kept}, with a changed text {none}");
    assert_eq!(plain, parsed);
}

#[test]
fn indexes_and_the_iterator_give_the_longhand_names() {
    let mut b = at(PAGE);
    // CSSOM: `CSSStyleDeclaration` has an indexed getter (`item`), so WebIDL gives it `[Symbol.iterator]` = `Array.prototype.values`.
    assert_eq!(js(&mut b, "var s = a.style; s.length + '|' + s[0] + '|' + s[1] + '|' + s[4] + '|' + typeof s[5]"), "5|color|margin-top|margin-left|undefined");
    assert_eq!(js(&mut b, "JSON.stringify(Array.from(s))"), r#"["color","margin-top","margin-right","margin-bottom","margin-left"]"#);
    assert_eq!(js(&mut b, "var o = []; for (var n of s) o.push(n); o.join(',')"), "color,margin-top,margin-right,margin-bottom,margin-left");
    assert_eq!(js(&mut b, "CSSStyleDeclaration.prototype[Symbol.iterator] === Array.prototype.values"), "true");
    assert_eq!(js(&mut b, "Object.keys(s).join(',') + '|' + (0 in s) + (5 in s)"), "0,1,2,3,4|truefalse");
    // The indexes are live: a write through the object, and a write of the attribute.
    assert_eq!(js(&mut b, "s.color = ''; s[0] + '|' + s.length"), "margin-top|4");
    assert_eq!(js(&mut b, "a.setAttribute('style', 'width: 1px'); s[0] + '|' + typeof s[1]"), "width|undefined");
    assert_eq!(js(&mut b, "a.removeAttribute('style'); s.length + '|' + typeof s[0]"), "0|undefined");
    // An inert document, as an HTML sanitizer makes it.
    assert_eq!(js(&mut b, "var d = document.implementation.createHTMLDocument(''); var e = d.createElement('div'); e.setAttribute('style', 'color: red'); [...e.style].join(',')"), "color");
    // Indexes are read only.
    assert_eq!(js(&mut b, "a.style.cssText = 'color: red'; a.style[0] = 'x'; a.style[0]"), "color");
}

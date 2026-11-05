//! Tests of `HTMLElement.dataset` and `DOMStringMap` (HTML Standard, "The dataset IDL attribute"; WebIDL "legacy platform
//! objects" with `[LegacyOverrideBuiltIns]`). Expectations are by reading the Standard, not from a Chrome run. The first test
//! is the failure seen on the live staging app: React's stylesheet ordering reads `dataset.precedence`.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head>\
<link rel=stylesheet href=a.css data-precedence=default><link rel=stylesheet href=b.css data-precedence=high>\
<link rel=stylesheet href=c.css><style data-precedence=low>p{}</style></head><body>\
<div id=d data-foo-bar=1 data-x=two class=c data-a-1=n data-=empty title=t></div><div id=e></div>\
<svg id=sv data-s-x=sx></svg><math id=mm data-m=1></math>\
</body></html>";

fn browser_with(html: &str, lim: BrowserLimits, steps: u64) -> Browser {
    Browser::new(tree::parse(html), lim, EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default(), 5_000_000)
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

fn ok(b: &mut Browser, src: &str, want: &str) {
    assert_eq!(js(b, src), want, "{src}");
}

/// React 19 (`ck` and `cT` in the minified react-dom): find the stylesheets with a precedence and compare them.
#[test]
fn react_stylesheet_ordering_reads_dataset_precedence() {
    big(|| {
        let mut b = browser();
        ok(
            &mut b,
            "var out = []; var l = document.querySelectorAll('link[rel=\"stylesheet\"][data-precedence]');\
             for (var i = 0; i < l.length; i++) out.push(l[i].dataset.precedence);\
             var s = document.querySelector('style[data-precedence]'); out.push(s.dataset.precedence);\
             out.push(String(document.querySelector('link:not([data-precedence])').dataset.precedence));\
             out.join()",
            "default,high,low,undefined",
        );
    });
}

#[test]
fn reads_convert_names_and_hide_other_attributes() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var ds = document.getElementById('d').dataset; [ds.fooBar, ds.x, ds['a-1'], ds[''], ds.id, ds.class, ds.title, ds.missing].join('|')", "1|two|n|empty||||");
        // `data-foo-bar` is `fooBar`, not `foo-bar`; the dashed name and the attribute name are not properties.
        ok(&mut b, "[ds['foo-bar'], ds['data-foo-bar'], ds.FooBar, ds.foobar, 'foo-bar' in ds, 'fooBar' in ds, 'x' in ds, 'id' in ds].join()", ",,,,false,true,true,false");
        // Attribute order, only `data-*`.
        ok(&mut b, "Object.keys(ds).join()", "fooBar,x,a-1,");
        ok(&mut b, "var seen = []; for (var k in ds) seen.push(k); seen.join()", "fooBar,x,a-1,");
        ok(&mut b, "Reflect.ownKeys(ds).join() + '|' + JSON.stringify(ds)", "fooBar,x,a-1,|{\"fooBar\":\"1\",\"x\":\"two\",\"a-1\":\"n\",\"\":\"empty\"}");
        ok(&mut b, "Object.entries(ds).map(function (e) { return e.join('='); }).join()", "fooBar=1,x=two,a-1=n,=empty");
        ok(&mut b, "var c = Object.assign({}, ds); c.fooBar + c.x", "1two");
        ok(&mut b, "var sp = { ...ds }; Object.keys(sp).length", "4");
        ok(&mut b, "document.getElementById('e').dataset.x + '|' + Object.keys(document.getElementById('e').dataset).length", "undefined|0");
    });
}

#[test]
fn property_descriptors_and_has_own() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var ds = document.getElementById('d').dataset; JSON.stringify(Object.getOwnPropertyDescriptor(ds, 'x'))", "{\"value\":\"two\",\"writable\":true,\"enumerable\":true,\"configurable\":true}");
        ok(&mut b, "String(Object.getOwnPropertyDescriptor(ds, 'nope'))", "undefined");
        ok(&mut b, "[Object.prototype.hasOwnProperty.call(ds, 'x'), Object.hasOwn(ds, 'fooBar'), Object.hasOwn(ds, 'zz'), Reflect.has(ds, 'x'), ds.propertyIsEnumerable('x')].join()", "true,true,false,true,true");
        // `[LegacyOverrideBuiltIns]`: a named property hides a property of the prototype chain.
        ok(&mut b, "var e = document.getElementById('e'); e.setAttribute('data-to-string', 'v'); e.dataset.toString + '|' + String(Object.prototype.toString.call(e.dataset))", "v|[object DOMStringMap]");
        ok(&mut b, "e.removeAttribute('data-to-string'); typeof e.dataset.toString", "function");
    });
}

#[test]
fn writes_set_the_attribute_with_the_converted_name() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var d = document.getElementById('d'); var ds = d.dataset; ds.fooBar = 'changed'; ds.newName = 5; ds.nu = null; ds.un = undefined; ds.Cap = 'C'; ds.o = { toString: function () { return 'obj'; } }; ds['a.b'] = 'dot'; 1", "1");
        ok(&mut b, "[d.getAttribute('data-foo-bar'), d.getAttribute('data-new-name'), d.getAttribute('data-nu'), d.getAttribute('data-un'), d.getAttribute('data--cap'), d.getAttribute('data-o'), d.getAttribute('data-a.b')].join()", "changed,5,null,undefined,C,obj,dot");
        // New attributes go at the end; an existing one keeps its place.
        ok(&mut b, "d.getAttributeNames().join()", "id,data-foo-bar,data-x,class,data-a-1,data-,title,data-new-name,data-nu,data-un,data--cap,data-o,data-a.b");
        ok(&mut b, "ds.newName + ds.Cap + ds['a.b']", "5Cdot");
        ok(&mut b, "Object.keys(ds).join()", "fooBar,x,a-1,,newName,nu,un,Cap,o,a.b");
        // A key that is an array index is still a name: `ds[7]` is `data-7`.
        ok(&mut b, "ds[7] = 'seven'; [d.getAttribute('data-7'), ds[7], ds['7'], 7 in ds, Object.keys(ds).indexOf('7') >= 0, delete ds[7], d.hasAttribute('data-7')].join()", "seven,seven,seven,true,true,true,false");
        // A write is visible through the attribute and the other way round, at once.
        ok(&mut b, "d.setAttribute('data-live', 'yes'); ds.live + '|' + (ds.fooBar = 'z') + '|' + d.getAttribute('data-foo-bar')", "yes|z|z");
        // `Object.assign` and `Reflect.set` use the same setter.
        ok(&mut b, "Object.assign(ds, { p: 1, q: 2 }); Reflect.set(ds, 'r', 3); d.getAttribute('data-p') + d.getAttribute('data-q') + d.getAttribute('data-r')", "123");
        ok(&mut b, "Reflect.set(ds, 'sym', 1) + '|' + d.getAttribute('data-sym')", "true|1");
    });
}

#[test]
fn invalid_names_throw_before_any_change() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var d = document.getElementById('e'); var ds = d.dataset; var n0 = d.attributes.length; 1", "1");
        // A `-` followed by an ASCII lowercase letter: SyntaxError (the conversion would not round-trip).
        ok(&mut b, "try { ds['foo-bar'] = 1; 'no' } catch (e) { e instanceof DOMException ? e.name + ':' + e.code : 'other:' + e }", "SyntaxError:12");
        ok(&mut b, "try { ds['-a'] = 1; 'no' } catch (e) { e.name }", "SyntaxError");
        // A name that is not an XML Name once `data-` is added: InvalidCharacterError.
        ok(&mut b, "try { ds['a b'] = 1; 'no' } catch (e) { e.name + ':' + e.code }", "InvalidCharacterError:5");
        ok(&mut b, "try { ds['a='] = 1; 'no' } catch (e) { e.name }", "InvalidCharacterError");
        ok(&mut b, "try { ds['a!'] = 1; 'no' } catch (e) { e.name }", "InvalidCharacterError");
        ok(&mut b, "try { ds['\\u0000'] = 1; 'no' } catch (e) { e.name }", "InvalidCharacterError");
        // `-` followed by a digit, an uppercase letter or `-` is fine (no lowercase letter follows).
        ok(&mut b, "ds['a-1'] = 'x'; ds['a-'] = 'y'; ds['a--'] = 'z'; d.getAttribute('data-a-1') + d.getAttribute('data-a-') + d.getAttribute('data-a--')", "xyz");
        ok(&mut b, "d.attributes.length - n0", "3");
        // The value is converted first: a throwing `toString` wins over a bad name.
        ok(&mut b, "try { ds['a b'] = { toString: function () { throw new RangeError('first'); } }; 'no' } catch (e) { e.name }", "RangeError");
        // Defining goes through the same checks.
        ok(&mut b, "try { Object.defineProperty(ds, 'x-y', { value: 1 }); 'no' } catch (e) { e.name }", "SyntaxError");
        ok(&mut b, "Reflect.defineProperty(ds, 'dp', { value: 'v' }) + d.getAttribute('data-dp')", "truev");
        ok(&mut b, "try { Object.defineProperty(ds, 'acc', { get: function () { return 1; } }); 'no' } catch (e) { e.name }", "TypeError");
        ok(&mut b, "Reflect.defineProperty(ds, 'acc', { get: function () {} }) + '|' + d.hasAttribute('data-acc')", "false|false");
        // A lone surrogate cannot be stored in the UTF-8 document: a gate, never a catchable error and never a lossy copy.
        let r = js(&mut b, "try { ds.u = '\\ud800'; 'stored' } catch (e) { 'caught' }");
        assert!(r.starts_with("unsupported:lone surrogate"), "{r}");
        ok(&mut b, "d.hasAttribute('data-u')", "false");
    });
}

#[test]
fn delete_removes_the_attribute() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var d = document.getElementById('d'); var ds = d.dataset; [delete ds.fooBar, d.hasAttribute('data-foo-bar'), 'fooBar' in ds, delete ds.nothing, delete ds.id, d.hasAttribute('id'), d.getAttribute('title')].join()", "true,false,false,true,true,true,t");
        ok(&mut b, "Object.keys(ds).join()", "x,a-1,");
        ok(&mut b, "Reflect.deleteProperty(ds, 'x') + '|' + d.getAttribute('data-x')", "true|null");
        ok(&mut b, "(function () { 'use strict'; return delete ds['a-1']; })()", "true");
        ok(&mut b, "d.hasAttribute('data-a-1')", "false");
        // The dashed form is not a property, so deleting it keeps `data-` + the dashed name only if it is supported.
        ok(&mut b, "d.setAttribute('data-keep-me', '1'); delete ds['keep-me']; d.hasAttribute('data-keep-me')", "true");
        ok(&mut b, "delete ds.keepMe; d.hasAttribute('data-keep-me')", "false");
    });
}

#[test]
fn same_object_brand_and_illegal_use() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var d = document.getElementById('d'); [d.dataset === d.dataset, d.dataset === document.getElementById('e').dataset, d.dataset instanceof DOMStringMap, typeof DOMStringMap, Object.prototype.toString.call(d.dataset)].join()", "true,false,true,function,[object DOMStringMap]");
        ok(&mut b, "try { new DOMStringMap(); 'no' } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal constructor");
        ok(&mut b, "var g = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'dataset'); [typeof g.get, g.set, g.enumerable, g.configurable].join()", "function,,true,true");
        ok(&mut b, "try { g.get.call({}); 'no' } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal invocation");
        ok(&mut b, "try { g.get.call(document.createTextNode('t')); 'no' } catch (e) { e.name }", "TypeError");
        ok(&mut b, "Element.prototype.hasOwnProperty('dataset') + '|' + document.hasOwnProperty('dataset') + '|' + ('dataset' in document)", "false|false|false");
        // `[PutForwards]` is not on `dataset`: the attribute is read only, and a write is ignored (sloppy).
        ok(&mut b, "var before = d.dataset; d.dataset = { x: 1 }; d.dataset === before", "true");
        // A map that the page holds keeps its element, and `Object.prototype` behaviour stays.
        ok(&mut b, "var ds = d.dataset; Object.getPrototypeOf(ds) === DOMStringMap.prototype && Object.getPrototypeOf(DOMStringMap.prototype) === Object.prototype", "true");
        ok(&mut b, "Object.isExtensible(ds)", "true");
    });
}

#[test]
fn svg_has_dataset_and_mathml_does_not() {
    big(|| {
        let mut b = browser();
        // `setAttribute` keeps the case on an SVG element: `data-Up` has an ASCII upper alpha, so it is not in the map.
        ok(&mut b, "var sv = document.getElementById('sv'); sv.setAttribute('data-Up', 'u'); Object.keys(sv.dataset).join() + '|' + sv.dataset.sX + '|' + sv.dataset.up + '|' + sv.getAttribute('data-Up')", "sX|sx|undefined|u");
        ok(&mut b, "sv.dataset.add = 'a'; sv.getAttribute('data-add') + (sv.dataset === sv.dataset)", "atrue");
        ok(&mut b, "SVGElement.prototype.hasOwnProperty('dataset')", "true");
        // Gate: MathML elements are plain `Element`s (no `MathMLElement` brand), so they have no `dataset`.
        ok(&mut b, "String(document.getElementById('mm').dataset)", "undefined");
    });
}

#[test]
fn inheriting_from_a_map_reads_through_and_writes_stay_ordinary() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var d = document.getElementById('d'); var c = Object.create(d.dataset); [c.x, 'x' in c, Object.keys(c).length].join()", "two,true,0");
        // OrdinarySet with another receiver: the child gets an own property, the attribute is untouched.
        ok(&mut b, "c.x = 'mine'; [c.x, d.getAttribute('data-x'), Object.keys(c).join()].join()", "mine,two,x");
        ok(&mut b, "c.y = 'new'; d.hasAttribute('data-y')", "false");
    });
}

#[test]
fn gc_keeps_held_maps_and_prunes_dropped_ones() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var hold = document.createElement('p'); hold.setAttribute('data-k', 'v'); var onlyMap = hold.dataset; hold = null; 1", "1");
        b.collect(&[]).expect("collect");
        // The map keeps its (detached) element alive, and reads its attributes.
        ok(&mut b, "onlyMap.k + '|' + (onlyMap.k2 = 'w') + '|' + Object.keys(onlyMap).join()", "v|w|k,k2");
        let held = b.dataset_stats().objects;
        ok(&mut b, "for (var i = 0; i < 300; i++) { var p = document.createElement('i'); p.dataset.n = i; } p = null; 1", "1");
        assert!(b.dataset_stats().objects >= held + 300);
        b.collect(&[]).expect("collect");
        assert_eq!(b.dataset_stats().objects, held);
        ok(&mut b, "onlyMap.k", "v");
        // An attached element keeps its map while the wrapper lives (SameObject).
        ok(&mut b, "var d = document.getElementById('d'); var m1 = d.dataset; 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "m1 === d.dataset", "true");
        ok(&mut b, "m1 = null; 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "d.dataset.fooBar", "1");
    });
}

#[test]
fn a_map_is_not_plain_data() {
    big(|| {
        let mut b = browser();
        b.set_document_url("https://example.test/").expect("url");
        ok(&mut b, "try { history.replaceState(document.getElementById('d').dataset, ''); 'cloned' } catch (e) { e.name }", "DataCloneError");
    });
}

#[test]
fn caps_are_host_limits_not_catchable_errors() {
    big(|| {
        let lim = BrowserLimits { max_attr_slots: 40, ..Default::default() };
        let mut b = browser_with("<!doctype html><body><p id=p></p></body>", lim, 5_000_000);
        let r = b.run_script("var ds = document.getElementById('p').dataset; try { for (var i = 0; i < 100; i++) ds['k' + i] = 'v'; 'done' } catch (e) { 'caught' }");
        assert!(matches!(r, Err(Error::Limit(_))), "{:?}", show(&mut b, r));
        let lim = BrowserLimits { max_wrappers: 8, ..Default::default() };
        let mut b = browser_with("<!doctype html><body><p id=p></p></body>", lim, 5_000_000);
        let r = b.run_script("var all = []; for (var i = 0; i < 50; i++) { var x = document.createElement('i'); all.push(x.dataset); } 1");
        assert!(matches!(r, Err(Error::Limit(_))), "{:?}", show(&mut b, r));
    });
}

#[test]
fn step_budget_bounds_key_listing() {
    big(|| {
        let mut html = String::from("<!doctype html><body><p id=p");
        for i in 0..2000 {
            html.push_str(&format!(" data-a{i}=v"));
        }
        html.push_str("></p></body>");
        let mut b = browser_with(&html, BrowserLimits::default(), 60_000);
        let r = b.run_script("var ds = document.getElementById('p').dataset; var n = 0; for (var i = 0; i < 1000; i++) n += Object.keys(ds).length; n");
        assert!(matches!(r, Err(Error::Limit(_))), "{:?}", show(&mut b, r));
    });
}

#[test]
fn long_attribute_lists_work_and_are_charged() {
    big(|| {
        let mut html = String::from("<!doctype html><body><p id=p class=x");
        for i in 0..100 {
            html.push_str(&format!(" data-a{i}=v{i}"));
        }
        html.push_str("></p></body>");
        let mut b = browser_with(&html, BrowserLimits::default(), 5_000_000);
        // More than 32 attributes: the heap asks the evaluator to charge, then the read repeats.
        ok(&mut b, "var ds = document.getElementById('p').dataset; [ds.a0, ds.a99, ds.a100, 'a50' in ds, 'a100' in ds, Object.keys(ds).length, Object.keys(ds)[99], delete ds.a5, Object.keys(ds).length, (ds.zz = 1, ds.zz)].join()", "v0,v99,,true,false,100,a99,true,99,1");
        let steps = |n: usize| {
            let mut html = String::from("<!doctype html><body><p id=p");
            for i in 0..n {
                html.push_str(&format!(" data-a{i}=v"));
            }
            html.push_str("></p></body>");
            let mut b = browser_with(&html, BrowserLimits::default(), 5_000_000);
            ok(&mut b, "var ds = document.getElementById('p').dataset; ds.a1; ds.a2; ds.a3; 1", "1");
            b.it.stats().steps
        };
        let (short, long) = (steps(10), steps(100));
        assert!(long >= short + 3 * 12, "a read of 100 attributes costs steps: long {long}, short {short}");
        let mut b = browser_with(&html, BrowserLimits::default(), 5_000);
        let r = b.run_script("var ds = document.getElementById('p').dataset; var n = 0; for (var i = 0; i < 100000; i++) n += ds.a1.length; n");
        assert!(matches!(r, Err(Error::Limit(_))), "{:?}", show(&mut b, r));
    });
}

#[test]
fn freezing_a_map_with_names_is_refused() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "try { Object.freeze(document.getElementById('d').dataset); 'frozen' } catch (e) { e.name }", "TypeError");
        ok(&mut b, "document.getElementById('d').dataset.x = 'still writable'; document.getElementById('d').dataset.x", "still writable");
    });
}

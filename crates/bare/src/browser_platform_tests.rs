//! Tests of `browser_platform`: matchMedia, classList, getElementsBy*. Real scripts in a real realm. The theme script
//! is the first inline script of the saved staging login page, found by walking the parsed page (no copy of it).
//! Expectations are written by hand from the DOM, HTML and CSSOM View standards; nothing was compared with Chrome.

use crate::browser::*;
use crate::browser_platform::PlatformLimits;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const LOGIN: &str = include_str!("../../../tools/fixtures/staging/login.html");
const ORIGIN: &str = "https://staging.app.8vox.ai";
const PAGE: &str = "<!doctype html><html><head></head><body><div id=a class='x y'></div><p id=b></p></body></html>";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn first_inline_script(doc: &Document) -> String {
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if let NodeData::Element { name, .. } = *doc.data(n)
            && doc.atom_str(name) == "script"
            && doc.find_atom("src").is_none_or(|a| doc.attr(n, a).is_none())
        {
            return doc.children(n).filter_map(|c| if let NodeData::Text(sp) = *doc.data(c) { Some(doc.text(sp).to_string()) } else { None }).collect();
        }
        let kids: Vec<NodeId> = doc.children(n).collect();
        stack.extend(kids.into_iter().rev());
    }
    panic!("no inline script")
}

fn mk(page: &str, steps: u64) -> Browser {
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_document_url(&format!("{ORIGIN}/login")).unwrap();
    b
}

fn at(page: &str) -> Browser {
    mk(page, 5_000_000)
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

// ---------------------------------------------------------------------------------------------- the real script

#[test]
fn theme_script_is_found_in_the_saved_page() {
    // The script is read from the saved page, not copied. Before this module it died with "matchMedia is not defined".
    let doc = tree::parse(LOGIN);
    assert!(first_inline_script(&doc).starts_with("(function(){var m=matchMedia(\"(prefers-color-scheme: dark)\")"));
}

#[test]
fn saved_theme_script_runs_and_follows_local_storage() {
    big(|| {
        let doc = tree::parse(LOGIN);
        let script = first_inline_script(&doc);
        assert!(script.contains("classList.toggle(\"dark\"") && script.contains("m.addEventListener(\"change\",a)"));
        let mut b = Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.set_document_url("https://staging.app.8vox.ai/login").unwrap();
        assert_eq!(b.media_environment().prefers_color_scheme, "light");
        let r = b.run_script(&script);
        assert_eq!(show(&mut b, r), "undefined");
        // Light, no saved theme: no `dark` class. The class attribute of <html> is what the page had.
        assert_eq!(b.query("html.dark").unwrap(), None);
        assert_eq!(js(&mut b, "typeof window.__applyTheme"), "function");
        assert_eq!(b.platform_stats().rooted_media_lists, 1, "the change listener roots its list");
        // Synthetic saved themes, through the real localStorage of the origin.
        let store = b.storage();
        store.set_item(false, ORIGIN, "theme", "dark").unwrap();
        assert_eq!(js(&mut b, "window.__applyTheme(); document.documentElement.classList.contains('dark')"), "true");
        let html = b.query("html.dark").unwrap().expect("selector sees the class");
        let doc = b.document();
        let NodeData::Element { name, .. } = *doc.borrow().data(html) else { panic!("not an element") };
        assert_eq!(doc.borrow().atom_str(name), "html");
        store.set_item(false, ORIGIN, "theme", "light").unwrap();
        js(&mut b, "window.__applyTheme()");
        assert_eq!(b.query("html.dark").unwrap(), None);
        store.set_item(false, ORIGIN, "theme", "dark").unwrap();
        js(&mut b, "window.__applyTheme()");
        assert!(b.query("html.dark").unwrap().is_some());
        store.set_item(false, ORIGIN, "theme", "other").unwrap();
        js(&mut b, "window.__applyTheme()");
        assert_eq!(b.query("html.dark").unwrap(), None, "any other value follows the (light) media query");
        // The page's own class is kept.
        let html = b.query("html").unwrap().unwrap();
        let cls = b.document().borrow().attr(html, Atom::CLASS).map(String::from);
        assert!(cls.is_none_or(|c| !c.contains("dark")));
    });
}

#[test]
fn theme_script_reacts_to_a_viewport_change_through_its_media_list() {
    big(|| {
        let mut b = at(PAGE);
        // Same shape as the page script, with a query that a viewport change can flip.
        js(&mut b, "var m=matchMedia('(max-width: 700px)'); function a(){document.documentElement.classList.toggle('narrow', m.matches)} a(); m.addEventListener('change',a);");
        assert_eq!(b.query("html.narrow").unwrap(), None);
        assert_eq!(b.set_viewport(600.0, 800.0).unwrap(), 1);
        assert!(b.query("html.narrow").unwrap().is_some());
        assert_eq!(b.set_viewport(1000.0, 800.0).unwrap(), 1);
        assert_eq!(b.query("html.narrow").unwrap(), None);
    });
}

// ---------------------------------------------------------------------------------------------- matchMedia

#[test]
fn match_media_object() {
    big(|| {
        let mut b = at(PAGE);
        assert_eq!(b.media_environment().viewport, (1280.0, 800.0));
        assert_eq!(js(&mut b, "var m=matchMedia('  (min-width: 1000px) '); m.media"), "(min-width: 1000px)");
        assert_eq!(js(&mut b, "m.matches"), "true");
        assert_eq!(js(&mut b, "matchMedia('(max-width: 800px)').matches"), "false");
        assert_eq!(js(&mut b, "matchMedia('(prefers-color-scheme: dark)').matches"), "false");
        assert_eq!(js(&mut b, "matchMedia('(prefers-color-scheme: light)').matches"), "true");
        assert_eq!(js(&mut b, "matchMedia('').matches + ',' + matchMedia('').media.length"), "true,0");
        assert_eq!(js(&mut b, "matchMedia('not-a-query-at-all(').matches"), "false");
        assert_eq!(js(&mut b, "[m instanceof MediaQueryList, m instanceof EventTarget, window.matchMedia === matchMedia, typeof m.addEventListener, typeof m.addListener, typeof m.onchange].join()"), "true,true,true,function,function,object");
        assert_eq!(js(&mut b, "window.matchMedia('(min-width:1px)') !== window.matchMedia('(min-width:1px)')"), "true");
        assert_eq!(js(&mut b, "m.matches = false; m.matches"), "true", "matches is read-only");
        assert_eq!(js(&mut b, "matchMedia()"), "throw:TypeError:Failed to execute 'matchMedia' on 'Window': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "MediaQueryList.prototype.matches"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "new MediaQueryList()"), "throw:TypeError:MediaQueryList is not a constructor");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(MediaQueryList.prototype,'matches').get.call({})"), "throw:TypeError:Illegal invocation");
        // The query is coerced with ToString.
        assert_eq!(js(&mut b, "matchMedia({toString(){return '(min-width: 1px)'}}).matches"), "true");
    });
}

#[test]
fn match_media_caps() {
    big(|| {
        let mut b = at(PAGE);
        b.set_platform_limits(PlatformLimits { max_media_lists: 3, max_media_query_bytes: 20, ..Default::default() });
        assert_eq!(js(&mut b, "matchMedia('(min-width: 100px)').matches"), "true");
        assert_eq!(js(&mut b, &format!("matchMedia('{}')", "a".repeat(21))), lim("media-query"));
        js(&mut b, "matchMedia('all'); matchMedia('all')");
        assert_eq!(js(&mut b, "matchMedia('all')"), lim("media-lists"));
        // GC frees them: unreachable lists with no listener are pruned.
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().media_lists, 0);
        assert_eq!(js(&mut b, "matchMedia('all').matches"), "true");
        b.collect(&[]).unwrap();
        // Listeners root their lists, so the cap counts them for good.
        b.collect(&[]).unwrap();
        js(&mut b, "for (var i=0;i<3;i++) matchMedia('all').addEventListener('change', function(){})");
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().rooted_media_lists, 3);
        assert_eq!(js(&mut b, "matchMedia('all')"), lim("media-lists"));
        // A string argument over the byte cap of the realm string is refused before any copy.
        let mut small = Browser::new(tree::parse(PAGE), BrowserLimits { max_string_bytes: 100, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).unwrap();
        assert_eq!(js(&mut small, &format!("matchMedia('{}')", "a".repeat(200))), lim("dom-string"));
    });
}

#[test]
fn media_change_events_and_listener_semantics() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var log=[]; var m=matchMedia('(max-width: 700px)'); var other=matchMedia('(min-width: 100px)');
            m.addEventListener('change', function(e){ log.push('a:'+e.type+':'+e.matches+':'+e.media+':'+(e.target===m)+':'+(e.currentTarget===m)+':'+(this===m)+':'+(e instanceof Event)+':'+e.bubbles+':'+e.isTrusted) });
            m.onchange = function(e){ log.push('on:'+e.matches) };
            m.addListener(function(e){ log.push('legacy') });
            m.addEventListener('change', {handleEvent(e){ log.push('obj') }});
            m.addEventListener('change', function(){ log.push('once') }, {once:true});
            m.addEventListener('change', function(){ throw new Error('boom') });
            m.addEventListener('change', function(){ log.push('after-throw') });
            other.addEventListener('change', function(){ log.push('other') });");
        assert_eq!(b.set_viewport(1280.0, 800.0).unwrap(), 0, "nothing changed");
        assert_eq!(js(&mut b, "log.length"), "0");
        assert_eq!(b.set_viewport(600.0, 800.0).unwrap(), 1);
        assert_eq!(js(&mut b, "log.join('|')"), "a:change:true:(max-width: 700px):true:true:true:true:false:false|on:true|legacy|obj|once|after-throw");
        let reports = b.take_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].event_type, "change");
        assert_eq!(reports[0].target, "object");
        assert_eq!(reports[0].message, "Error: boom");
        js(&mut b, "log.length = 0");
        assert_eq!(b.set_viewport(600.0, 900.0).unwrap(), 0, "same matches: no event");
        // Back: `once` is gone, the others fire; `other` flipped to false at width 50.
        assert_eq!(b.set_viewport(50.0, 900.0).unwrap(), 1, "only `other` changed (m stays true)");
        assert_eq!(js(&mut b, "log.join('|')"), "other");
        assert_eq!(b.set_viewport(1000.0, 900.0).unwrap(), 2, "both flip back, oldest first");
        assert_eq!(js(&mut b, "log.slice(1).join('|')"), "a:change:false:(max-width: 700px):true:true:true:true:false:false|on:false|legacy|obj|after-throw|other");
        // removeListener / removeEventListener / onchange = null.
        js(&mut b, "log.length=0; var f=function(){log.push('f')}; m.addListener(f); m.removeListener(f); m.onchange=null; m.addListener(null);");
        assert_eq!(js(&mut b, "m.onchange"), "null");
        b.set_viewport(100.0, 900.0).unwrap();
        assert_eq!(js(&mut b, "log.join('|')").matches("on:").count(), 0);
        assert_eq!(js(&mut b, "log.includes('f')"), "false");
        assert_eq!(js(&mut b, "typeof (m.onchange = function(){}, m.onchange)"), "function");
        assert_eq!(js(&mut b, "m.onchange = 5; m.onchange"), "null", "a non-function clears the handler");
        assert_eq!(js(&mut b, "m.addListener(5)"), "throw:TypeError:Failed to execute 'addEventListener': parameter 2 is not of type 'Object'.");
    });
}

#[test]
fn onchange_keeps_its_place_when_replaced() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var log=[]; var m=matchMedia('(max-width: 700px)'); m.addEventListener('change', function(){log.push(1)}); m.onchange=function(){log.push('h1')}; m.addEventListener('change', function(){log.push(3)}); m.onchange=function(){log.push('h2')};");
        assert_eq!(b.platform_stats().rooted_media_lists, 1);
        b.set_viewport(10.0, 10.0).unwrap();
        assert_eq!(js(&mut b, "log.join()"), "1,h2,3");
        js(&mut b, "m.onchange=null; log.length=0");
        b.set_viewport(2000.0, 10.0).unwrap();
        assert_eq!(js(&mut b, "log.join()"), "1,3");
    });
}

#[test]
fn media_checkpoints_and_cross_script_callbacks() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var log=[]; var m=matchMedia('(max-width: 700px)');
            m.addEventListener('change', function(){ log.push('a'); Promise.resolve().then(function(){ log.push('micro-a') }) });
            m.addEventListener('change', function(){ log.push('b') });");
        // A second script adds a listener that reads state written by the first.
        js(&mut b, "m.addEventListener('change', function(e){ log.push('c:' + log.length + ':' + m.matches) }); window.addEventListener('x', function(){});");
        b.set_viewport(300.0, 300.0).unwrap();
        assert_eq!(js(&mut b, "log.join('|')"), "a|micro-a|b|c:3:true", "a checkpoint runs after each listener");
    });
}

#[test]
fn fatal_error_halts_viewport_callbacks() {
    big(|| {
        let mut b = mk(PAGE, 200_000);
        js(&mut b, "var n=0; var m=matchMedia('(max-width: 700px)'); m.addEventListener('change', function(){ n++; while(true){} });");
        let r = b.set_viewport(300.0, 300.0);
        assert!(matches!(r, Err(Error::Limit(LimitKind::Steps))), "{r:?}");
        assert_eq!(js(&mut b, "n"), "1");
        let again = b.set_viewport(2000.0, 300.0);
        assert!(matches!(again, Err(Error::Limit(LimitKind::Steps))), "{again:?}");
        assert_eq!(js(&mut b, "n"), "1", "no JavaScript after the halt");
        // An invalid size is refused and is not fatal for a healthy browser.
        let mut ok = at(PAGE);
        assert!(matches!(ok.set_viewport(f32::NAN, 1.0), Err(Error::Unsupported(_))));
        assert!(matches!(ok.set_viewport(-1.0, 1.0), Err(Error::Unsupported(_))));
        assert_eq!(ok.media_environment().viewport, (1280.0, 800.0));
        assert!(ok.set_viewport(500.0, 500.0).is_ok());
        assert_eq!(ok.media_environment().viewport, (500.0, 500.0));
    });
}

#[test]
fn media_lists_in_gc() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var log=[]; (function(){ var m=matchMedia('(max-width: 700px)'); m.addEventListener('change', function(){ log.push('rooted') }); })();");
        js(&mut b, "for (var i=0;i<10;i++) matchMedia('(max-width: '+(100+i)+'px)');");
        assert_eq!(b.platform_stats().media_lists, 11);
        js(&mut b, "0"); // the last script value is kept until the next script runs
        b.collect(&[]).unwrap();
        let s = b.platform_stats();
        assert_eq!((s.media_lists, s.rooted_media_lists), (1, 1), "unreferenced list without listeners is pruned, the rooted one stays");
        // Slot reuse: new lists take the freed object slots and must not inherit anything.
        js(&mut b, "var fresh=[]; for (var i=0;i<20;i++) fresh.push(matchMedia('(min-width: '+(i*100)+'px)'));");
        assert_eq!(js(&mut b, "fresh.map(function(x){return x.matches?1:0}).join('')"), "11111111111110000000");
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().media_lists, 21);
        js(&mut b, "fresh = null");
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().media_lists, 1);
        assert_eq!(b.set_viewport(300.0, 300.0).unwrap(), 1);
        assert_eq!(js(&mut b, "log.join()"), "rooted");
        // Drop the last listener: the list is freed at the next collection.
        js(&mut b, "var q=matchMedia('(max-width: 700px)'); var f=function(){}; q.addEventListener('change', f); q.removeEventListener('change', f); q=null;");
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().media_lists, 1);
    });
}

#[test]
fn platform_objects_are_not_plain_data_for_clone() {
    big(|| {
        let mut b = at(PAGE);
        for (src, what) in [
            ("history.pushState(matchMedia('all'), '')", "mql"),
            ("history.pushState(document.body.classList, '')", "list"),
            ("history.pushState(document.body.classList.values(), '')", "iterator"),
            ("history.pushState({a: matchMedia('all')}, '')", "nested"),
        ] {
            assert_eq!(js(&mut b, src), "throw:DataCloneError:An object could not be cloned.", "{what}");
        }
    });
}

// ---------------------------------------------------------------------------------------------- classList

#[test]
fn class_list_basics_and_live_attribute() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var el=document.getElementById('a'); var cl=el.classList;");
        assert_eq!(js(&mut b, "[cl.length, cl.value, String(cl), cl.item(0), cl.item(1), cl.item(2), cl.contains('x'), cl.contains('z'), cl.contains('')].join('|')"), "2|x y|x y|x|y||true|false|false");
        assert_eq!(js(&mut b, "el.classList === el.classList && cl === el.classList"), "true");
        assert_eq!(js(&mut b, "cl instanceof DOMTokenList && Object.prototype.toString.call(cl) === '[object Object]'"), "true");
        // Live: every other way to change the attribute shows at once.
        js(&mut b, "el.setAttribute('class', '  b   a b  '); ");
        assert_eq!(js(&mut b, "cl.length + ':' + cl.value + ':' + cl.item(0) + cl.item(1)"), "2:  b   a b  :ba");
        js(&mut b, "el.className = 'q'");
        assert_eq!(js(&mut b, "cl.length + cl.item(0)"), "1q");
        js(&mut b, "el.removeAttribute('class')");
        assert_eq!(js(&mut b, "cl.length + ':' + cl.value + ':' + el.hasAttribute('class')"), "0::false");
        // The update steps write nothing for an empty set and no attribute.
        js(&mut b, "cl.remove('nothing'); cl.remove(); cl.add(); cl.toggle('t', false);");
        assert_eq!(js(&mut b, "el.hasAttribute('class')"), "false");
        // add/remove/toggle/replace change the attribute and normalise it.
        js(&mut b, "cl.add('a', 'b', 'a'); ");
        assert_eq!(js(&mut b, "el.getAttribute('class')"), "a b");
        js(&mut b, "el.setAttribute('class', 'a  b   a c'); cl.add('z')");
        assert_eq!(js(&mut b, "el.getAttribute('class')"), "a b c z", "duplicates collapse, first occurrence kept");
        js(&mut b, "cl.remove('b', 'nope')");
        assert_eq!(js(&mut b, "el.className"), "a c z");
        assert_eq!(js(&mut b, "cl.toggle('c') + ',' + cl.toggle('c') + ',' + cl.toggle('c', true) + ',' + cl.toggle('d', true) + ',' + cl.toggle('d', false) + ',' + cl.toggle('e', false) + ',' + cl.toggle('x', 1) + ',' + cl.toggle('x', 0)"), "false,true,true,true,false,false,true,false");
        assert_eq!(js(&mut b, "el.className"), "a z c");
        // The selector engine sees the native change at once (cache invalidation).
        let n = b.query("#a.c").unwrap();
        assert!(n.is_some());
        js(&mut b, "cl.remove('c')");
        assert_eq!(b.query("#a.c").unwrap(), None);
        assert!(b.query("#a.a.z").unwrap().is_some());
        // value setter and PutForwards.
        js(&mut b, "cl.value = 'm n'");
        assert_eq!(js(&mut b, "el.className + '|' + cl.length"), "m n|2");
        js(&mut b, "el.classList = 'p  q'");
        assert_eq!(js(&mut b, "el.className + '|' + (cl === el.classList)"), "p  q|true");
        // Text nodes have no classList; Element.prototype accessor refuses a wrong `this`.
        assert_eq!(js(&mut b, "document.createTextNode('t').classList"), "undefined");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(Element.prototype,'classList').get.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "DOMTokenList.prototype.add.call({}, 'x')"), "throw:TypeError:Illegal invocation");
    });
}

#[test]
fn class_list_long_attribute_cache() {
    big(|| {
        let mut b = at(PAGE);
        // 300 bytes: the native matcher builds a set for it (>= 128 bytes). Changes must invalidate it.
        js(&mut b, "var el=document.getElementById('b'); el.setAttribute('class', Array.from({length:60}, function(_, i){return 'c'+i}).join(' '));");
        assert!(b.query("#b.c59").unwrap().is_some());
        assert_eq!(js(&mut b, "el.classList.contains('c59')"), "true");
        js(&mut b, "el.classList.remove('c59'); el.classList.add('new')");
        assert_eq!(b.query("#b.c59").unwrap(), None);
        assert!(b.query("#b.new").unwrap().is_some());
        assert_eq!(js(&mut b, "el.classList.length"), "60");
        assert_eq!(js(&mut b, "el.classList.contains('c59') + ',' + el.classList.contains('new')"), "false,true");
    });
}

#[test]
fn class_list_token_validation_and_order() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var el=document.getElementById('a'); var cl=el.classList; var log=[]; function t(n,v){return {toString(){log.push(n); return v}}}");
        let e = |b: &mut Browser, s: &str| js(b, &format!("try {{ {s}; 'no-throw' }} catch(e) {{ e.name + ':' + e.code }}"));
        assert_eq!(e(&mut b, "cl.add('')"), "SyntaxError:12");
        assert_eq!(e(&mut b, "cl.add('a b')"), "InvalidCharacterError:5");
        assert_eq!(e(&mut b, "cl.add('a\\tb')"), "InvalidCharacterError:5");
        assert_eq!(e(&mut b, "cl.remove('')"), "SyntaxError:12");
        assert_eq!(e(&mut b, "cl.toggle('')"), "SyntaxError:12");
        assert_eq!(e(&mut b, "cl.toggle('a b')"), "InvalidCharacterError:5");
        // Atomic: a bad token anywhere leaves the attribute alone.
        assert_eq!(e(&mut b, "cl.add('ok1', 'bad token', 'ok2')"), "InvalidCharacterError:5");
        assert_eq!(js(&mut b, "el.className"), "x y");
        assert_eq!(e(&mut b, "cl.remove('x', '')"), "SyntaxError:12");
        assert_eq!(js(&mut b, "el.className"), "x y");
        // The first bad token in argument order decides the error name.
        assert_eq!(e(&mut b, "cl.add('a b', '')"), "InvalidCharacterError:5");
        assert_eq!(e(&mut b, "cl.add('', 'a b')"), "SyntaxError:12");
        // replace: both empty checks come before the white space checks.
        assert_eq!(e(&mut b, "cl.replace('a b', '')"), "SyntaxError:12");
        assert_eq!(e(&mut b, "cl.replace('', 'a b')"), "SyntaxError:12");
        assert_eq!(e(&mut b, "cl.replace('x', 'a b')"), "InvalidCharacterError:5");
        assert_eq!(e(&mut b, "cl.replace('x')"), "TypeError:undefined");
        assert_eq!(e(&mut b, "cl.item()"), "TypeError:undefined");
        assert_eq!(e(&mut b, "cl.contains()"), "TypeError:undefined");
        assert_eq!(e(&mut b, "cl.toggle()"), "TypeError:undefined");
        assert_eq!(e(&mut b, "cl.supports('x')"), "TypeError:undefined");
        // Every argument is converted (in order) before any validation or write; a throw in conversion aborts.
        assert_eq!(e(&mut b, "cl.add(t(1,'p'), '', t(2,'q'))"), "SyntaxError:12");
        assert_eq!(js(&mut b, "log.join()"), "1,2");
        assert_eq!(js(&mut b, "el.className"), "x y");
        assert_eq!(e(&mut b, "cl.add('p', {toString(){throw new RangeError('ts')}}, 'q')"), "RangeError:undefined");
        assert_eq!(js(&mut b, "el.className"), "x y");
        // Coercions: undefined, numbers, null become strings.
        js(&mut b, "cl.add(undefined, 5, null)");
        assert_eq!(js(&mut b, "el.className"), "x y undefined 5 null");
        // A toString that changes the attribute: the set is read after conversion.
        js(&mut b, "cl.add({toString(){ el.setAttribute('class', 'fresh'); return 'added' }})");
        assert_eq!(js(&mut b, "el.className"), "fresh added");
        // replace semantics.
        js(&mut b, "el.className = 'a b c'");
        assert_eq!(js(&mut b, "cl.replace('b','x') + ':' + el.className"), "true:a x c");
        assert_eq!(js(&mut b, "cl.replace('nope','z') + ':' + el.className"), "false:a x c");
        assert_eq!(js(&mut b, "cl.replace('c','a') + ':' + el.className"), "true:a x", "new token already present: first instance of either stays, others go");
        assert_eq!(js(&mut b, "cl.replace('x','a') + ':' + el.className"), "true:a");
        // item index conversion.
        js(&mut b, "el.className = 'a b'");
        assert_eq!(js(&mut b, "[cl.item(-1), cl.item(4294967296), cl.item(4294967297), cl.item(NaN), cl.item('1'), cl.item(1.9), cl.item(Infinity)].join('|')"), "|a|b|a|b|b|a");
    });
}

#[test]
fn class_list_iteration() {
    big(|| {
        let mut b = at(PAGE);
        js(&mut b, "var el=document.getElementById('a'); var cl=el.classList; el.className='a b c';");
        assert_eq!(js(&mut b, "var o=[]; for (var t of cl) o.push(t); o.join()"), "a,b,c");
        assert_eq!(js(&mut b, "[...cl].join() + '|' + Array.from(cl).join() + '|' + Array.from(cl.keys()).join() + '|' + JSON.stringify(Array.from(cl.entries()))"), "a,b,c|a,b,c|0,1,2|[[0,\"a\"],[1,\"b\"],[2,\"c\"]]");
        assert_eq!(js(&mut b, "cl[Symbol.iterator] === cl.values"), "true");
        assert_eq!(js(&mut b, "var it=cl.values(); typeof it.next + (it[Symbol.iterator]() === it)"), "functiontrue");
        assert_eq!(js(&mut b, "var o=[]; cl.forEach(function(v,i,l){ o.push(v+i+(l===cl)+(this.k)) }, {k:'K'}); o.join()"), "a0trueK,b1trueK,c2trueK");
        assert_eq!(js(&mut b, "cl.forEach(5)"), "throw:TypeError:Failed to execute 'forEach' on 'DOMTokenList': parameter 1 is not of type 'Function'.");
        assert_eq!(js(&mut b, "cl.forEach(function(){ throw new SyntaxError('from-cb') })"), "throw:SyntaxError:from-cb");
        // Live: the iterator and forEach see changes made during the walk; an exhausted iterator stays done.
        assert_eq!(js(&mut b, "var seen=[]; cl.forEach(function(v,i){ seen.push(v); if (i===0) cl.add('d'); }); seen.join()"), "a,b,c,d");
        assert_eq!(js(&mut b, "var it=cl.values(); it.next(); cl.remove('a'); var r=[]; var s; while(!(s=it.next()).done) r.push(s.value); cl.add('zz'); r.join() + '|' + JSON.stringify(it.next())"), "c,d|{\"done\":true}");
        assert_eq!(js(&mut b, "Object.keys(cl.values().next()).join()"), "value,done");
        assert_eq!(js(&mut b, "cl.values().next.call({})"), "throw:TypeError:Illegal invocation");
    });
}

#[test]
fn class_list_gc_detached_and_slot_reuse() {
    big(|| {
        let mut b = at(PAGE);
        // A detached element is kept alive only by its classList and by an iterator over it.
        js(&mut b, "var keep=(function(){ var e=document.createElement('section'); e.className='one two'; return e.classList; })(); var itr=(function(){ var e=document.createElement('aside'); e.className='k'; return e.classList.values(); })();");
        let before = b.stats().wrappers;
        b.collect(&[]).unwrap();
        assert_eq!(b.stats().wrappers, before, "wrappers of detached elements survive through their lists");
        assert_eq!(js(&mut b, "keep.add('three'); keep.value + '|' + itr.next().value"), "one two three|k");
        assert_eq!(js(&mut b, "keep.item(0)"), "one");
        let p = b.platform_stats();
        assert_eq!((p.token_lists, p.token_iters), (2, 1));
        // Drop everything: lists, iterator and the element wrappers are pruned.
        js(&mut b, "keep=null; itr=null;");
        b.collect(&[]).unwrap();
        let p = b.platform_stats();
        assert_eq!((p.token_lists, p.token_iters), (0, 0));
        assert!(b.stats().wrappers < before);
        // Slot reuse: lists made now start clean and are not confused with the dead ones.
        js(&mut b, "var ls=[]; for (var i=0;i<30;i++){ var e=document.createElement('i'); e.className='n'+i; ls.push(e.classList) }");
        assert_eq!(js(&mut b, "ls.every(function(l,i){ return l.length===1 && l.item(0)==='n'+i })"), "true");
        js(&mut b, "ls=null; e=null");
        b.collect(&[]).unwrap();
        assert_eq!(b.platform_stats().token_lists, 0);
        js(&mut b, "var f=document.createElement('b'); f.className='fresh'; var l2=f.classList;");
        assert_eq!(js(&mut b, "l2.value + l2.length"), "fresh1");
        // An attached element's list is kept while the element is attached, even when JS holds nothing.
        js(&mut b, "document.getElementById('a').classList.add('z')");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('a').classList.contains('z')"), "true");
    });
}

#[test]
fn class_list_caps() {
    big(|| {
        let mut b = at(PAGE);
        b.set_platform_limits(PlatformLimits { max_token_lists: 2, max_token_iters: 1, max_class_tokens: 3, ..Default::default() });
        js(&mut b, "var a=document.getElementById('a').classList, c=document.getElementById('b').classList;");
        assert_eq!(js(&mut b, "document.body.classList"), lim("token-lists"));
        assert_eq!(js(&mut b, "a.add('1','2','3','4')"), lim("class-tokens"));
        assert_eq!(js(&mut b, "a.value"), "x y");
        js(&mut b, "var i1=a.values()");
        assert_eq!(js(&mut b, "a.keys()"), lim("token-iterators"));
        // The native text buffer caps repeated rewrites.
        let mut small = Browser::new(tree::parse(PAGE), BrowserLimits { max_text_bytes: 400, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).unwrap();
        let r = js(&mut small, "var c=document.getElementById('a').classList; for (var i=0;i<1000;i++){ c.add('t'); c.remove('t'); } 'done'");
        assert_eq!(r, lim("dom-text"));
        // Writing the same text again does not grow the buffer.
        let mut s2 = Browser::new(tree::parse(PAGE), BrowserLimits { max_text_bytes: 400, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).unwrap();
        assert_eq!(js(&mut s2, "var c=document.getElementById('a').classList; for (var i=0;i<2000;i++){ c.add('x'); c.toggle('y', true); c.contains('x') } c.value"), "x y");
        // A long attribute costs steps, so a loop over it stops under a small budget.
        let mut tiny = mk(PAGE, 300_000);
        let r = js(&mut tiny, "var e=document.getElementById('a'); e.className = new Array(5000).join('tok '); var c=e.classList; for (var i=0;i<100000;i++) c.item(4000); 'done'");
        assert_eq!(r, "limit:Steps");
    });
}

// ---------------------------------------------------------------------------------------------- getElementsBy*

const TREE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=r class='a b'><p id=p1 class=a>x</p><span id=s1 class='b a'><p id=p2 class='a  c'></p></span><P id=p3></P></div><p id=p4 class='A'></p><svg id=sv><rect id=rc class=a></rect></svg></body></html>";

#[test]
fn get_elements_by_tag_name() {
    big(|| {
        let mut b = at(TREE);
        let ids = "Array.prototype.map.call(r, function(e){return e.id}).join()";
        assert_eq!(js(&mut b, &format!("var r=document.getElementsByTagName('p'); {ids}")), "p1,p2,p3,p4");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByTagName('P'); {ids}")), "p1,p2,p3,p4", "case-insensitive for HTML elements");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByTagName('*'); r.length + ':' + {ids}")), "12:,,,,r,p1,s1,p2,p3,p4,sv,rc");
        assert_eq!(js(&mut b, &format!("r=document.getElementById('r').getElementsByTagName('p'); {ids}")), "p1,p2,p3", "scope: descendants only");
        assert_eq!(js(&mut b, &format!("r=document.getElementById('r').getElementsByTagName('*'); {ids}")), "p1,s1,p2,p3");
        assert_eq!(js(&mut b, "document.getElementById('p1').getElementsByTagName('*').length"), "0");
        assert_eq!(js(&mut b, "document.getElementsByTagName('nosuchtag').length + ',' + document.getElementsByTagName('').length"), "0,0");
        assert_eq!(js(&mut b, "document.getElementsByTagName('script').length"), "0");
        assert_eq!(js(&mut b, "document.getElementsByTagName('p')[0] === document.getElementById('p1')"), "true");
        assert_eq!(js(&mut b, "Array.isArray(document.getElementsByTagName('p'))"), "true");
        // Snapshot, documented: it does not follow later changes.
        assert_eq!(js(&mut b, "var snap=document.getElementsByTagName('p'); document.body.appendChild(document.createElement('p')); snap.length + ',' + document.getElementsByTagName('p').length"), "4,5");
        // Created elements and removed ones.
        assert_eq!(js(&mut b, "var q=document.createElement('p'); q.id='q'; q.getElementsByTagName('*').length + ',' + document.getElementsByTagName('p').length"), "0,5");
        assert_eq!(js(&mut b, "document.getElementsByTagName()"), "throw:TypeError:Failed to execute 'getElementsByTagName': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "document.getElementsByTagName(undefined).length + ',' + document.getElementsByTagName(null).length"), "0,0");
    });
}

#[test]
fn get_elements_by_class_name() {
    big(|| {
        let mut b = at(TREE);
        let ids = "Array.prototype.map.call(r, function(e){return e.id}).join()";
        assert_eq!(js(&mut b, &format!("var r=document.getElementsByClassName('a'); {ids}")), "r,p1,s1,p2,rc", "tree order, the root element of the scope is not special");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('a b'); {ids}")), "r,s1", "all names, any order");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('  b \\t a\\n'); {ids}")), "r,s1");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('a a'); {ids}")), "r,p1,s1,p2,rc");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('A'); {ids}")), "p4", "case-sensitive (standards mode)");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('c'); {ids}")), "p2");
        assert_eq!(js(&mut b, "document.getElementsByClassName('').length + ',' + document.getElementsByClassName('  ').length + ',' + document.getElementsByClassName('nope').length"), "0,0,0");
        assert_eq!(js(&mut b, &format!("r=document.getElementById('r').getElementsByClassName('a'); {ids}")), "p1,s1,p2", "descendants only");
        assert_eq!(js(&mut b, "document.getElementsByClassName('a')[1] === document.getElementById('p1')"), "true");
        js(&mut b, "document.getElementById('p1').classList.add('zz')");
        assert_eq!(js(&mut b, &format!("r=document.getElementsByClassName('zz a'); {ids}")), "p1", "sees the class list change");
        assert_eq!(js(&mut b, "document.getElementsByClassName()"), "throw:TypeError:Failed to execute 'getElementsByClassName': 1 argument required, but only 0 present.");
        // The collection cap is a Limit before the array grows.
        let mut small = Browser::new(tree::parse(TREE), BrowserLimits { max_results: 2, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).unwrap();
        assert_eq!(js(&mut small, "document.getElementsByClassName('a').length"), lim("dom-results"));
        assert_eq!(js(&mut small, "document.getElementsByTagName('*').length"), lim("dom-results"));
        assert_eq!(js(&mut small, "document.getElementsByClassName('c').length"), "1");
        // The name cap.
        b.set_platform_limits(PlatformLimits { max_class_tokens: 2, ..Default::default() });
        assert_eq!(js(&mut b, "document.getElementsByClassName('a b c')"), lim("class-tokens"));
        assert_eq!(js(&mut b, "document.getElementsByClassName('a b a b').length"), "2");
    });
}

#[test]
fn get_elements_by_work_is_charged_and_bounded() {
    big(|| {
        let page = format!("<!doctype html><body>{}</body>", "<i class=k></i>".repeat(3000));
        let mut ok = mk(&page, 5_000_000);
        assert_eq!(js(&mut ok, "document.getElementsByTagName('i').length + ',' + document.getElementsByClassName('k').length"), "3000,3000");
        let mut tiny = mk(&page, 2_000);
        assert_eq!(js(&mut tiny, "document.getElementsByTagName('i').length"), "limit:Steps");
        let mut tiny = mk(&page, 2_000);
        assert_eq!(js(&mut tiny, "document.getElementsByClassName('k').length"), "limit:Steps");
        // Wrappers are bounded too (the result needs a wrapper for each node).
        let mut few = Browser::new(tree::parse(&page), BrowserLimits { max_wrappers: 50, ..Default::default() }, EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut few, "document.getElementsByTagName('i').length"), lim("dom-wrappers"));
    });
}

#[test]
fn webpack_runtime_script_lookup_shape() {
    big(|| {
        // The lookup `webpack-runtime.js` does (`b.l`): scripts of the saved page, then `getAttribute` on each.
        let mut b = Browser::new(tree::parse(LOGIN), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.set_document_url("https://staging.app.8vox.ai/login").unwrap();
        let native = {
            let doc = b.document();
            let doc = doc.borrow();
            let mut n = 0;
            let mut stack = vec![doc.root()];
            while let Some(c) = stack.pop() {
                if matches!(*doc.data(c), NodeData::Element { name, .. } if doc.atom_str(name) == "script") {
                    n += 1;
                }
                stack.extend(doc.children(c));
            }
            n
        };
        assert!(native > 10);
        assert_eq!(js(&mut b, "var a=document.getElementsByTagName('script'); a.length"), native.to_string());
        // Same loop as `b.l`: find a script by its `src` attribute (and one that does not exist).
        let srcs = js(&mut b, "Array.prototype.map.call(a, function(s){ return s.getAttribute('src') }).join('|')");
        let first_src = srcs.split('|').find(|s| !s.is_empty() && *s != "null").expect("a script with src").to_string();
        let r = js(&mut b, &format!("var found=null; for (var f=0; f<a.length; f++){{ var l=a[f]; if (l.getAttribute('src') == '{first_src}' || l.getAttribute('data-webpack') == '_N_E:x') {{ found=l; break }} }} found.getAttribute('src')"));
        assert_eq!(r, first_src);
        assert_eq!(js(&mut b, "document.getElementsByTagName('link').length > 0 && document.getElementsByTagName('style').length >= 0"), "true");
    });
}

//! Tests of the CSSOM object model: the shape of a rule declaration, and rule objects that are made on demand.
use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}", b.it.get_str(v, "name")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// The number of rule objects that exist now.
fn made(b: &Browser) -> usize {
    crate::browser_cssom::rule_objects(&b.dom)
}

fn many(n: usize) -> String {
    let mut css = String::new();
    for i in 0..n {
        css.push_str(&format!(".c{i} {{ color: red; margin: {i}px }}\n"));
    }
    format!("<style>{css}</style><p class=c3>x</p>")
}

#[test]
fn a_rule_declaration_has_the_prototype_and_methods_of_element_style() {
    big(|| {
        let mut b = browser("<style>a { color: red; margin: 1px 2px }</style><p id=p>x</p>");
        assert_eq!(js(&mut b, "var s = document.styleSheets[0].cssRules[0].style, es = document.getElementById('p').style; Object.getPrototypeOf(s) === Object.getPrototypeOf(es) && Object.getPrototypeOf(s) === CSSStyleDeclaration.prototype"), "true");
        assert_eq!(js(&mut b, "[s.setProperty === es.setProperty, s.removeProperty === es.removeProperty, s.hasOwnProperty('setProperty'), s.hasOwnProperty('color')].join()"), "true,true,false,false");
        assert_eq!(js(&mut b, "JSON.stringify(Object.getOwnPropertyNames(s))"), r#"["0","1","2","3","4"]"#);
        assert_eq!(js(&mut b, "['color' in s, 'setProperty' in s, 'marginTop' in s, s.length, s[1], s.color, s.margin, s.cssText].join('|')"), "true|true|true|5|margin-top|red|1px 2px|color: red; margin: 1px 2px;");
        assert_eq!(js(&mut b, "s.setProperty.call(es, 'top', '2px'); es.top"), "2px");
    });
}

#[test]
fn every_write_to_a_rule_declaration_reaches_the_sheet_text() {
    big(|| {
        let mut b = browser("<style>a { color: red }</style><a id=t>x</a>");
        assert_eq!(js(&mut b, "var st = document.querySelector('style'), s = st.sheet.cssRules[0].style; s.color = 'green'; st.textContent"), "a { color: green; }\n");
        assert_eq!(js(&mut b, "s.setProperty('top', '9px', 'important'); st.textContent"), "a { color: green; top: 9px !important; }\n");
        assert_eq!(js(&mut b, "s.removeProperty('color'); st.textContent"), "a { top: 9px !important; }\n");
        assert_eq!(js(&mut b, "s.cssText = 'left: 1px'; st.textContent"), "a { left: 1px; }\n");
        assert_eq!(js(&mut b, "s.marginTop = '3px'; getComputedStyle(document.getElementById('t')).marginTop"), "3px");
        // A write through the declaration of an element does not touch a sheet.
        assert_eq!(js(&mut b, "document.getElementById('t').style.color = 'blue'; st.textContent"), "a { left: 1px; margin-top: 3px; }\n");
        assert_eq!(js(&mut b, "try { CSSStyleDeclaration.prototype.setProperty.call({}, 'top', '1px') } catch (e) { e.constructor.name }"), "TypeError");
    });
}

#[test]
fn reading_the_length_of_a_rule_list_makes_no_rule_object() {
    big(|| {
        let mut b = browser(&many(500));
        assert_eq!(js(&mut b, "document.styleSheets[0].cssRules.length"), "500");
        assert_eq!(made(&b), 0);
        assert_eq!(js(&mut b, "var r = document.styleSheets[0].cssRules; r.length + ',' + r.item(7).selectorText"), "500,.c7");
        assert_eq!(made(&b), 1);
        assert_eq!(js(&mut b, "r[3] === r[3] && r[3] === r.item(3) && r[3] === document.styleSheets[0].cssRules[3]"), "true");
        assert_eq!(made(&b), 2);
        assert_eq!(js(&mut b, "r[499].selectorText + r[499].style.margin + (r[500] === undefined) + (500 in r) + (499 in r)"), ".c499499pxtruefalsetrue");
        assert_eq!(made(&b), 3);
        assert_eq!(js(&mut b, "var n = 0; for (var x of r) n += x.style.length; n"), "2500");
        assert_eq!(made(&b), 500);
    });
}

#[test]
fn rule_objects_made_on_demand_keep_their_place_when_the_sheet_changes() {
    big(|| {
        let mut b = browser(&many(40));
        assert_eq!(js(&mut b, "var st = document.querySelector('style'), sh = st.sheet, r = sh.cssRules, r5 = r[5]; r5.selectorText"), ".c5");
        assert_eq!(js(&mut b, "sh.insertRule('.new { top: 1px }', 0); [r.length, r[0].selectorText, r[6] === r5, r[6].selectorText, r[7].selectorText].join('|')"), "41|.new|true|.c5|.c6");
        assert_eq!(js(&mut b, "st.textContent.includes('.c39 { color: red; margin: 39px }') && st.textContent.startsWith('.new { top: 1px; }')"), "true");
        assert_eq!(js(&mut b, "sh.deleteRule(1); [r.length, r[1].selectorText, r5.parentStyleSheet === sh, r[5] === r5].join('|')"), "40|.c1|true|true");
        assert_eq!(js(&mut b, "sh.deleteRule(5); [r.length, r5.parentStyleSheet, r[5].selectorText].join('|')"), "39||.c6");
        assert_eq!(js(&mut b, "r[30].style.color = 'blue'; st.textContent.includes('.c31 { color: blue; margin: 31px; }') && st.textContent.includes('.c32 { color: red; margin: 32px }')"), "true");
        assert_eq!(js(&mut b, "r.length + ',' + r[38].selectorText + ',' + r[0].selectorText"), "39,.c39,.new");
        // The sheet text changed: the list reads the new text, not stale spans.
        assert_eq!(js(&mut b, "st.textContent = '.a{top:1px}.b{top:2px}'; r.length + r[1].selectorText"), "2.b");
    });
}

#[test]
fn nested_rules_are_made_on_demand_too() {
    big(|| {
        let mut css = String::from("@media screen {");
        for i in 0..100 {
            css.push_str(&format!(".n{i} {{ color: red }}"));
        }
        css.push_str("} @keyframes k { from { opacity: 0 } 50% { opacity: .5 } to { opacity: 1 } }");
        let mut b = browser(&format!("<style>{css}</style>"));
        assert_eq!(js(&mut b, "var st = document.querySelector('style'), r = st.sheet.cssRules, m = r[0]; r.length + ',' + m.cssRules.length"), "2,100");
        assert_eq!(made(&b), 1);
        assert_eq!(js(&mut b, "m.cssRules[42].selectorText + (m.cssRules[42] === m.cssRules[42]) + (m.cssRules[42].parentRule === m)"), ".n42truetrue");
        assert_eq!(made(&b), 2);
        assert_eq!(js(&mut b, "m.insertRule('.x { top: 1px }', 0); m.cssRules[43].selectorText + ',' + m.cssRules[0].selectorText"), ".n42,.x");
        assert_eq!(js(&mut b, "st.textContent.includes('.n99 { color: red }') && st.textContent.includes('@keyframes')"), "true");
        assert_eq!(js(&mut b, "var k = r[1]; [k.cssRules.length, k.cssRules[1].keyText, k.cssRules[2].style.opacity].join()"), "3,50%,1");
    });
}

#[test]
fn a_rule_declaration_is_made_when_a_script_needs_it() {
    big(|| {
        let mut b = browser(&many(10));
        assert_eq!(js(&mut b, "var r = document.styleSheets[0].cssRules; r[0].selectorText + r[1].selectorText + r[1].type"), ".c0.c11");
        assert_eq!((made(&b), b.style_stats().objects), (2, 0));
        assert_eq!(js(&mut b, "r[0].style === r[0].style && r[0].style.color"), "red");
        assert_eq!(b.style_stats().objects, 1);
        assert_eq!(js(&mut b, "r[1].cssText"), ".c1 { color: red; margin: 1px; }");
        assert_eq!(b.style_stats().objects, 2);
        // A rule whose declaration was never read keeps its text when the sheet is published.
        assert_eq!(js(&mut b, "var st = document.querySelector('style'); r[2].selectorText; r[5].style.color = 'blue'; st.textContent.includes('.c2 { color: red; margin: 2px; }') && st.textContent.includes('.c5 { color: blue; margin: 5px; }')"), "true");
    });
}

#[test]
fn a_constructed_sheet_makes_rule_objects_on_demand_too() {
    big(|| {
        let mut b = browser("<p class=c3>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(); sh.replaceSync('.a{top:1px}.b{top:2px}@media print{.c{top:3px}}'); sh.cssRules.length"), "3");
        assert_eq!(made(&b), 0);
        assert_eq!(js(&mut b, "sh.cssRules[1].selectorText + sh.cssRules[2].cssRules[0].selectorText"), ".b.c");
        assert_eq!(js(&mut b, "sh.insertRule('.z{top:9px}', 0); [sh.cssRules.length, sh.cssRules[0].selectorText, sh.cssRules[2].selectorText, sh.cssRules[3].type].join()"), "4,.z,.b,4");
        assert_eq!(js(&mut b, "sh.replaceSync('.q{}'); sh.cssRules.length + sh.cssRules[0].selectorText"), "1.q");
    });
}

#[test]
fn a_page_can_read_more_rules_than_the_old_object_cap() {
    big(|| {
        let n = 66_000;
        let mut b = browser(&many(n));
        assert_eq!(js(&mut b, "var r = document.styleSheets[0].cssRules, last = ''; for (var i = 0; i < r.length; i++) last = r[i].selectorText; r.length + last"), format!("{n}.c{}", n - 1));
        assert_eq!(made(&b), n);
    });
}

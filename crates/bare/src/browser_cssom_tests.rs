//! Tests of the CSSOM object model: the shape of a rule declaration, and rule objects that are made on demand.
use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}", b.it.get_str(v, "name")),
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
#[ignore = "vex: left: 'throw:TypeError'"]
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
#[ignore = "vex: left: 'throw:TypeError'"]
fn rule_objects_made_on_demand_keep_their_place_when_the_sheet_changes() {
    big(|| {
        let mut b = browser(&many(40));
        assert_eq!(js(&mut b, "var st = document.querySelector('style'), sh = st.sheet, r = sh.cssRules, r5 = r[5]; r5.selectorText"), ".c5");
        assert_eq!(js(&mut b, "sh.insertRule('.new { top: 1px }', 0); [r.length, r[0].selectorText, r[6] === r5, r[6].selectorText, r[7].selectorText].join('|')"), "41|.new|true|.c5|.c6");
        assert_eq!(js(&mut b, "st.textContent.includes('.c39 { color: red; margin: 39px }') && st.textContent.startsWith('.new { top: 1px }')"), "true");
        assert_eq!(js(&mut b, "sh.deleteRule(1); [r.length, r[1].selectorText, r5.parentStyleSheet === sh, r[5] === r5].join('|')"), "40|.c1|true|true");
        assert_eq!(js(&mut b, "sh.deleteRule(5); [r.length, r5.parentStyleSheet, r[5].selectorText].join('|')"), "39||.c6");
        assert_eq!(js(&mut b, "r[30].style.color = 'blue'; st.textContent.includes('.c31 { color: blue; margin: 31px; }') && st.textContent.includes('.c32 { color: red; margin: 32px }')"), "true");
        assert_eq!(js(&mut b, "r.length + ',' + r[38].selectorText + ',' + r[0].selectorText"), "39,.c39,.new");
        // The sheet text changed: the list reads the new text, not stale spans.
        assert_eq!(js(&mut b, "st.textContent = '.a{top:1px}.b{top:2px}'; r.length + r[1].selectorText"), "2.b");
    });
}

#[test]
#[ignore = "vex: left: 'throw:TypeError'"]
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
        // A rule whose declaration was never read keeps its text when the sheet is published. The element text stays (`browser_cssom_owned_tests.rs`).
        assert_eq!(js(&mut b, "var st = document.querySelector('style'), t = st.textContent; r[2].selectorText; r[5].style.color = 'blue'; var c2 = document.createElement('i'), c5 = document.createElement('i'); c2.className = 'c2'; c5.className = 'c5'; document.body.append(c2, c5); [getComputedStyle(c2).color, getComputedStyle(c2).marginTop, getComputedStyle(c5).color, getComputedStyle(c5).marginTop, st.textContent === t].join()"), "rgb(255, 0, 0),2px,rgb(0, 0, 255),5px,true");
    });
}

#[test]
fn constructed_sheet_edits_parse_only_new_rules_and_publish_on_read() {
    use crate::browser_cssom::take_work;

    big(|| {
        let measure = |n: usize| {
            let mut b = browser("<p id=p>x</p>");
            assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(), r = sh.cssRules, p = document.getElementById('p'); sh.replaceSync('#p { color: red }'); document.adoptedStyleSheets = [sh]; r.length + '|' + getComputedStyle(p).color"), "1|rgb(255, 0, 0)");
            take_work();
            let builds = b.computed_stats().builds;
            assert_eq!(js(&mut b, &format!("for (var i = 0; i < {n}; i++) sh.insertRule('#p {{ color: blue; top: ' + i + 'px }}', r.length); r.length")), (n + 1).to_string());
            let insert = take_work();
            assert_eq!(b.computed_stats().builds, builds);
            assert_eq!(js(&mut b, "getComputedStyle(p).color"), "rgb(0, 0, 255)");
            let read_after_insert = take_work();
            assert_eq!(b.computed_stats().builds, builds + 1);
            assert_eq!(js(&mut b, "while (r.length > 1) sh.deleteRule(r.length - 1); r.length"), "1");
            let delete = take_work();
            assert_eq!(b.computed_stats().builds, builds + 1);
            assert_eq!(js(&mut b, "getComputedStyle(p).color"), "rgb(255, 0, 0)");
            (insert, read_after_insert, delete, take_work())
        };
        let (small, large) = (measure(64_usize), measure(128_usize));
        assert_eq!(
            (small, large),
            (((64, 0), (0, 65), (0, 0), (0, 1)), ((128, 0), (0, 129), (0, 0), (0, 1))),
        );
    });
}

#[test]
fn reading_constructed_rules_preserves_the_adopted_text_and_live_indexes() {
    big(|| {
        let mut b = browser("<p id=p>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(), r = sh.cssRules, p = document.getElementById('p'); sh.replaceSync('#p { color: red }'); document.adoptedStyleSheets = [sh]; sh.insertRule('#p{color:blue}', 1)"), "1");
        assert_eq!(js(&mut b, "var blue = r[1]; [blue === r.item(1), getComputedStyle(p).color].join('|')"), "true|rgb(0, 0, 255)");
        assert_eq!(js(&mut b, "sh.insertRule('#p{color:green}', 0); [blue === r[2], getComputedStyle(p).color].join('|')"), "true|rgb(0, 0, 255)");
        assert_eq!(js(&mut b, "sh.deleteRule(2); [blue.parentStyleSheet === null, r.length, getComputedStyle(p).color].join('|')"), "true|2|rgb(255, 0, 0)");
        assert_eq!(js(&mut b, "sh.insertRule('#p{color:blue}', 2); var other = r[2]; sh.deleteRule(2); [blue !== other, other.parentStyleSheet === null, getComputedStyle(p).color].join('|')"), "true|true|rgb(255, 0, 0)");
    });
}

#[test]
fn compacting_a_constructed_sheet_preserves_its_remaining_rules() {
    big(|| {
        let mut b = browser("<p id=p>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(), r = sh.cssRules, p = document.getElementById('p'); sh.replaceSync('#p{color:red}'); document.adoptedStyleSheets = [sh]; for (var i = 0; i < 512; i++) sh.insertRule('.other{top:' + i + 'px}', r.length); sh.insertRule('#p{color:blue}', r.length); r.length"), "514");
        assert_eq!(js(&mut b, "while (r.length > 3) sh.deleteRule(1); [r.length, r[1].style.top, r[2].selectorText, getComputedStyle(p).color].join('|')"), "3|511px|#p|rgb(0, 0, 255)");
    });
}

#[test]
fn a_style_read_during_serialization_keeps_pending_rule_spans_valid() {
    big(|| {
        let mut b = browser("<p id=p>x</p>");
        let result = js(&mut b, r#"
            var sh = new CSSStyleSheet(), r = sh.cssRules, p = document.getElementById('p');
            sh.replaceSync('#p{color:red}');
            document.adoptedStyleSheets = [sh];
            sh.insertRule('#p{color:blue}', 0);
            var blue = r[0];
            var proto = CSSStyleDeclaration.prototype;
            var get = Object.getOwnPropertyDescriptor(proto, 'cssText').get, reads = 0;
            Object.defineProperty(proto, 'cssText', {
                get: function () { getComputedStyle(p).color; reads++; return get.call(this); }
            });
            sh.insertRule('.other{top:1px}', 1);
            [reads, r[0] === blue, r[1].selectorText, getComputedStyle(p).color].join('|')
        "#);
        // The insert serializes no rule, so the getter of the page does not run in it (as in Chrome).
        assert_eq!(result, "0|true|.other|rgb(255, 0, 0)");
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

/// The collector runs between tasks only, so a page that edits a sheet in a loop is a series of tasks here.
fn collect(b: &mut Browser) {
    b.collect(&[]).expect("collect");
}

#[test]
fn a_style_sheet_edited_in_a_loop_leaves_no_objects_and_hits_no_cap() {
    big(|| {
        let mut b = browser("<style>.a { color: red }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0]; function batch() { for (var i = 0; i < 1000; i++) { sh.insertRule('.b { top: 1px }', 0); sh.deleteRule(0); } return 'ok' }; sh.cssRules.length"), "1");
        // Each edit writes the text of the element, and the DOM keeps every text node (`dom-nodes`, 1 Mi): so this loop is shorter than the one below.
        for round in 0..200 {
            assert_eq!(js(&mut b, "batch()"), "ok", "round {round}");
            if round % 50 == 49 {
                collect(&mut b);
                assert!(crate::browser_cssom::table_sizes(&b.dom).0 <= 2, "round {round}: {:?}", crate::browser_cssom::table_sizes(&b.dom));
            }
        }
        assert_eq!(js(&mut b, "sh.cssRules.length + sh.cssRules[0].selectorText"), "1.a");
    });
}

#[test]
fn a_constructed_sheet_edited_more_often_than_the_old_cap_leaves_no_objects() {
    big(|| {
        let mut b = browser("<p>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(); sh.replaceSync('.a { color: red }'); document.adoptedStyleSheets = [sh]; function batch() { for (var i = 0; i < 1000; i++) { sh.insertRule('.b { top: 1px }', 0); sh.deleteRule(0); } return 'ok' }; sh.cssRules.length"), "1");
        // 1,000,000 cycles: more than the 524,288 objects of the old cap.
        for round in 0..1000 {
            assert_eq!(js(&mut b, "batch()"), "ok", "round {round}");
            if round % 100 == 99 {
                collect(&mut b);
                assert!(crate::browser_cssom::table_sizes(&b.dom).0 <= 2, "round {round}: {:?}", crate::browser_cssom::table_sizes(&b.dom));
            }
        }
        assert_eq!(js(&mut b, "sh.cssRules.length + sh.cssRules[0].selectorText"), "1.a");
    });
}

#[test]
fn rules_that_a_script_read_and_deleted_are_released_by_the_collector() {
    big(|| {
        let mut b = browser("<style>.a { color: red }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0]; function batch() { var t = 0; for (var i = 0; i < 500; i++) { sh.insertRule('.b { top: 1px }', 0); t += sh.cssRules[0].style.top.length; sh.deleteRule(0); } return t }; batch()"), "1500");
        for _ in 0..20 {
            assert_eq!(js(&mut b, "batch()"), "1500");
        }
        collect(&mut b);
        let (rules, lists, sheets, decls, medias) = crate::browser_cssom::table_sizes(&b.dom);
        assert!(rules <= 2 && decls <= 2 && medias == 0 && lists == 2 && sheets == 1, "{:?}", (rules, lists, sheets, decls, medias));
        assert!(b.style_stats().objects <= 2, "{}", b.style_stats().objects);
        assert_eq!(js(&mut b, "sh.cssRules.length + sh.cssRules[0].selectorText"), "1.a");
    });
}

#[test]
#[ignore = "vex: left: 'false'"]
fn a_rule_in_a_live_list_keeps_its_identity_and_expandos_across_a_collect() {
    big(|| {
        let mut b = browser("<style>.a { color: red } @media screen { .m { top: 1px } }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0]; sh.cssRules[0].mark = 7; var m = sh.cssRules[1].cssRules[0]; m.mark = 8; var s0 = sh.cssRules[0].style; s0.color"), "red");
        // No script variable holds the first rule: only its list does.
        collect(&mut b);
        collect(&mut b);
        assert_eq!(js(&mut b, "[sh.cssRules[0] === sh.cssRules[0], sh.cssRules[0].mark, sh.cssRules[0].style === s0, sh.cssRules[1].cssRules[0] === m, m.mark, sh.cssRules[1].cssRules[0].parentRule === sh.cssRules[1]].join()"), "true,7,true,true,8,true");
        assert_eq!(js(&mut b, "sh.cssRules[0].parentStyleSheet === sh && document.styleSheets[0] === sh && document.querySelector('style').sheet === sh"), "true");
    });
}

#[test]
fn a_deleted_rule_that_a_script_holds_stays_a_valid_object() {
    big(|| {
        let mut b = browser("<style>.a { color: red } @media screen { .m { top: 1px } } .z { left: 2px }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0], r = sh.cssRules[0], st = r.style, g = sh.cssRules[1], inner = g.cssRules[0]; st.color + r.selectorText"), "red.a");
        assert_eq!(js(&mut b, "sh.deleteRule(0); sh.deleteRule(0); sh.cssRules.length"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(js(&mut b, "[r.parentStyleSheet, g.parentStyleSheet, r.selectorText, r.style === st, st.color, r.cssText, r.type, g.cssRules.length, g.cssRules[0] === inner, inner.selectorText, inner.parentRule === g, inner.parentStyleSheet].join('|')"), "||.a|true|red|.a { color: red; }|1|1|true|.m|true|");
        // A write through a held deleted rule works and does not touch the live rules.
        assert_eq!(js(&mut b, "st.color = 'blue'; r.selectorText = '.q'; inner.style.top = '9px'; [r.style.color, r.selectorText, sh.cssRules.length, sh.cssRules[0].selectorText, document.querySelector('style').textContent.includes('.z')].join()"), "blue,.q,1,.z,true");
        assert_eq!(js(&mut b, "sh.insertRule('.n { top: 0 }', 0); [sh.cssRules[0].selectorText, r.parentStyleSheet, sh.cssRules.length].join('|')"), ".n||2");
    });
}

#[test]
#[ignore = "vex: left: 'throw:TypeError'"]
fn a_removed_style_element_releases_its_objects_unless_a_script_holds_them() {
    big(|| {
        let mut b = browser("<style id=s1>.a { color: red }</style><style id=s2>.b { top: 1px }</style>");
        assert_eq!(js(&mut b, "var held = document.getElementById('s1').sheet.cssRules[0]; held.style.color; document.getElementById('s2').sheet.cssRules[0].selectorText + document.styleSheets.length"), ".b2");
        assert_eq!(js(&mut b, "document.getElementById('s1').remove(); document.getElementById('s2').remove(); document.styleSheets.length"), "0");
        collect(&mut b);
        // `held` keeps its rule and the sheet of that rule. The other sheet and its rule are gone.
        let (rules, _lists, sheets, decls, _) = crate::browser_cssom::table_sizes(&b.dom);
        assert_eq!((rules, sheets, decls), (1, 1, 1));
        assert_eq!(js(&mut b, "[held.selectorText, held.style.color, held.parentStyleSheet === null ? 'null' : 'attached'].join()"), ".a,red,attached");
        assert_eq!(js(&mut b, "held = null; 1"), "1");
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom), (0, 1, 0, 0, 0));
    });
}

#[test]
fn a_replaced_constructed_sheet_releases_its_old_rules_and_an_adopted_sheet_stays() {
    big(|| {
        let mut b = browser("<p>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(); document.adoptedStyleSheets = [sh]; sh.replaceSync('.a{top:1px}'); sh.cssRules[0].selectorText"), ".a");
        for _ in 0..50 {
            assert_eq!(js(&mut b, "sh.replaceSync('.a{top:1px}.b{top:2px}'); sh.cssRules[0].style.top + sh.cssRules[1].selectorText"), "1px.b");
        }
        // Nothing but the document holds the sheet now.
        assert_eq!(js(&mut b, "sh = null; 1"), "1");
        collect(&mut b);
        let (rules, _, sheets, decls, _) = crate::browser_cssom::table_sizes(&b.dom);
        assert_eq!((rules, sheets, decls), (2, 1, 1), "the adopted sheet and the 2 rules of its text stay; the rules of the 50 old texts go");
        assert_eq!(js(&mut b, "[document.adoptedStyleSheets.length, document.adoptedStyleSheets[0].cssRules.length, document.adoptedStyleSheets[0].cssRules[1].selectorText].join()"), "1,2,.b");
        assert_eq!(js(&mut b, "document.adoptedStyleSheets = []; 1"), "1");
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 0);
    });
}

#[test]
fn a_collect_between_creating_and_reading_leaves_no_stale_entry() {
    big(|| {
        let mut b = browser(&many(30));
        for round in 0..5 {
            assert_eq!(js(&mut b, "var r = document.styleSheets[0].cssRules; r[3].selectorText + r[4].style.margin"), ".c34px");
            assert_eq!(js(&mut b, "var h = r[10]; h.selectorText"), ".c10");
            collect(&mut b);
            assert_eq!(js(&mut b, "r[10] === h && r[3].selectorText + r[4].style.margin + r[11].style.length"), ".c34px5", "round {round}");
            assert_eq!(js(&mut b, "document.styleSheets[0].insertRule('.n{top:1px}', 0); document.styleSheets[0].deleteRule(0); r.length"), "30");
        }
    });
}

#[test]
#[ignore = "vex: left: 'throw:TypeError'"]
fn a_sheet_that_a_script_holds_keeps_its_identity_after_a_collect() {
    big(|| {
        let mut b = browser("<style id=s>.a { color: red }</style><p>x</p>");
        assert_eq!(js(&mut b, "var held = document.getElementById('s').sheet; held.mark = 3; 1"), "1");
        // The wrapper of the element dies with the script variable that held it. The held sheet stays, and so does `el.sheet`.
        collect(&mut b);
        collect(&mut b);
        assert_eq!(js(&mut b, "var el = document.getElementById('s'); [el.sheet === el.sheet, el.sheet === held, held.mark, held.ownerNode === el, held.cssRules[0].selectorText].join()"), "true,true,3,true,.a");
        // The wrapper of an element in the tree stays (the document wrapper reaches it), so its sheet stays too, with no script variable.
        assert_eq!(js(&mut b, "held = null; el = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 1);
        // Removed from the tree and held by no script: the wrapper goes, and the sheet with it. A new read makes a new sheet.
        assert_eq!(js(&mut b, "var e = document.getElementById('s'); e.remove(); e = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 0);
        assert_eq!(js(&mut b, "document.head.appendChild(document.createElement('style')).textContent = '.a { color: red }'; 1"), "1");
        assert_eq!(js(&mut b, "var e2 = document.querySelector('style'); [e2.sheet === e2.sheet, e2.sheet.mark === undefined, e2.sheet.cssRules[0].selectorText].join()"), "true,true,.a");
    });
}

#[test]
fn a_held_rule_keeps_its_sheet_and_a_dropped_rule_is_collected() {
    big(|| {
        let mut b = browser("<style>.a { color: red } .b { top: 1px } @media screen { .m { left: 2px } }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0], r = sh.cssRules[0], st = r.style, md = sh.cssRules[2].media; sh.cssRules[1].selectorText + st.color + md.mediaText"), ".bredscreen");
        assert_eq!(js(&mut b, "sh.deleteRule(1); 1"), "1");
        assert_eq!(js(&mut b, "document.querySelector('style').remove(); document.styleSheets.length"), "0");
        // Only `r`, `st` and `md` are held. `sh` is a variable too: drop it, so the rule must keep the sheet.
        assert_eq!(js(&mut b, "sh = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        let (rules, _, sheets, decls, medias) = crate::browser_cssom::table_sizes(&b.dom);
        assert_eq!((sheets, medias), (1, 1), "the held rule and media list keep the sheet");
        assert!(rules >= 2 && decls == 1, "{rules} {decls}");
        assert_eq!(js(&mut b, "[r.parentStyleSheet !== null, r.selectorText, st.color, md.mediaText].join()"), "true,.a,red,screen");
        // A media list keeps its rule, the rule keeps its sheet, and the list of the sheet keeps the other rule and its declaration.
        assert_eq!(js(&mut b, "r = null; st = null; 1"), "1");
        collect(&mut b);
        let (rules, _, sheets, decls, medias) = crate::browser_cssom::table_sizes(&b.dom);
        assert_eq!((rules, sheets, decls, medias), (2, 1, 1, 1));
        assert_eq!(js(&mut b, "md = null; 1"), "1");
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom), (0, 1, 0, 0, 0));
    });
}

#[test]
fn an_adopted_sheet_lives_with_its_root() {
    big(|| {
        let mut b = browser("<div id=h></div><p id=p>x</p>");
        assert_eq!(js(&mut b, "var sh = new CSSStyleSheet(); sh.replaceSync('#p { color: red }'); document.adoptedStyleSheets = [sh]; sh = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 1);
        assert_eq!(js(&mut b, "[document.adoptedStyleSheets.length, document.adoptedStyleSheets[0].cssRules[0].selectorText, getComputedStyle(document.getElementById('p')).color].join()"), "1,#p,rgb(255, 0, 0)");
        // A shadow root that a script holds keeps what it adopted. The host and the root are held by the script.
        assert_eq!(js(&mut b, "var root = document.getElementById('h').attachShadow({ mode: 'open' }); var s2 = new CSSStyleSheet(); s2.replaceSync(':host { top: 1px }'); root.adoptedStyleSheets = [s2]; s2 = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 2);
        assert_eq!(js(&mut b, "root.adoptedStyleSheets[0].cssRules[0].selectorText"), ":host");
        // The adopted list is cleared: nothing reaches the sheet any more.
        assert_eq!(js(&mut b, "root.adoptedStyleSheets = []; document.adoptedStyleSheets = []; 1"), "1");
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 0);
    });
}

#[test]
fn a_reused_key_answers_for_the_new_object_only() {
    big(|| {
        let mut b = browser("<style>.a { color: red }</style>");
        assert_eq!(js(&mut b, "var sh = document.styleSheets[0]; var rr = []; for (var i = 0; i < 20; i++) { sh.insertRule('.t' + i + ' { top: ' + i + 'px }', 0); rr.push(sh.cssRules[0]); } rr[0].style.top + rr[19].selectorText"), "0px.t19");
        assert_eq!(js(&mut b, "rr = null; sh.cssRules.length"), "21");
        // The 20 rules are dead. Their keys go to the objects made next.
        collect(&mut b);
        assert_eq!(js(&mut b, "for (var i = 0; i < 20; i++) { sh.deleteRule(0); } var n = []; for (var i = 0; i < 20; i++) { sh.insertRule('@media print { .u' + i + ' { top: 0 } }', 0); n.push(sh.cssRules[0]); } n[0].media.mediaText + n[19].cssRules[0].selectorText + n[5].cssRules[0].style.top"), "print.u190");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(js(&mut b, "[n[0].media.mediaText, n[19].cssRules[0].selectorText, n[7].parentStyleSheet === sh, sh.cssRules.length, sh.cssRules[0] === n[19]].join()"), "print,.u19,true,21,true");
    });
}

#[test]
fn a_shadow_root_that_no_script_holds_keeps_its_adopted_sheet() {
    big(|| {
        let mut b = browser("<div id=h></div>");
        assert_eq!(js(&mut b, "var s = new CSSStyleSheet(); s.replaceSync('p { color: red }'); var r = document.getElementById('h').attachShadow({ mode: 'open' }); r.innerHTML = '<p>x</p>'; r.adoptedStyleSheets = [s]; s = null; r = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 1);
        assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('h').shadowRoot.firstChild).color"), "rgb(255, 0, 0)");
    });
}

#[test]
fn a_detached_shadow_root_that_no_script_holds_leaves_no_adopted_entry() {
    big(|| {
        let mut b = browser("<div id=h></div><p id=p>x</p>");
        assert_eq!(js(&mut b, "var s = new CSSStyleSheet(); s.replaceSync('p { color: red }'); var h = document.createElement('div'); var r = h.attachShadow({ mode: 'open' }); r.adoptedStyleSheets = [s]; s = h = r = null; 1"), "1");
        collect(&mut b);
        collect(&mut b);
        assert_eq!(crate::browser_cssom::table_sizes(&b.dom).2, 0);
        assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('p')).color"), "rgb(0, 0, 0)");
    });
}

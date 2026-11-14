use super::*;
fn check(remove_one: bool, important: bool) {
    let lim = Lim { bytes: 10000, decls: 100 };
    let mut charge = |_| Ok(());
    let text = if important { "--m: 3px 4px; margin: var(--m) !important" } else { "--m: 3px 4px; margin: var(--m)" };
    let mut list = parse(text, lim, &mut charge).ok().unwrap();
    let mut out = String::new();
    if remove_one {
        assert!(remove(&mut list, "margin-left", lim.bytes, &mut charge).is_ok());
    } else {
        assert!(set(&mut list, "margin-top", "1px", false, lim, &mut out, &mut charge).is_ok());
    }
    assert!(serialize(&list, lim, &mut out, &mut charge).is_ok(), "pending substitution must serialize without a fatal stop");
    assert!(out.contains(if important { "margin-right:  !important;" } else { "margin-right: ;" }), "{out}");
    assert_eq!(get(&list, "margin-right", lim.bytes, &mut charge).ok().flatten().as_deref(), Some(""));
    assert_eq!(item(&list, 1).as_deref(), Some("margin-top"));
}
#[test] fn page_rule_insertion() {
    use crate::browser::{Browser, BrowserLimits};
    let mut b = Browser::new(bare_html::tree::parse("<html><head><style></style></head><body><div id=a class=x style='--r:7px'></div></body></html>"), BrowserLimits::default(), Default::default(), Default::default()).unwrap();
    let result = b.run_script("document.styleSheets[0].insertRule('.x { border-radius: var(--r); border-top-left-radius: 0; border-bottom-left-radius: 0 }', 0)");
    assert!(result.is_ok(), "{result:?}");
    run(&mut b, "var a = document.getElementById('a'); getComputedStyle(a).borderTopLeftRadius + ',' + getComputedStyle(a).borderTopRightRadius + ',' + getComputedStyle(a).borderBottomLeftRadius + ',' + getComputedStyle(a).borderBottomRightRadius", "0px,7px,0px,7px");
    run(&mut b, "a.style.setProperty('--r', '13px'); getComputedStyle(a).borderTopRightRadius", "13px");
}
#[test] fn overwrite_pending_longhand() { check(false, false); }
#[test] fn remove_pending_longhand() { check(true, false); }
#[test] fn priority_pending_longhand() { check(false, true); }

fn browser() -> crate::browser::Browser {
    crate::browser::Browser::new(bare_html::tree::parse("<html><head><style>#a { margin-left: 9px }</style></head><body><div id=a></div><div id=b></div></body></html>"), Default::default(), Default::default(), Default::default()).unwrap()
}

fn run(b: &mut crate::browser::Browser, source: &str, expected: &str) {
    let v = b.run_script(source).expect(source);
    assert_eq!(b.it.display(v), expected, "{source}");
}

#[test]
fn element_pending_computation_updates_removal_and_copy() {
    let mut b = browser();
    run(&mut b, "var firstRule = document.styleSheets[0].cssRules.item(0); var a = document.getElementById('a'); var c = document.getElementById('b'); a.style.cssText = '--m: 3px 4px; margin: var(--m)'; a.style.marginTop = '1px'; getComputedStyle(a).marginRight", "4px");
    run(&mut b, "document.styleSheets[0].cssRules.item(0) === firstRule", "true");
    run(&mut b, "a.style.marginRight + ',' + a.style.item(1) + ',' + a.style.item(2)", ",margin-top,margin-right");
    run(&mut b, "a.style.setProperty('--m','7px 8px'); getComputedStyle(a).marginTop + ',' + getComputedStyle(a).marginRight + ',' + getComputedStyle(a).marginBottom", "1px,8px,7px");
    run(&mut b, "a.style.removeProperty('margin-left'); getComputedStyle(a).marginLeft + ',' + getComputedStyle(a).marginRight", "9px,8px");
    run(&mut b, "c.style.cssText = a.style.cssText; getComputedStyle(c).marginRight + ',' + getComputedStyle(a).marginRight", "0px,8px");
    run(&mut b, "a.setAttribute('title', 'kept'); getComputedStyle(a).marginRight", "8px");
    run(&mut b, "a.setAttribute('style', a.getAttribute('style')); getComputedStyle(a).marginRight + ',' + a.style.marginRight", "0px,");
}

#[test]
fn element_pending_priority_and_css_text_replacement() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); a.style.cssText = '--m: 3px 4px; margin: var(--m) !important'; a.style.marginTop = '1px'; a.style.getPropertyPriority('margin-right') + ',' + a.style.getPropertyPriority('margin-top') + ',' + getComputedStyle(a).marginRight", "important,,4px");
    run(&mut b, "a.style.cssText = a.style.cssText; getComputedStyle(a).marginRight", "0px");
}

#[test]
fn published_sheet_pending_state_and_replacement() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); var sh = document.styleSheets[0]; sh.insertRule('#a { --m: 3px 4px; margin: var(--m); margin-top: 1px }', 1); var r = sh.cssRules.item(1); getComputedStyle(a).marginRight + ',' + r.style.marginRight", "4px,");
    run(&mut b, "r.style.setProperty('--m', '7px 8px'); getComputedStyle(a).marginRight", "8px");
    run(&mut b, "r.style.removeProperty('margin-left'); getComputedStyle(a).marginLeft + ',' + getComputedStyle(a).marginRight", "9px,8px");
    run(&mut b, "sh.insertRule('#b { color: red }', 2); getComputedStyle(a).marginRight + ',' + getComputedStyle(document.getElementById('b')).marginRight", "8px,0px");
    run(&mut b, "var st = document.getElementsByTagName('style').item(0); st.textContent = st.textContent; getComputedStyle(a).marginRight", "0px");
}

#[test]
fn pending_state_survives_parse_cache_eviction() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); a.style.cssText = '--m: 3px 4px; margin: var(--m)'; a.style.marginTop = '1px'; for(var i=0;i<1030;i++){ var n = document.createElement('div'); n.style.color = 'red'; } getComputedStyle(a).marginRight + ',' + a.style.marginRight", "4px,");
}

#[test]
fn equal_serialized_sheets_keep_distinct_sources() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); a.style.cssText = '--x: 3px 4px; --y: 5px 6px'; var sh = document.styleSheets[0]; sh.insertRule('#a { margin: var(--x); margin-top: 1px }', 1); getComputedStyle(a).marginRight", "4px");
    run(&mut b, "var st = document.createElement('style'); document.head.appendChild(st); st.sheet.insertRule('#a { margin: var(--y); margin-top: 1px }', 0); getComputedStyle(a).marginRight", "6px");
    run(&mut b, "var r = st.sheet.cssRules.item(0); r.style.margin = 'var(--x)'; r.style.marginTop = '1px'; getComputedStyle(a).marginRight", "4px");
    run(&mut b, "r.style.cssText = r.style.cssText; getComputedStyle(a).marginRight", "4px");
}

#[test]
fn wrapping_properties_have_live_computed_text_and_aliases() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); a.style.overflowWrap = 'anywhere'; a.style.wordBreak = 'break-all'; getComputedStyle(a).overflowWrap + '|' + getComputedStyle(a).wordBreak", "anywhere|break-all");
    run(&mut b, "a.style.wordWrap = 'break-word'; getComputedStyle(a).overflowWrap + '|' + a.style.getPropertyValue('word-wrap') + '|' + getComputedStyle(a).wordWrap", "break-word|break-word|break-word");
}

#[test]
fn wrapping_style_changes_relayout_the_real_element() {
    let mut b = browser();
    run(&mut b, "var a = document.getElementById('a'); a.style.cssText = 'width:200px;font-size:14px;line-height:20px'; a.textContent = 'x'.repeat(120); var before = a.getBoundingClientRect().height; a.style.overflowWrap = 'anywhere'; before + '|' + a.getBoundingClientRect().height", "20|120");
    run(&mut b, "a.style.overflowWrap = 'normal'; a.style.wordBreak = 'break-all'; a.getBoundingClientRect().height", "120");
}

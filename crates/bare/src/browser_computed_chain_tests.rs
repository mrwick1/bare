//! Tests of the lazy chain read of `getComputedStyle` (`RuleSet::style_chain`): a read that needs no layout styles the element and its
//! ancestors, and not the page. The oracle is the full cascade: a second browser that read layout first (`offsetWidth`), so that it
//! keeps the cascade of the whole page and answers from it.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn at(page: &str) -> Browser {
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

/// Every element, every property that needs no layout, and the custom properties: `name:value|...`, in the order of the elements. `order`
/// is the order of the reads: the values are put back in the order of the elements.
const DUMP: &str = "function dump(order) {
  var all = [].slice.call(document.querySelectorAll('*')), n = all.length, idx = [], out = [], i, k;
  for (i = 0; i < n; i++) idx.push(i);
  if (order === 'rev') idx.reverse();
  if (order === 'mix') idx.sort(function (a, b) { return ((a * 37) % n - (b * 37) % n) || a - b; });
  var skip = /^(width|height|margin-|padding-|top$|right$|bottom$|left$|grid-template)/;
  for (k = 0; k < n; k++) {
    var e = all[idx[k]], cs = getComputedStyle(e), parts = [];
    for (i = 0; i < cs.length; i++) { var name = cs.item(i); if (!skip.test(name)) parts.push(name + ':' + cs.getPropertyValue(name)); }
    out[idx[k]] = (e.id || e.tagName) + '{' + parts.join(';') + '}';
  }
  return out.join('|');
}";

const PAGE: &str = "<!doctype html><html id=root><head><style>
html { font-size: 20px }
body { --gap: 3px; --acc: var(--gap); color: #123456; font-size: 1.5em; line-height: 1.5 }
.box { border: 2px solid currentColor; --w: calc(var(--gap) * 2); padding: .5em }
.em { font-size: 2em; text-indent: 1em }
.rem { font-size: 2rem }
#l3.cc { border-color: currentcolor; outline-color: currentColor; color: rgb(10, 20, 30) }
li:nth-child(odd) { color: red }
li:nth-child(3n+2) { background-color: #0f0 }
li + li { opacity: .5 }
ul ~ p { visibility: hidden }
li.em ~ li { text-transform: uppercase }
li:first-child { font-weight: 700 }
li:last-child { font-style: italic }
.on > .kid { color: blue }
.on .deep span { letter-spacing: 2px }
div:has(> em) { text-decoration: underline }
div:has(.flag) { cursor: pointer }
.gone { display: none }
.gone * { color: green }
[data-x=a] { z-index: 3 }
input:checked + label { color: #abcdef }
p:empty { border-top-style: dashed }
.late { word-spacing: 4px }
</style></head><body id=body class=b>
<div id=top class='box on'>
  <ul id=list><li id=l1>a</li><li id=l2 class=em>b</li><li id=l3 class=cc>c</li><li id=l4>d</li><li id=l5>e</li></ul>
  <p id=pp>p</p><p id=pe></p>
  <div class=kid id=k1><em id=e1>x</em></div>
  <div class=deep><div><span id=s1>s</span></div></div>
  <div id=hid class=gone><p id=h1>h</p><div><span id=h2 class=rem>q</span></div></div>
  <div id=fl><span class=flag id=f1></span></div>
  <input type=checkbox id=ck checked><label id=lb>l</label>
  <span id=dx data-x=a class=rem>r</span>
  <span id=st style='color: #ff0; --gap: 9px; font-size: 3em'>s</span>
  <p id=hh hidden>hidden attr</p>
</div></body></html>";

/// A browser that has not read layout, and one that has (it keeps the cascade of the whole page).
fn pair(page: &str) -> (Browser, Browser) {
    let (mut lazy, mut full) = (at(page), at(page));
    for b in [&mut lazy, &mut full] {
        js(b, DUMP);
    }
    settle(&mut full);
    assert_eq!(full.computed_stats().computes, 1, "the oracle keeps a full cascade");
    (lazy, full)
}

/// Make the oracle current: a layout read gives the full cascade of the page (or restyles the kept one).
fn settle(b: &mut Browser) {
    js(b, "document.body.offsetWidth");
}

fn same(a: &str, b: &str, what: &str) {
    if a == b {
        return;
    }
    let (x, y): (Vec<&str>, Vec<&str>) = (a.split('|').collect(), b.split('|').collect());
    for (i, (p, q)) in x.iter().zip(&y).enumerate() {
        if p != q {
            let (p, q): (Vec<&str>, Vec<&str>) = (p.split(';').collect(), q.split(';').collect());
            let d = p.iter().zip(&q).find(|(m, n)| m != n);
            panic!("{what}: element {i} differs: chain {d:?} (of {} and {} parts)", p.len(), q.len());
        }
    }
    panic!("{what}: {} elements against {}", x.len(), y.len());
}

/// All three read orders of `lazy` against the oracle. The lazy browser never makes a full cascade.
fn agree(lazy: &mut Browser, full: &mut Browser, what: &str) {
    settle(full);
    let want = js(full, "dump('fwd')");
    for order in ["fwd", "rev", "mix"] {
        let got = js(lazy, &format!("dump('{order}')"));
        same(&got, &want, &format!("{what}, {order}"));
    }
    assert_eq!(lazy.computed_stats().computes, 0, "{what}: the chain reads made a full cascade");
    assert!(lazy.computed_stats().chains > 0, "{what}: no chain read ran");
}

/// Apply `src` to both, then compare every element in three read orders.
fn step(lazy: &mut Browser, full: &mut Browser, src: &str) {
    let (a, b) = (js(lazy, src), js(full, src));
    assert_eq!(a, b, "the script gave other results: {src}");
    agree(lazy, full, src);
}

#[test]
fn chain_styles_equal_the_full_walk() {
    let (mut lazy, mut full) = pair(PAGE);
    agree(&mut lazy, &mut full, "first read");
    // The values that the page is about: inheritance, `em`, `rem`, `currentColor`, custom properties, siblings. `:has()` is not supported by the
    // engine (`selector.rs`): the `:has` rules of the page are dropped, equally on both paths.
    for (id, prop, want) in [
        ("e1", "color", "rgb(0, 0, 255)"),
        ("l2", "font-size", "60px"),
        ("dx", "font-size", "40px"),
        ("l3", "border-top-color", "rgb(10, 20, 30)"),
        ("l1", "color", "rgb(255, 0, 0)"),
        ("l2", "background-color", "rgb(0, 255, 0)"),
        ("l4", "opacity", "0.5"),
        ("pp", "visibility", "hidden"),
        ("k1", "color", "rgb(0, 0, 255)"),
        ("s1", "letter-spacing", "2px"),
        ("l4", "text-transform", "uppercase"),
        ("lb", "color", "rgb(171, 205, 239)"),
        ("pe", "border-top-style", "dashed"),
        ("st", "--gap", "9px"),
        ("top", "--w", "calc(3px * 2)"),
        ("e1", "--acc", "3px"),
    ] {
        let got = js(&mut lazy, &format!("getComputedStyle(document.getElementById('{id}')).getPropertyValue('{prop}')"));
        let oracle = js(&mut full, &format!("getComputedStyle(document.getElementById('{id}')).getPropertyValue('{prop}')"));
        assert_eq!(got, oracle, "{id} {prop}: chain against full");
        assert_eq!(got, want, "{id} {prop}");
    }
}

#[test]
fn a_chain_read_sees_each_mutation() {
    let (mut lazy, mut full) = pair(PAGE);
    agree(&mut lazy, &mut full, "before");
    // A class on an ancestor: descendant rules, `>` and inheritance.
    step(&mut lazy, &mut full, "document.getElementById('top').className = 'box'; 0");
    step(&mut lazy, &mut full, "document.getElementById('top').className = 'box on late'; 0");
    // A class and an attribute on a sibling: `+`, `~`, `:nth-child`, `:checked`.
    step(&mut lazy, &mut full, "document.getElementById('l2').className = 'cc'; document.getElementById('l5').setAttribute('data-x', 'a'); 0");
    step(&mut lazy, &mut full, "document.getElementById('ck').checked = false; document.getElementById('ck').removeAttribute('checked'); 0");
    // The tree: a node that is added or removed moves `:nth-child`, `:last-child`, `:empty` and `:has`.
    step(&mut lazy, &mut full, "var li = document.createElement('li'); li.id = 'l6'; document.getElementById('list').appendChild(li); 0");
    step(&mut lazy, &mut full, "document.getElementById('list').removeChild(document.getElementById('l1')); 0");
    step(&mut lazy, &mut full, "document.getElementById('pe').appendChild(document.createTextNode('t')); 0");
    step(&mut lazy, &mut full, "var f = document.getElementById('f1'); f.parentNode.removeChild(f); 0");
    // The `style` attribute and a text edit.
    step(&mut lazy, &mut full, "document.getElementById('st').style.color = 'rgb(1, 2, 3)'; document.getElementById('pp').firstChild.data = 'q'; 0");
    // A sheet: a new `<style>` at the end, and a change of the text of one.
    step(&mut lazy, &mut full, "var s = document.createElement('style'); s.textContent = '.late { color: #010203 } li { font-variant: small-caps }'; document.head.appendChild(s); 0");
    step(&mut lazy, &mut full, "document.head.lastChild.textContent = '.late { color: #040506 }'; 0");
    // The root: `rem` moves for every element.
    step(&mut lazy, &mut full, "document.documentElement.style.fontSize = '10px'; 0");
    // A hidden subtree that is shown, and a shown one that is hidden.
    step(&mut lazy, &mut full, "document.getElementById('hid').className = ''; document.getElementById('fl').className = 'gone'; 0");
}

#[test]
fn a_read_after_a_write_in_the_same_script_is_current() {
    let (mut lazy, mut full) = pair(PAGE);
    let src = "var e = document.getElementById('e1'), r = [getComputedStyle(e).color];
        document.getElementById('top').className = 'box'; r.push(getComputedStyle(e).color);
        document.getElementById('k1').style.color = 'rgb(9, 9, 9)'; r.push(getComputedStyle(e).color, getComputedStyle(document.getElementById('k1')).color);
        document.body.className = 'late'; r.push(getComputedStyle(document.getElementById('l3')).wordSpacing);
        document.body.className = 'b'; r.push(getComputedStyle(document.getElementById('l3')).wordSpacing, getComputedStyle(e).fontSize);
        r.join()";
    let (a, b) = (js(&mut lazy, src), js(&mut full, src));
    assert_eq!(a, b);
    assert_eq!(a, "rgb(0, 0, 255),rgb(18, 52, 86),rgb(9, 9, 9),rgb(9, 9, 9),4px,0px,30px");
    assert_eq!(lazy.computed_stats().computes, 0);
}

#[test]
fn a_hidden_element_reads_the_same_by_the_chain() {
    let (mut lazy, mut full) = pair(PAGE);
    for id in ["h1", "h2", "hh", "root"] {
        let src = format!("var cs = getComputedStyle(document.getElementById('{id}')); [cs.color, cs.display, cs.fontSize, cs.visibility, cs.getPropertyValue('--gap')].join()");
        assert_eq!(js(&mut lazy, &src), js(&mut full, &src), "{id}");
    }
    assert_eq!(js(&mut lazy, "getComputedStyle(document.getElementById('h2')).fontSize"), "40px");
    assert_eq!(js(&mut lazy, "getComputedStyle(document.getElementById('h2')).color"), "rgb(0, 128, 0)");
    assert_eq!(lazy.computed_stats().computes, 0);
}

#[test]
fn a_layout_read_after_a_style_read_is_not_changed() {
    let (mut lazy, mut full) = pair(PAGE);
    // The style read first, on the chain. Then reads that need the box: the same values as the oracle, and one full cascade.
    for src in [
        "getComputedStyle(document.getElementById('l2')).color",
        "document.getElementById('l2').getBoundingClientRect().width + ',' + document.getElementById('l2').offsetHeight",
        "getComputedStyle(document.getElementById('l2')).width + ',' + getComputedStyle(document.getElementById('top')).paddingLeft",
        "document.body.className = 'late'; getComputedStyle(document.getElementById('e1')).color + document.getElementById('e1').getBoundingClientRect().height",
        "getComputedStyle(document.getElementById('e1')).margin + getComputedStyle(document.getElementById('top')).height",
    ] {
        assert_eq!(js(&mut lazy, src), js(&mut full, src), "{src}");
    }
    assert_eq!(lazy.computed_stats().computes, 1, "one full cascade for the layout reads");
    // After that every read is answered as before.
    let src = "getComputedStyle(document.getElementById('l3')).color + getComputedStyle(document.getElementById('hid')).display";
    assert_eq!(js(&mut lazy, src), js(&mut full, src));
}

#[test]
fn layout_free_shorthands_and_fixed_lengths_read_on_the_chain() {
    // `margin` and `padding` are in the set of properties that may need layout. A fixed length needs none.
    let (mut lazy, mut full) = pair(PAGE);
    let src = "var e = document.getElementById('l3'), cs = getComputedStyle(e); [cs.margin, cs.paddingTop, cs.top, cs.color].join()";
    assert_eq!(js(&mut lazy, src), js(&mut full, src));
    assert_eq!(lazy.computed_stats().computes, 0, "fixed lengths need no layout");
}

#[test]
fn an_element_that_is_not_in_the_document_has_no_style_as_before() {
    let (mut lazy, mut full) = pair(PAGE);
    let src = "var e = document.createElement('div'); var cs = getComputedStyle(e); [cs.color, cs.display, cs.length].join()";
    assert_eq!(js(&mut lazy, src), js(&mut full, src));
}

#[test]
fn a_page_with_a_shadow_tree_reads_as_before() {
    let page = "<!doctype html><html><head><style>x-a { color: red } p { font-weight: 700 }</style></head><body><x-a id=h><span id=s>l</span></x-a></body></html>";
    let src = "var r = document.getElementById('h').attachShadow({mode: 'open'}); r.innerHTML = '<style>p { color: blue }</style><p id=in>x</p><slot></slot>';
        var cs = getComputedStyle(r.getElementById('in')), ls = getComputedStyle(document.getElementById('s')); [cs.color, cs.fontWeight, ls.color, ls.fontWeight].join()";
    let (mut a, mut b) = (at(page), at(page));
    settle(&mut b);
    assert_eq!(js(&mut a, src), js(&mut b, src));
}

#[test]
fn many_reads_of_one_page_cost_the_ancestors_once() {
    // A loop over the elements in document order styles each once: no more styles than the full cascade makes.
    let mut b = at(PAGE);
    js(&mut b, DUMP);
    js(&mut b, "dump('fwd'); dump('rev'); dump('mix'); 0");
    assert_eq!((b.computed_stats().computes, b.computed_stats().builds), (0, 1));
}

#[test]
fn a_page_with_motion_reads_by_the_whole_cascade() {
    // A read of the whole page starts transitions and animations (`motion_seen`): a chain read does not, so the page keeps the whole cascade.
    let mut b = at("<!doctype html><html><head><style>.t { transition: opacity 1s }</style></head><body><div id=a>x</div></body></html>");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).opacity"), "1");
    assert_eq!((b.computed_stats().computes, b.computed_stats().chains), (1, 0), "a rule has a transition");
    // Only an inline style has it: the style of the element tells.
    let mut b = at("<!doctype html><html><body><div id=a style='transition: opacity 1s'>x</div><div id=b>y</div></body></html>");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('b')).opacity"), "1");
    assert_eq!((b.computed_stats().computes, b.computed_stats().chains), (0, 1));
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).opacity"), "1");
    assert_eq!(b.computed_stats().computes, 1, "the element has a transition");
    // The transition starts from the value that was read: the same as when the whole page was read first.
    let page = "<!doctype html><html><body><div id=a style='transition: opacity 1s'>x</div><div id=b>y</div></body></html>";
    let src = "var a = document.getElementById('a'); getComputedStyle(a).opacity; a.style.opacity = '0'; getComputedStyle(a).opacity";
    let (mut lazy, mut full) = (at(page), at(page));
    js(&mut full, "getComputedStyle(document.body).width; 0");
    assert_eq!(js(&mut lazy, src), js(&mut full, src));
}

//! Tests of the reclaim of dead trees (`browser_reclaim.rs`). A tree that no wrapper, range, observer or other state names goes back to the arena;
//! a tree that one of them names stays. Each test runs on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><body><div id=a><p id=p>hi</p><input id=i></div><select id=s><option>a<option>b</select></body></html>";

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

fn live(b: &Browser) -> usize {
    b.document().borrow().live_len()
}

/// A script that parses `n` documents of about 150 nodes and keeps none.
const PARSE: &str = "function parse(n) { for (var i = 0; i < n; i++) { var d = new DOMParser().parseFromString('<div>' + '<p class=c>x<b>y</b></p>'.repeat(30) + '</div>', 'text/html'); d.querySelector('p').textContent; } } parse(";

#[test]
fn the_nodes_of_documents_that_died_go_back_to_the_arena() {
    big(|| {
        let mut b = browser();
        let base = live(&b);
        js(&mut b, &format!("{PARSE}40)"));
        assert!(live(&b) > base + 40 * 100, "the arena grew: {}", live(&b));
        b.collect(&[]).unwrap();
        assert!(live(&b) < base + 300, "dead documents left {} nodes", live(&b) - base);
        let (runs, freed) = b.reclaim_stats();
        assert!(runs >= 1 && freed > 40 * 100, "runs {runs} freed {freed}");
        // the page still works, and the freed slots are used again
        let slots = b.document().borrow().len();
        js(&mut b, &format!("{PARSE}40)"));
        assert!(b.document().borrow().len() <= slots + 300, "slots grew from {slots} to {}", b.document().borrow().len());
        assert_eq!(js(&mut b, "document.getElementById('p').textContent"), "hi");
    });
}

#[test]
fn a_document_that_a_script_holds_stays() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var keep = new DOMParser().parseFromString('<p id=k>kept</p>', 'text/html'); keep.mark = 1;");
        js(&mut b, &format!("{PARSE}20)"));
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "keep.getElementById('k').textContent + keep.mark"), "kept1");
        js(&mut b, "keep = null;");
        let before = live(&b);
        // the walk waits for new nodes (`FLOOR`): the parses are those
        js(&mut b, &format!("{PARSE}20)"));
        b.collect(&[]).unwrap();
        assert!(live(&b) < before, "{} nodes, {before} before", live(&b));
    });
}

#[test]
fn the_walk_waits_until_enough_nodes_were_made_since_the_last_one() {
    big(|| {
        let mut b = browser();
        b.dom.reclaim.set_floor(1_000_000);
        js(&mut b, &format!("{PARSE}20)"));
        let made = live(&b);
        b.collect(&[]).unwrap();
        assert_eq!(live(&b), made, "no walk: too few nodes were made");
        assert_eq!(b.reclaim_stats(), (0, 0));
        b.dom.reclaim.set_floor(0);
        b.collect(&[]).unwrap();
        assert!(live(&b) < made, "the walk that was due runs once the floor is met");
    });
}

#[test]
fn a_range_and_an_observer_keep_the_trees_they_name() {
    big(|| {
        let mut b = browser();
        js(
            &mut b,
            "var log = []; (function () { var d = new DOMParser().parseFromString('<p>one<b>two</b></p>', 'text/html'); window.r = d.createRange(); r.setStart(d.querySelector('p').firstChild, 1); r.setEnd(d.querySelector('b').firstChild, 2); \
             window.mo = new MutationObserver(function (m) { log.push(m.length); }); mo.observe(d.querySelector('b'), { childList: true }); })();",
        );
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "r.toString()"), "netw");
        assert_eq!(js(&mut b, "r.commonAncestorContainer.nodeName"), "P");
    });
}

#[test]
fn state_of_freed_nodes_is_forgotten_and_does_not_reach_a_node_that_takes_the_slot() {
    big(|| {
        let mut b = browser();
        // A started script, a dirty input value and a dialog result on trees that die; new nodes then take the slots.
        js(&mut b, "(function () { for (var i = 0; i < 20; i++) { var d = new DOMParser().parseFromString('<script>1</script><input value=v><select><option selected>x</select><dialog></dialog>', 'text/html'); d.querySelector('input').value = 'typed'; d.querySelector('dialog').returnValue = 'r'; } })();");
        // the `selectionchange` task of each input names it: the trees stay until the tasks ran
        b.advance_time(0.0, 100).unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(crate::browser_loader::started_len(&b.dom), 0, "the started flags of freed scripts are gone");
        assert_eq!(js(&mut b, "var s = document.createElement('script'); s.textContent = '1'; s.type"), "");
        assert_eq!(js(&mut b, "var x = document.createElement('input'); x.value"), "");
        assert_eq!(js(&mut b, "document.createElement('dialog').returnValue"), "");
        assert_eq!(js(&mut b, "var o = document.createElement('select'); o.appendChild(document.createElement('option')); o.selectedIndex"), "0");
    });
}

#[test]
fn reclaim_after_every_task_with_poison_does_not_panic() {
    big(|| {
        let mut b = browser();
        b.set_reclaim_stress(true);
        js(&mut b, "var held = []; function work(i) { var d = new DOMParser().parseFromString('<ul><li>a<li>b</ul>', 'text/html'); var li = d.querySelectorAll('li'); if (i % 5 == 0) held.push(li[1]); var e = document.createElement('div'); e.innerHTML = '<span>' + i + '</span>'; if (i % 7 == 0) document.body.appendChild(e); } for (var i = 0; i < 60; i++) setTimeout(work.bind(null, i), i);");
        for _ in 0..80 {
            b.advance_time(1.0, 50).expect("advance");
            b.collect(&[]).expect("collect");
        }
        assert_eq!(js(&mut b, "held.length + ',' + held[2].textContent + ',' + document.querySelectorAll('body > div > span').length"), "12,b,9");
    });
}

#[test]
fn parsed_documents_ask_for_a_collect_by_their_native_bytes() {
    big(|| {
        let mut b = browser();
        // 5 documents of about 1 MiB of text from one source string: the JS heap stays under its own limit, the arena grows by 5 MiB.
        js(&mut b, "var src = '<p>' + 'x'.repeat(1 << 20) + '</p>'; function big(n) { for (var i = 0; i < n; i++) new DOMParser().parseFromString(src, 'text/html'); } big(2)");
        assert!(!b.it.gc_due(), "2 MiB of native bytes are below the limit");
        js(&mut b, "big(3)");
        assert!(b.it.gc_due(), "5 MiB of native bytes ask for a collect");
        b.collect(&[]).unwrap();
        assert!(!b.it.gc_due());
        assert!(live(&b) < 1000, "the five documents are gone");
        assert!(b.document().borrow().arena_bytes() < 64 << 10);
    });
}

/// The caps `dom-nodes`, `dom-text` and `dom-attrs` count the nodes, the text and the attribute slots that are alive, not the length of the arena.
#[test]
fn the_dom_caps_count_live_nodes_text_and_attributes() {
    big(|| {
        let lim = BrowserLimits { max_nodes: 2_000, max_text_bytes: 20_000, max_attr_slots: 600, ..BrowserLimits::default() };
        let mut b = Browser::new(tree::parse(PAGE), lim, EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock_at(1000.0).unwrap();
        // One call parses 5 documents of about 95 nodes, 300 bytes of text and 60 attributes, and keeps none.
        let parse = "function parse() { for (var i = 0; i < 5; i++) new DOMParser().parseFromString('<div>' + '<p class=c id=i>0123456789</p>'.repeat(30) + '</div>', 'text/html'); } parse()";
        // 30 rounds are 14,000 nodes, 45,000 bytes of text and 9,000 attribute slots: far over each cap, and each round alone is under.
        for round in 0..30 {
            let r = b.run_script(parse);
            assert!(r.is_ok(), "round {round}: {:?}", r.err());
            b.collect(&[]).unwrap();
        }
        assert_eq!(js(&mut b, "document.getElementById('p').textContent"), "hi");
        // A cap still stops a page that keeps what it parses.
        let r = b.run_script("var keep = []; for (var i = 0; i < 40; i++) keep.push(new DOMParser().parseFromString('<div>' + '<p class=c id=i>0123456789</p>'.repeat(30) + '</div>', 'text/html'));");
        assert!(r.is_err(), "40 documents that stay are over the caps");
    });
}

//! Tests of `browser_controls.rs` and `browser_textarea.rs`: constraint validation, `labels`, `form.reset()` and `HTMLTextAreaElement`.
//! The Chrome cases are in `tools/fixtures/ui/{validity,textarea,labels,reset}-cases.txt`.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use crate::eng_old::OldFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

topic_test!(chrome_validity, "validity", 83);
topic_test!(chrome_textarea, "textarea", 38);
topic_test!(chrome_labels, "labels", 33);
topic_test!(chrome_reset, "reset", 29);

fn page(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u),
        Err(e) => format!("{e:?}"),
    }
}

fn owners(b: &Browser) -> usize {
    b.dom.st.borrow().owners.slots.iter().filter(|o| o.is_some()).count()
}

#[test]
fn a_validity_state_of_a_dead_element_frees_its_key_and_its_map_entries() {
    let mut b = page("<input id=i required>");
    js(&mut b, "var held = document.getElementById('i'); held.validity.valueMissing;");
    b.collect(&[]).expect("gc");
    // The script holds only the element: its `ValidityState` is garbage already.
    assert_eq!(b.dom.ctl_entries(), (0, 0, 0));
    let keys = owners(&b);
    js(&mut b, "(function () { for (var i = 0; i < 50; i++) { var e = document.createElement('input'); e.required = true; e.validity.valueMissing; e.labels.length; } })();");
    assert_eq!(b.dom.ctl_entries(), (50, 50, 50));
    assert!(owners(&b) >= keys + 150);
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.ctl_entries(), (0, 0, 0));
    assert_eq!(owners(&b), keys);
    assert_eq!(js(&mut b, "held.validity.valueMissing + ',' + held.labels.length"), "true,0");
}

#[test]
fn a_validity_state_is_the_same_object_while_a_script_holds_it() {
    let mut b = page("<input id=i required><select id=s></select>");
    js(&mut b, "var el = document.getElementById('i'), v = el.validity, l = el.labels;");
    b.collect(&[]).expect("gc");
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.ctl_entries(), (1, 1, 1));
    assert_eq!(js(&mut b, "(el.validity === v) + ',' + (el.labels === l) + ',' + v.valueMissing + ',' + l.length"), "true,true,true,0");
    // Nothing holds it any more: the next read makes a new object that answers for the element.
    js(&mut b, "v = null; l = null;");
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.ctl_entries(), (0, 0, 0));
    assert_eq!(js(&mut b, "var w = el.validity; (w === el.validity) + ',' + w.valueMissing + ',' + (el.value = 'x', w.valid)"), "true,true,true");
}

#[test]
fn a_key_reused_after_a_collection_answers_for_the_new_validity_state_only() {
    let mut b = page("<input id=a required><input id=b>");
    js(&mut b, "var a = document.getElementById('a'), bb = document.getElementById('b'); (function () { a.validity.valueMissing; })();");
    b.collect(&[]).expect("gc");
    let slots = b.dom.st.borrow().owners.slots.len();
    // The new object takes the freed slot: the table does not grow, and the object answers for `bb`, not for `a`.
    assert_eq!(js(&mut b, "var vb = bb.validity; vb.valueMissing + ',' + vb.valid"), "false,true");
    assert_eq!(b.dom.st.borrow().owners.slots.len(), slots);
    assert_eq!(b.dom.ctl_entries(), (1, 1, 0));
    assert_eq!(js(&mut b, "var va = a.validity; (va === vb) + ',' + va.valueMissing + ',' + vb.valueMissing"), "false,true,false");
    b.collect(&[]).expect("gc");
    assert_eq!(js(&mut b, "(va === a.validity) + ',' + (vb === bb.validity) + ',' + va.valueMissing + ',' + vb.valid"), "true,true,true,true");
}

#[test]
fn the_shared_objects_of_the_controls_survive_a_collection_that_no_script_reaches() {
    let mut b = page("<label id=l for=i>x</label><input id=i required><button id=b></button><form id=f></form>");
    js(&mut b, "document.getElementById('i').validity; document.getElementById('b').willValidate; document.getElementById('f').reset;");
    for _ in 0..3 {
        b.collect(&[]).expect("gc");
    }
    assert_eq!(js(&mut b, "var i = document.getElementById('i'); [i.validity instanceof ValidityState, i.required, i.willValidate, i.labels.length, i.labels[0] instanceof HTMLLabelElement, i.setCustomValidity === HTMLInputElement.prototype.setCustomValidity].join()"), "true,true,true,1,true,true");
    assert_eq!(js(&mut b, "i.setCustomValidity('no'); [i.validationMessage, i.checkValidity(), document.createElement('select').validity.valid].join()"), "no,false,true");
}

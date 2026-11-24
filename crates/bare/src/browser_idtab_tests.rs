//! Tests of `IdTab`, the table that the host keeps by the slot of an object or a node.

use crate::browser::*;
use bare_html::tree;

#[test]
fn nodes_get_insert_and_get_or_insert() {
    let doc = tree::parse("<body><p>a</p><p>b</p></body>");
    let html = doc.children(doc.root()).next().unwrap();
    let ps: Vec<_> = doc.children(html).collect(); // head and body
    let mut t: IdTab<_, u32> = IdTab::default();
    assert_eq!(*t.get_or_insert(ps[0], 5), 5);
    *t.get_or_insert(ps[0], 9) += 1;
    assert_eq!(t.get(&ps[0]), Some(&6));
    assert_eq!(t.get(&ps[1]), None);
    assert_eq!(t.insert(ps[0], 1), Some(6));
    *t.get_mut(&ps[0]).unwrap() = 3;
    assert_eq!(t.len(), 1);
}

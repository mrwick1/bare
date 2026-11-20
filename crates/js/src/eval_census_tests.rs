//! The layout census (`eval_census.rs`): each bucket counts what it names, and the total is the sum of the buckets.

use super::*;
use crate::heap::Heap;

fn census_after(src: &str, exec: Exec) -> (LayoutCensus, LayoutCensus) {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut it = Interp::realm(Heap::default(), EvalLimits { exec, ..Default::default() }).unwrap();
            let before = it.layout_census();
            it.run_script(&src).unwrap();
            (before, it.layout_census())
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn total_is_the_sum_of_the_buckets() {
    let (c, _) = census_after("1", Exec::Tree);
    assert_eq!(c.total(), c.buckets().iter().map(|b| b.1).sum::<usize>());
    assert!(c.total() > 0 && c.objects > 0 && c.shapes > 0 && c.strings > 0 && c.names > 0);
}

#[test]
fn an_array_adds_elements_and_a_string_adds_units() {
    let (a, b) = census_after("var a = []; for (var i = 0; i < 1000; i++) a.push(i); var s = 'x'.repeat(5000);", Exec::Tree);
    assert!(b.elements >= a.elements + 1000 * 16, "elements {} -> {}", a.elements, b.elements);
    assert!(b.strings >= a.strings + 10000, "strings {} -> {}", a.strings, b.strings);
}

#[test]
fn compiled_code_adds_bytecode_and_feedback_only_in_auto() {
    let src = "function f(o) { return o.x + 1; } var s = 0; for (var i = 0; i < 10; i++) s += f({ x: i });";
    let (_, tree) = census_after(src, Exec::Tree);
    let (before, auto) = census_after(src, Exec::Auto);
    assert_eq!(tree.bytecode, 0);
    assert!(auto.bytecode > before.bytecode && auto.feedback > before.feedback);
}

#[test]
fn objects_with_many_properties_add_slot_vectors() {
    let (a, b) = census_after("var o = {}; for (var i = 0; i < 20; i++) o['k' + i] = i; var p = {a:1,b:2,c:3,d:4,e:5,f:6};", Exec::Tree);
    assert!(b.slots + b.dict > a.slots + a.dict, "slots {} -> {}, dict {} -> {}", a.slots, b.slots, a.dict, b.dict);
}

#[test]
fn a_scripts_tree_and_source_are_counted_once() {
    let (a, b) = census_after("var x = 1;", Exec::Tree);
    assert!(b.ast > a.ast && b.source >= a.source + "var x = 1;".len());
}

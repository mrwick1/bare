// Tests of `error_stack.rs` with a collection between captures: the keys that `ErrStack` keeps must stay alive.
use crate::script;

#[test]
fn the_stack_keys_survive_a_collection_when_no_error_holds_them() {
    // The first capture interns the keys. No error stays alive, so only `ErrStack` refers to them.
    let mut r = script::run("(function () { new Error('x').stack })()").unwrap().realm;
    let kept = r.es.keys.expect("the keys are made");
    r.collect(&mut [], |_| {});
    assert!(r.intern("stack") == kept.2, "the cached key `stack` is still the atom of its text");
    assert!(r.intern("\0stk") == kept.1, "the cached key `\\0stk` is still the atom of its text");
    assert!(r.intern("stackTraceLimit") == kept.0, "the cached key `stackTraceLimit` is still the atom of its text");
}

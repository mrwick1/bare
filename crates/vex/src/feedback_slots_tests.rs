// Tests of the feedback slots (`docs/internal/design-feedback-table.md`): a code gets its feedback at its first run, so a code that
// never ran costs one word in each machine.
use super::*;
use crate::interp_rt::Feedback;

fn made(fbs: &[Option<Box<Feedback>>]) -> usize {
    fbs.iter().filter(|f| f.is_some()).count()
}

#[test]
fn a_function_that_never_ran_has_no_feedback() {
    let vm = run("function a() { return 1; } function b() { return 2; } function c() { return 3; } var r = a() + a();").unwrap();
    assert_eq!(made(&vm.fbs), 2, "the script and `a` ran");
    assert!(vm.fbs.len() >= 4, "a slot per code");
}


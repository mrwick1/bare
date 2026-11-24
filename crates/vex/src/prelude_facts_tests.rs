//! Check the generated facts and the names used by the prelude slots.

use crate::facts::Facts;
use crate::parse::{Limits, parse_script_lazy};
use crate::script::PRELUDE;

/// Each name in `prelude_pre.rs` and in a lazy table that asks for a top-level function must be one.
#[test]
fn every_prelude_slot_function_exists() {
    for (name, pd) in crate::prelude_pre::PRE {
        if let crate::prelude_pre::Pd::Func(top) = pd {
            assert_eq!(name, top);
            assert!(crate::prelude_facts::TOP.iter().any(|(n, _)| n == top), "prelude.js has no function {top}");
        }
    }
}

#[test]
fn prelude_facts_equal_a_fresh_pre_parse() {
    let mut fresh = Facts::new();
    parse_script_lazy(PRELUDE, Limits::default(), &mut fresh).unwrap();
    let k = crate::prelude_facts::facts();
    assert_eq!(format!("{:?}", fresh), format!("{:?}", k));
}

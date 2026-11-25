use super::{Program, Vm};
use crate::bytecode::Code;
use std::sync::Arc;

#[test]
#[should_panic(expected = "invalid bytecode: function 0: no code")]
fn constructor_rejects_an_empty_function() {
    Vm::new(Program { codes: vec![Code::new(0, 0)] }, Arc::default());
}

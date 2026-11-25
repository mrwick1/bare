use crate::script::load;

#[test]
fn a_var_declaration_reuses_the_implicit_arguments_binding() {
    let src = "function f() { return arguments[0]; var arguments = []; } var result = f(42);";
    let mut vm = load(src).unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("result"), "42");
}

#[test]
fn an_arrow_var_named_arguments_starts_as_undefined() {
    let src = "var f = () => { return typeof arguments; var arguments = []; }; var result = f();";
    let mut vm = load(src).unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("result"), "undefined");
}

use super::{SharedAtoms, compile_all};
use crate::{parse::Limits, script::load, verify::verify};

fn check(src: &str, expected: &str) {
    let mut vm = load(src).expect("load script");
    vm.run_main(0).expect("run script");
    vm.drain_microtasks(&mut |_, _| {}).expect("run jobs");
    assert_eq!(vm.global_text("result"), expected);

    let (script, functions) = compile_all(src, &SharedAtoms::default(), Limits::default()).expect("compile eagerly");
    for compiled in std::iter::once(script).chain(functions.into_iter().map(|(_, f)| f)) {
        verify(&compiled.code).unwrap_or_else(|e| panic!("{e}\n{}", compiled.code.disassemble()));
    }
}

#[test]
fn generator_default_reads_arguments_object() {
    check("function* g(x = arguments) { function arguments() {} yield x; } var result = typeof g().next().value;", "object");
}

#[test]
fn default_arrow_retains_parameter_scope() {
    check(
        "function f(a = () => arguments) { function arguments() {} return [a, () => arguments]; } var pair = f(undefined, 7); var result = pair[0]()[1] + ':' + typeof pair[1]();",
        "7:function",
    );
}

#[test]
fn plain_function_default_reads_arguments_object() {
    check("function f(x = arguments) { function arguments() { return 'body'; } return x[1] + ':' + arguments(); } var result = f(undefined, 7);", "7:body");
}

#[test]
fn async_function_default_reads_arguments_object() {
    check(
        "async function f(x = arguments) { function arguments() { return 'body'; } await 0; return x[1] + ':' + arguments(); } var result; f(undefined, 7).then(x => result = x);",
        "7:body",
    );
}

#[test]
fn computed_parameter_key_reads_arguments_object() {
    check(
        "var o = { m({ [arguments[1]]: x }) { function arguments() { return 'body'; } return x + ':' + arguments(); } }; var result = o.m({ k: 7 }, 'k');",
        "7:body",
    );
}

#[test]
fn body_function_wins_without_parameter_expressions() {
    check("function f(x) { function arguments() { return 'body'; } return typeof arguments + ':' + arguments(); } var result = f(7);", "function:body");
    check("function f({x}, ...rest) { function arguments() { return 'body'; } return x + ':' + arguments(); } var result = f({x: 7});", "7:body");
}

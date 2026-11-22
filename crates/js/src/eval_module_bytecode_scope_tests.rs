use super::*;

#[test]
fn module_imports_and_default_export_do_not_shift_bytecode_closure_bindings() {
    for exec in [Exec::Tree, Exec::Auto, Exec::Byte] {
        let mut it = Interp::realm(Heap::default(), EvalLimits { exec, ..Default::default() }).unwrap();
        let dep = it.module_parse("export let value = 11;", "dep").unwrap();
        let module = it.module_parse(
            "import * as dep from 'dep';
             export default 17;
             function earlier() { return 19; }
             function wrong() { return 23; }
             function target() { return 42; }
             function run() { return target(); }
             globalThis.result = run();",
            "main",
        ).unwrap();
        it.module_set_requested(module, &[dep]);
        it.module_link(module).unwrap();
        it.module_evaluate(module).unwrap();
        let result = it.run_script("globalThis.result").unwrap();
        assert!(matches!(result, Value::Number(42.0)), "{exec:?}: {result:?}");
    }
}

#[test]
fn bytecode_closures_read_live_named_imports() {
    for exec in [Exec::Tree, Exec::Auto, Exec::Byte] {
        let mut it = Interp::realm(Heap::default(), EvalLimits { exec, ..Default::default() }).unwrap();
        let dep = it.module_parse("export let value = 11; export function update() { value = 42; }", "dep").unwrap();
        let module = it.module_parse(
            "import { value, update } from 'dep';
             function current() { return typeof value + ':' + value; }
             globalThis.before = current();
             update();
             globalThis.after = current();",
            "main",
        ).unwrap();
        it.module_set_requested(module, &[dep]);
        it.module_link(module).unwrap();
        it.module_evaluate(module).unwrap();
        let result = it.run_script("globalThis.before + ';' + globalThis.after").unwrap();
        assert_eq!(it.display(result), "number:11;number:42", "{exec:?}");
    }
}

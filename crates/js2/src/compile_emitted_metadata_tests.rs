use super::*;

#[test]
fn flags_from_emission_match_the_decoder() {
    let source = r#"
        function plain(a) { return a; }
        function args(a) { return arguments[0] + a; }
        function rest(...xs) { return plain(...xs); }
        function* gen(a) { yield a; }
        async function wait() { await plain(1); }
        class Base { get x() { return 2; } }
        class Derived extends Base {
            constructor(...a) { super(...a); }
            method() { return super.x; }
        }
    "#;
    let (_, children) = compile_all(source, &SharedAtoms::default(), crate::parse::Limits::default()).unwrap();
    let mut flags = 0;
    for (_, f) in children {
        let expected = crate::interp::code_flags(&f.code);
        assert_eq!(f.code.flags, expected, "{}", f.code.disassemble());
        flags |= expected;
    }
    assert_eq!(flags, crate::bytecode::CF_ARGS | crate::bytecode::CF_GEN | crate::bytecode::CF_LINK);
}

#[test]
fn maps_keep_exact_bits_across_register_words_and_joins() {
    let atoms = SharedAtoms::default();
    let prog = Program::new("");
    let facts = Facts::new();
    let mut f = Fc::new(&prog, &facts, &atoms, &Input::default());

    f.window(130);
    for r in [0, 63, 64, 128] {
        f.set_live(r);
    }
    f.op(Op::NewObject, 20, 0, 0);
    let join = f.label();
    f.jmp_r(Op::JmpIf, 0, join);
    f.set_live(20);
    f.bind(join);
    f.op(Op::NewObject, 21, 0, 0);
    f.free(65);
    f.op(Op::NewObject, 22, 0, 0);
    let maps = f.finish(0).unwrap().code.roots;
    assert_eq!(maps.pcs().len(), 3);
    let slots = |i: usize| maps.find(maps.pcs()[i]).unwrap().slots().collect::<Vec<_>>();
    assert_eq!(slots(0), [0, 63, 64, 128]);
    assert_eq!(slots(1), [0, 63, 64, 128]);
    assert_eq!(slots(2), [0, 63, 64]);
}

#[test]
fn a_throwing_accessor_can_collect_with_a_live_caller() {
    let source = r#"
        function run() {
            let keep = {value: 39};
            let obj = {get x() {
                let count = 0;
                for (var i = 0; i < 6000; i++) {
                    let item = {n: i};
                    count += item.n === i;
                }
                throw count === 6000 ? {delta: 3} : {delta: 0};
            }};
            try { return obj.x; }
            catch (e) { return keep.value + e.delta; }
        }
        var result = run();
    "#;
    let mut vm = crate::script::load(source).unwrap();
    vm.realm.heap.set_trigger(crate::heap::Trigger { min: 1024, slack: 1024 });
    vm.run_main(0).unwrap();
    assert!(vm.realm.collections > 0);
    assert_eq!(vm.global_text("result"), "42");
}

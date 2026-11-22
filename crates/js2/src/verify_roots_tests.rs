use super::*;
use crate::atoms::SharedAtoms;

#[test]
fn postfix_update_keeps_the_old_numeric_value() {
    let shared = SharedAtoms::default();
    let source = "function inc(x) { return x++; } function dec(x) { return x--; }";
    let prog = crate::script::compile(source, &shared, Default::default()).unwrap();
    for code in &prog.codes[1..] {
        let (_, len) = decode(&code.words, 0).unwrap();
        let pc = len;
        let (update, _) = decode(&code.words, pc).unwrap();
        assert!(matches!(update.op, Op::Inc | Op::Dec));
        assert!(code.roots.find(pc as u32).unwrap().slots().any(|r| r == update.b as usize), "{}", code.disassemble());
    }
}

#[test]
fn postfix_bigint_under_collection() {
    let mut vm = crate::script::load(
        "function f(x) { return x++; }
         var r = f({ valueOf() { return 2n ** 200n; } });",
    )
    .unwrap();
    vm.realm.heap.set_trigger(crate::heap::Trigger { min: 1, slack: 1 });
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("r"), "1606938044258990275541962092341162602522202993782792835301376");
}

fn assemble(nregs: u16, nparams: u16, emit: impl FnOnce(&mut crate::bytecode::Asm)) -> Code {
    let mut asm = crate::bytecode::Asm::default();
    emit(&mut asm);
    let out = asm.finish().unwrap();
    let mut code = Code::new(nregs, nparams);
    code.words = out.words;
    code.loops = out.loops;
    code
}

#[test]
fn a_call_keeps_a_value_used_after_the_call() {
    let mut code = assemble(3, 2, |a| {
        a.emit(Ins::new(Op::CallU, 2, 0, 0));
        a.emit(Ins::new(Op::Ret, 1, 0, 0));
    });
    code.roots.add(0, [0]);
    assert_eq!(verify(&code), Err("0000 CallU: root map misses live r1".into()));
}

#[test]
fn a_call_keeps_the_old_destination_for_its_handler() {
    let mut code = assemble(3, 2, |a| {
        a.emit(Ins::new(Op::CallU, 1, 0, 0));
        a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
        a.emit(Ins::new(Op::Ret, 1, 0, 0));
    });
    code.handlers.push(crate::bytecode::Handler { start: 0, end: 1, target: 2, reg: 2, depth: 0 });
    code.roots.add(0, [0]);
    assert_eq!(verify(&code), Err("0000 CallU: root map misses live r1".into()));
}

#[test]
fn a_safepoint_keeps_a_value_needed_on_resume() {
    let mut code = assemble(3, 2, |a| {
        a.emit(Ins::new(Op::GenInit, 0, 0, 0));
        a.emit(Ins::new(Op::CallU, 2, 0, 0));
        a.emit(Ins::new(Op::Yield, 2, 5, 0));
        a.emit(Ins::new(Op::RetGen, 1, 0, 0));
    });
    code.noctor = true;
    code.gen_kind = GK_GEN;
    code.gen_regs = 2;
    code.tables = vec![NO_REG, NO_REG, 2, 0, 1, NO_REG, NO_REG, 1, 1];
    code.roots.add(0, [0, 1]);
    code.roots.add(1, [0]);
    code.roots.add(2, [1, 2]);
    code.roots.add(3, [1]);
    assert_eq!(verify(&code), Err("0001 CallU: root map misses live r1".into()));
}

#[test]
fn a_back_edge_keeps_a_value_read_at_the_loop_header() {
    let mut code = assemble(2, 1, |a| {
        let head = a.label();
        a.bind(head);
        a.emit(Ins::new(Op::CallU, 1, 0, 0));
        a.jump(Ins::new(Op::JmpBack, 0, 0, 0), head);
    });
    code.roots.add(0, [0]);
    code.roots.add(1, []);
    assert_eq!(verify(&code), Err("0001 JmpBack: root map misses live r0".into()));
}

#[test]
fn compiled_roots_cover_source_corpus() {
    let shared = SharedAtoms::default();
    for source in [
        "function* g(a) { try { yield a; } finally { return a; } }",
        "async function f(a) { try { return await a(); } catch(e) { return a; } }",
        "async function* g(a) { yield* a; }",
        "function f(a) { a.x = a.y.z += a.w; }",
        "function f(a) { try { a = a(); } finally { a.x; } return a; }",
        "class A { m() { return {}; } } class B extends A { x = 1; m() { return super.m(); } }",
        "function f(a) { switch (a) { case 1: return {}; case 2: return []; case 3: return a; } }",
        "function f(a) { while (a.test()) { a.step(); } return a; }",
        include_str!("../examples/data/eval_webpack.js"),
    ] {
        let prog = crate::script::compile(source, &shared, Default::default()).unwrap_or_else(|e| panic!("{source}\n{e}"));
        verify_program(&prog.codes).unwrap_or_else(|e| panic!("{source}\n{e}"));
    }
}

#[test]
fn resolved_child_indexes_must_name_a_code() {
    let mut parent = assemble(1, 0, |a| {
        a.emit(Ins::new(Op::Closure, 0, 0, 0));
        a.emit(Ins::new(Op::Ret, 0, 0, 0));
    });
    parent.roots.add(0, []);
    parent.funcs.push(1);
    let child = assemble(0, 0, |a| _ = a.emit(Ins::new(Op::RetUndef, 0, 0, 0)));
    let mut codes = [parent, child];
    verify_program(&codes).unwrap();
    codes[0].funcs[0] = 2;
    assert!(verify_program(&codes).unwrap_err().contains("no child function 2"));
}

#[test]
fn lazy_stubs_match_the_argument_mode() {
    let vm = crate::script::load("function pair(x, y) {} function rest(...xs) {} function* gen(x) {}").unwrap();
    for &i in &vm.realm.prog[0].funcs {
        let code = &vm.realm.prog[i as usize];
        assert_eq!(decode(&code.words, 0).unwrap().0.op, Op::Lazy);
        verify(code).unwrap();
    }

    let mut stub = assemble(1, 1, |a| _ = a.emit(Ins::new(Op::Lazy, 0, 0, 0)));
    stub.fn_len = 1;
    verify(&stub).unwrap();
    stub.flags = CF_ARGS;
    assert_eq!(verify(&stub), Err("invalid lazy stub metadata".into()));
    stub.flags = 0;
    stub.fn_name = 0;
    assert_eq!(verify(&stub), Err("the function name has no atom".into()));
}

#[test]
fn a_template_recipe_cannot_double_as_object_keys() {
    let mut code = assemble(2, 1, |a| {
        a.emit(Ins::new(Op::GetTemplate, 1, 0, 0));
        a.emit(Ins::new(Op::NewObjLit, 0, 0, 0));
        a.emit(Ins::new(Op::Ret, 0, 0, 0));
    });
    let shared = SharedAtoms::default();
    code.atoms.push(shared.intern(crate::string::Chars::One(b"key")).unwrap());
    code.sites.push(0);
    code.recipes.push(&[u32::MAX, 0]);
    assert!(verify(&code).unwrap_err().contains("used as both kinds"));
}

#[test]
fn a_later_map_cannot_mark_a_value_omitted_at_an_earlier_safepoint() {
    let mut code = assemble(3, 2, |a| {
        a.emit(Ins::new(Op::CallU, 2, 0, 0));
        a.emit(Ins::new(Op::CallU, 2, 0, 0));
        a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    });
    code.roots.add(0, [0]);
    code.roots.add(1, [0, 1]);
    assert_eq!(verify(&code), Err("0000 CallU: root map misses live r1".into()));
}

#[test]
fn operand_table_uses_single_letters_and_one_space() {
    for op in (0..=u8::MAX).map_while(Op::from_u8) {
        let spec = op.spec();
        assert!(spec.is_empty() || spec.split(' ').all(|letter| letter.len() == 1));
        for i in 0..4 {
            assert_eq!(op.letter(i), spec.chars().filter(|&c| c != ' ').nth(i).unwrap_or('-'), "{op:?}");
        }
    }
}

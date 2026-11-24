use super::*;

thread_local! {
    pub(super) static ENCODE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn straight_line_code_encodes_each_instruction_once() {
    let mut asm = Asm::default();
    let start = asm.label();
    let middle = asm.label();
    let end = asm.label();
    asm.label(); // An unused label may remain unbound.
    asm.bind(start);
    let ins = [
        Ins::new(Op::LoadI, 1, (-300i32) as u32, 0),
        Ins::new(Op::GetCtxField, 300, 2, 3).with_x(9),
        Ins::new(Op::Ret, 1, 0, 0),
    ];
    asm.emit(ins[0]);
    asm.bind(middle);
    asm.emit(ins[1]);
    asm.emit(ins[2]);
    asm.bind(end);

    let mut expected = Vec::new();
    for i in &ins {
        encode(&mut expected, i).unwrap();
    }
    ENCODE_COUNT.set(0);
    let code = asm.finish().unwrap();
    assert_eq!(code.words, expected);
    assert_eq!(code.pcs, [0, 1, 4, 5]);
    assert_eq!(code.label_pcs, [0, 1, 5, u32::MAX]);
    assert!(code.loops.is_empty());
    assert_eq!(ENCODE_COUNT.get(), ins.len());
}

#[test]
fn straight_line_assembly_matches_relaxation_for_raw_operands() {
    let assemble = || {
        let mut asm = Asm::default();
        for &op in Op::ALL {
            if op == Op::Wide {
                continue;
            }
            let end = asm.label();
            asm.emit(Ins::new(op, 1, 0, 0));
            asm.emit(Ins::new(op, 300, 0, 0));
            asm.bind(end);
        }
        asm
    };
    let before = assemble().finish_relaxed().unwrap();
    let after = assemble().finish().unwrap();
    assert_eq!(after.words, before.words);
    assert_eq!(after.pcs, before.pcs);
    assert_eq!(after.label_pcs, before.label_pcs);
    let mut invalid = Asm::default();
    invalid.emit(Ins::new(Op::Mov, 65536, 0, 0));
    assert!(matches!(invalid.finish(), Err(BcError::Operand)));
}

#[test]
fn static_operands_match_each_opcode_spec() {
    for &op in Op::ALL {
        let letters: Vec<_> = op.spec().chars().filter(|&c| c != ' ').collect();
        for i in 0..5 {
            assert_eq!(op.letter(i), letters.get(i).copied().unwrap_or('-'), "{op:?}");
        }
        assert_eq!(op.jump_field(), letters.iter().position(|&c| c == 'j'), "{op:?}");
    }
}

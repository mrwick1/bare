// Tests of `bytecode.rs`: the op table, the encoder and decoder, the tables and the assembler.
use super::*;
use std::collections::HashSet;

#[test]
fn the_op_table_is_consistent() {
    assert!(Op::ALL.len() < 255, "op numbers and the Wide prefix share one byte");
    let mut names = HashSet::new();
    for (i, &op) in Op::ALL.iter().enumerate() {
        assert_eq!(op as usize, i);
        assert_eq!(Op::from_u8(i as u8), Some(op));
        assert!(names.insert(op.name()), "{} twice", op.name());
        let letters: Vec<char> = op.spec().chars().filter(|l| *l != ' ').collect();
        assert!(letters.len() <= op.form().fields(), "{}: more letters than fields", op.name());
        assert!(letters.iter().all(|l| "wrWRiujzNAFHST-".contains(*l)), "{}: a letter", op.name());
        assert!(letters.iter().filter(|l| **l == 'j').count() <= 1, "{}: two jump fields", op.name());
        // The field `a` of the forms with three narrow fields is never signed, and a window letter is alone.
        assert!(!matches!(op.letter(0), 'i') || op.form() == Form::Sj, "{}", op.name());
        assert!(letters.iter().filter(|l| "RW".contains(**l)).count() <= 1, "{}: two windows", op.name());
        // A jump op has the form that holds its offset: Sj, AsBx, AsBxX in `b`, AbcX in `x`.
        if let Some(f) = op.jump_field() {
            assert!(matches!((op.form(), f), (Form::Sj, 0) | (Form::AsBx | Form::AsBxX, 1) | (Form::AbcX, 3)), "{}", op.name());
        }
    }
    assert_eq!(Op::from_u8(Op::ALL.len() as u8), None);
    assert!(Op::JmpBack.is_safepoint() && Op::GetField.is_safepoint() && Op::Call.is_safepoint() && Op::Add.is_safepoint());
    assert!(!Op::Mov.is_safepoint() && !Op::JSEqNull.is_safepoint() && !Op::GetCtx.is_safepoint() && !Op::Ret.is_safepoint());
    assert!(!Op::Jmp.falls_through() && !Op::JmpBack.falls_through() && !Op::Throw.falls_through() && Op::JmpIf.falls_through());
    // A switch ends the block (all its targets are in the table) and cannot allocate or throw.
    assert!(!Op::SwitchI.falls_through() && !Op::SwitchA.falls_through() && !Op::SwitchI.is_safepoint() && !Op::SwitchA.is_safepoint());
}

/// An instruction with every field set to `v` (a field that the spec leaves out is 0).
fn all(op: Op, v: u32) -> Ins {
    let mut i = Ins::new(op, 0, 0, 0);
    for k in 0..op.form().fields() {
        if op.letter(k) != '-' {
            set(&mut i, k, v);
        }
    }
    i
}

fn set(i: &mut Ins, k: usize, v: u32) {
    match (i.op.form(), k) {
        (_, 0) => i.a = v,
        (_, 1) => i.b = v,
        (Form::AsBxX, 2) | (Form::AbcX, 3) => i.x = v,
        _ => i.c = v,
    }
}

#[test]
fn every_op_round_trips_with_narrow_and_wide_operands() {
    let values: [u32; 14] = [0, 1, 127, 128, 255, 256, 32767, 32768, 65535, 65536, 1 << 23, 1 << 31, u32::MAX, -1i32 as u32];
    for &op in Op::ALL {
        if op == Op::Wide {
            continue;
        }
        for &v in &values {
            // Every field at `v`, then each field at `v` alone with the others at 1 (or -1 for a signed one).
            let mut cases = vec![all(op, v)];
            for k in 0..op.form().fields() {
                let mut i = Ins::new(op, 0, 0, 0);
                for j in 0..op.form().fields() {
                    if op.letter(j) != '-' {
                        set(&mut i, j, if matches!(op.letter(j), 'i' | 'j') { -1i32 as u32 } else { 1 });
                    }
                }
                if op.letter(k) != '-' {
                    set(&mut i, k, v);
                    cases.push(i);
                }
            }
            for ins in cases {
                let mut words = vec![0xdead_beef];
                match encode(&mut words, &ins) {
                    Ok(n) => {
                        assert_eq!(words.len(), 1 + n);
                        let (back, len) = decode(&words[1..], 0).unwrap();
                        assert_eq!((back, len), (ins, n), "{ins:?}");
                        // The narrow form is used when it fits: no `Wide` prefix then.
                        let narrow = words[1] as u8 != Op::Wide as u8;
                        assert_eq!(narrow, n == 1 + op.form().has_x() as usize, "{ins:?}");
                    }
                    Err(e) => {
                        assert_eq!(e, BcError::Operand);
                        assert_eq!(words.len(), 1, "an error writes nothing");
                        // It must really not fit the wide form.
                        let widths = op.form().widths(true);
                        assert!((0..widths.len()).any(|k| !fits(ins.field(k), widths[k], signed(op, k))), "{ins:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_sizes_of_the_common_instructions() {
    let len = |i: Ins| encode(&mut Vec::new(), &i).unwrap();
    assert_eq!(len(Ins::new(Op::Mov, 1, 2, 0)), 1);
    assert_eq!(len(Ins::new(Op::GetField, 1, 2, 200)), 1, "no name operand, no cache number");
    assert_eq!(len(Ins::new(Op::SetFieldK, 1, 3, 4)), 1);
    assert_eq!(len(Ins::new(Op::Call, 1, 2, 3)), 1);
    assert_eq!(len(Ins::new(Op::AddI, 1, 2, -1i32 as u32)), 1);
    assert_eq!(len(Ins::new(Op::LoadI, 1, -300i32 as u32, 0)), 1);
    assert_eq!(len(Ins::new(Op::JSNeNull, 1, 5, 0)), 1, "a compare with null and a jump is one word");
    assert_eq!(len(Ins::new(Op::JSNeI, 1, 5, 0).with_x(70000)), 2);
    assert_eq!(len(Ins::new(Op::JnLt, 1, 2, 0).with_x(-3i32 as u32)), 2);
    assert_eq!(len(Ins::new(Op::Mov, 300, 2, 0)), 2);
    assert_eq!(len(Ins::new(Op::GetField, 1, 2, 300)), 2);
    assert_eq!(len(Ins::new(Op::GetCtxField, 300, 2, 3).with_x(9)), 3);
    assert_eq!(len(Ins::new(Op::LoadN, 1, 70000, 0)), 2);
    assert_eq!(len(Ins::new(Op::JmpBack, -40000i32 as u32, 0, 0)), 1);
    assert_eq!(len(Ins::new(Op::JmpBack, -(1 << 24) as u32, 0, 0)), 2);
    // An 8-bit immediate and a 16-bit register do not fit together in the narrow form, so the wide form holds both.
    let mut w = Vec::new();
    encode(&mut w, &Ins::new(Op::AddI, 1, 2, -200i32 as u32)).unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(decode(&w, 0).unwrap().0.c as i32, -200);
}

#[test]
fn operands_that_fit_no_form_are_an_error() {
    let mut w = Vec::new();
    assert_eq!(encode(&mut w, &Ins::new(Op::Mov, 65536, 0, 0)), Err(BcError::Operand));
    assert_eq!(encode(&mut w, &Ins::new(Op::AddI, 0, 0, 40000)), Err(BcError::Operand));
    assert_eq!(encode(&mut w, &Ins::new(Op::LoadI, 65536, 0, 0)), Err(BcError::Operand));
    assert!(w.is_empty());
}

#[test]
fn decode_rejects_bad_words() {
    assert_eq!(decode(&[], 0), Err(BcError::End));
    assert_eq!(decode(&[255], 0), Err(BcError::BadOp(255)));
    let wide = |op: Op| Op::Wide as u32 | (op as u32) << 8;
    assert_eq!(decode(&[wide(Op::Mov)], 0), Err(BcError::BadWide), "cut off");
    assert_eq!(decode(&[wide(Op::Wide), 0], 0), Err(BcError::BadWide), "Wide of Wide");
    assert_eq!(decode(&[255 << 8 | Op::Wide as u32, 0], 0), Err(BcError::BadWide), "no such op after Wide");
    assert_eq!(decode(&[wide(Op::GetCtxField), 0], 0), Err(BcError::BadWide), "the x word is cut off");
    assert_eq!(decode(&[Op::JnLt as u32], 0), Err(BcError::End), "the x word is missing");
}

#[test]
fn recipes_keep_the_key_lists() {
    let mut r = Recipes::default();
    assert!(r.is_empty() && r.get(0).is_none());
    assert_eq!(r.push(&[3, 1, 2]), 0);
    assert_eq!(r.push(&[]), 1);
    assert_eq!(r.push(&[7]), 2);
    assert_eq!((r.len(), r.get(0), r.get(1), r.get(2), r.get(3)), (3, Some(&[3, 1, 2][..]), Some(&[][..]), Some(&[7][..]), None));
}

fn decode_all(words: &[u32]) -> Vec<(usize, Ins, usize)> {
    let mut v = Vec::new();
    let mut pc = 0;
    while pc < words.len() {
        let (i, n) = decode(words, pc).unwrap();
        v.push((pc, i, n));
        pc += n;
    }
    v
}

#[test]
fn the_assembler_fills_offsets_and_builds_the_loop_table() {
    let mut a = Asm::default();
    let (head, exit, _unused) = (a.label(), a.label(), a.label());
    a.emit(Ins::new(Op::LoadI, 1, 0, 0));
    a.bind(head);
    let test = a.jump(Ins::new(Op::JnLtI, 1, 0, 0).with_x(10), exit);
    a.emit(Ins::new(Op::AddI, 1, 1, 1));
    let back = a.jump(Ins::new(Op::JmpBack, 0, 0, 0), head);
    a.bind(exit);
    let ret = a.emit(Ins::new(Op::Ret, 1, 0, 0));
    let out = a.finish().unwrap();
    assert_eq!(out.pcs, [0, 1, 3, 4, 5, 6]);
    assert_eq!(out.label_pcs[..2], [1, 5]);
    assert_eq!(out.label_pcs[2], u32::MAX);
    let ins = decode_all(&out.words);
    assert_eq!(ins.len(), 5);
    let (pc, i, len) = ins[test];
    assert_eq!((pc as u32, i.target(pc, len)), (out.pcs[test], Some(5)));
    let (pc, i, len) = ins[back];
    assert_eq!((pc as u32, i.target(pc, len)), (out.pcs[back], Some(1)));
    assert_eq!(ins[ret].1.op, Op::Ret);
    assert_eq!(out.loops, [LoopHeader { head: 1, end: 4 }]);
}

#[test]
fn two_back_edges_to_one_header_and_nested_loops() {
    let mut a = Asm::default();
    let (outer, inner, exit) = (a.label(), a.label(), a.label());
    a.bind(outer);
    a.jump(Ins::new(Op::JmpIf, 0, 0, 0), exit);
    a.bind(inner);
    a.emit(Ins::new(Op::Nop, 0, 0, 0));
    a.jump(Ins::new(Op::JmpIf, 0, 0, 0), exit);
    a.jump(Ins::new(Op::JmpBack, 0, 0, 0), inner);
    a.jump(Ins::new(Op::JmpBack, 0, 0, 0), inner);
    a.jump(Ins::new(Op::JmpBack, 0, 0, 0), outer);
    a.bind(exit);
    a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    let out = a.finish().unwrap();
    // Words: JmpIf 0, Nop 1, JmpIf 2, JmpBack 3, JmpBack 4, JmpBack 5, RetUndef 6.
    assert_eq!(out.loops, [LoopHeader { head: 0, end: 5 }, LoopHeader { head: 1, end: 4 }]);
}

#[test]
fn a_jump_label_that_is_not_bound_is_an_error() {
    let mut a = Asm::default();
    let l = a.label();
    a.jump(Ins::new(Op::Jmp, 0, 0, 0), l);
    assert_eq!(a.finish().unwrap_err(), BcError::Unbound);
}

#[test]
fn a_long_jump_gets_the_wide_form_and_widening_one_jump_can_widen_another() {
    // `JmpIf` has a 16-bit offset. `first` jumps forward over `X + 1 + 1` words, and `second` jumps back over the `first` jump and
    // `X` nops: the back offset is -(1 + X + 1) = -32768 while `first` is narrow, and -32769 when `first` is wide.
    const X: usize = 32766;
    let mut a = Asm::default();
    let (top, end) = (a.label(), a.label());
    a.bind(top);
    let first = a.jump(Ins::new(Op::JmpIf, 0, 0, 0), end);
    (0..X).for_each(|_| _ = a.emit(Ins::new(Op::Nop, 0, 0, 0)));
    let second = a.jump(Ins::new(Op::JmpIf, 0, 0, 0), top);
    a.emit(Ins::new(Op::Nop, 0, 0, 0));
    a.bind(end);
    a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    let out = a.finish().unwrap();
    let ins = decode_all(&out.words);
    let w = |k: usize| out.words[out.pcs[k] as usize] as u8 == Op::Wide as u8;
    assert!(w(first) && w(second));
    assert_eq!(ins.len(), X + 4);
    for (k, want) in [(first, out.label_pcs[1]), (second, out.label_pcs[0])] {
        let (pc, i, len) = ins[k];
        assert_eq!((pc as u32, i.target(pc, len)), (out.pcs[k], Some(want as i64)));
    }
    // A jump that fits stays narrow.
    let mut a = Asm::default();
    let l = a.label();
    let j = a.jump(Ins::new(Op::JmpIf, 0, 0, 0), l);
    (0..32767).for_each(|_| _ = a.emit(Ins::new(Op::Nop, 0, 0, 0)));
    a.bind(l);
    a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    let out = a.finish().unwrap();
    assert_eq!(out.words[out.pcs[j] as usize] as u8, Op::JmpIf as u8);
}

#[test]
fn the_disassembly_names_ops_registers_and_targets() {
    let mut a = Asm::default();
    let l = a.label();
    a.emit(Ins::new(Op::GetField, 1, 300, 2));
    a.jump(Ins::new(Op::JSNeI, 1, 0, 0).with_x(-7i32 as u32), l);
    a.bind(l);
    a.emit(Ins::new(Op::RetUndef, 0, 0, 0));
    let out = a.finish().unwrap();
    let mut c = Code::new(2, 0);
    c.words = out.words;
    let text = c.disassemble();
    assert!(text.contains("0000  GetField.w  r1, r300, s2"), "{text}");
    assert!(text.contains("0002  JSNeI       r1, -> 0004, -7"), "{text}");
    assert!(text.contains("0004  RetUndef"), "{text}");
}

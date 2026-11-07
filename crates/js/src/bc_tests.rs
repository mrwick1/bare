use super::*;

/// Words that the narrow form of `ins` takes, or `None` when it has to be wide.
fn narrow_len(ins: &Ins) -> Option<usize> {
    let Ins { op, a, b, c, .. } = *ins;
    match op.form() {
        Form::Abc => (a < 256 && b < 256 && c < 256).then_some(1),
        Form::AbcX => (a < 256 && b < 256 && c < 256).then_some(2),
        Form::Abx => (a < 256 && b < 65536).then_some(1),
        Form::AsBx => (a < 256 && (b as i32) >= -32768 && (b as i32) <= 32767).then_some(1),
        Form::Sj => ((a as i32) >= -(1 << 23) && (a as i32) < 1 << 23).then_some(1),
    }
}

/// Operands at and around every width boundary of a form.
fn samples(op: Op) -> Vec<Ins> {
    let mut v = Vec::new();
    let regs = [0u32, 1, 255, 256, 1000, 65535];
    let xs = [0u32, 0xdead_beef, u32::MAX];
    match op.form() {
        Form::Abc | Form::AbcX => {
            for &a in &regs {
                for &b in &regs {
                    for &c in &[0u32, 7, 255, 256, 65535] {
                        for &x in if op.form() == Form::AbcX { &xs[..] } else { &xs[..1] } {
                            v.push(Ins::new(op, a, b, c).with_x(x));
                        }
                    }
                }
            }
        }
        Form::Abx => {
            for &a in &regs {
                for &b in &[0u32, 1, 65535, 65536, 1 << 24, u32::MAX] {
                    v.push(Ins::new(op, a, b, 0));
                }
            }
        }
        Form::AsBx => {
            for &a in &regs {
                for &b in &[0i32, 1, -1, 32767, -32768, 32768, -32769, i32::MAX, i32::MIN] {
                    v.push(Ins::new(op, a, b as u32, 0));
                }
            }
        }
        Form::Sj => {
            for &a in &[0i32, 1, -1, (1 << 23) - 1, -(1 << 23), 1 << 23, -(1 << 23) - 1, i32::MAX, i32::MIN] {
                v.push(Ins::new(op, a as u32, 0, 0));
            }
        }
    }
    v
}

#[test]
fn the_opcode_table_matches_the_numbers() {
    assert!(Op::ALL.len() < 256);
    for (i, &op) in Op::ALL.iter().enumerate() {
        assert_eq!(op as usize, i, "{}", op.name());
        assert_eq!(Op::from_u8(i as u8), Some(op));
        // an op with no operand letters is a form with nothing to name; letters never outnumber the operands
        let max = match op.form() {
            Form::Abc => 3,
            Form::AbcX => 4,
            Form::Abx | Form::AsBx => 2,
            Form::Sj => 1,
        };
        assert!(op.spec().split(' ').filter(|s| !s.is_empty()).count() <= max, "{}", op.name());
    }
    assert_eq!(Op::from_u8(Op::ALL.len() as u8), None);
    assert_eq!(Op::from_u8(255), None);
}

#[test]
fn every_form_round_trips_at_every_width_boundary() {
    let mut cases = 0;
    for &op in Op::ALL.iter().filter(|o| **o != Op::Wide) {
        for ins in samples(op) {
            let mut ops = vec![0xffff_ffff; 3]; // the encoder appends after what is there
            let n = encode(&mut ops, &ins).unwrap();
            assert_eq!(ops.len(), 3 + n);
            let expect = narrow_len(&ins).unwrap_or(if op.form() == Form::AbcX { 3 } else { 2 });
            assert_eq!(n, expect, "length of {ins:?}");
            assert_eq!(ops[3] as u8 == Op::Wide as u8, narrow_len(&ins).is_none(), "wide prefix of {ins:?}");
            assert_eq!(decode(&ops, 3), Ok((ins, n)), "{ins:?}");
            cases += 1;
        }
    }
    assert!(cases > 5000, "{cases}");
}

#[test]
fn a_stream_decodes_in_order() {
    let list = [
        Ins::new(Op::LoadI, 0, 7, 0),
        Ins::new(Op::LoadK, 300, 70000, 0),
        Ins::new(Op::GetField, 1, 0, 2).with_x(9),
        Ins::new(Op::GetField, 400, 1, 2).with_x(70000),
        Ins::new(Op::Jmp, (-3i32) as u32, 0, 0),
        Ins::new(Op::JmpIf, 2, (-40000i32) as u32, 0),
        Ins::new(Op::Add, 1, 2, 3),
        Ins::new(Op::RetUndef, 0, 0, 0),
    ];
    let mut ops = Vec::new();
    for i in &list {
        encode(&mut ops, i).unwrap();
    }
    let (mut pc, mut got) = (0, Vec::new());
    while pc < ops.len() {
        let (i, n) = decode(&ops, pc).unwrap();
        got.push(i);
        pc += n;
    }
    assert_eq!(got, list);
}

fn rnd(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

/// A random number of 1 to `bits` bits.
fn pick(x: &mut u64, bits: u32) -> u32 {
    let n = (rnd(x) % bits as u64) as u32 + 1;
    (rnd(x) >> (64 - n)) as u32
}

#[test]
fn random_operands_round_trip() {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..20_000 {
        let op = Op::ALL[(rnd(&mut x) % Op::ALL.len() as u64) as usize];
        if op == Op::Wide {
            continue;
        }
        let sh = (rnd(&mut x) % 3) as u32;
        let ins = match op.form() {
            Form::Abc => Ins::new(op, pick(&mut x, 16), pick(&mut x, 16), pick(&mut x, 16)),
            Form::AbcX => Ins::new(op, pick(&mut x, 16), pick(&mut x, 16), pick(&mut x, 16)).with_x(rnd(&mut x) as u32),
            Form::Abx => Ins::new(op, pick(&mut x, 16), pick(&mut x, 32), 0),
            Form::AsBx => Ins::new(op, pick(&mut x, 16), (pick(&mut x, 32) as i32 >> sh) as u32, 0),
            Form::Sj => Ins::new(op, (pick(&mut x, 32) as i32 >> sh) as u32, 0, 0),
        };
        let mut ops = Vec::new();
        let n = encode(&mut ops, &ins).unwrap();
        assert_eq!(decode(&ops, 0), Ok((ins, n)), "{ins:?}");
    }
}

#[test]
fn an_operand_that_is_too_wide_is_an_error() {
    let mut ops = Vec::new();
    assert_eq!(encode(&mut ops, &Ins::new(Op::Add, 65536, 0, 0)), Err(BcError::Operand));
    assert_eq!(encode(&mut ops, &Ins::new(Op::Add, 0, 0, 65536)), Err(BcError::Operand));
    assert_eq!(encode(&mut ops, &Ins::new(Op::GetField, 0, 65536, 0)), Err(BcError::Operand));
    assert_eq!(encode(&mut ops, &Ins::new(Op::LoadK, 65536, 0, 0)), Err(BcError::Operand));
    assert_eq!(encode(&mut ops, &Ins::new(Op::JmpIf, 65536, 0, 0)), Err(BcError::Operand));
    assert!(ops.is_empty(), "a failed encode writes nothing");
}

#[test]
fn bad_input_is_an_error_and_never_a_panic() {
    assert_eq!(decode(&[], 0), Err(BcError::End));
    assert_eq!(decode(&[0xff], 0), Err(BcError::BadOp(255)));
    assert_eq!(decode(&[Op::Wide as u32], 0), Err(BcError::BadWide)); // the wide prefix is cut off
    assert_eq!(decode(&[Op::Wide as u32 | (Op::Wide as u32) << 8, 0], 0), Err(BcError::BadWide)); // wide of wide
    assert_eq!(decode(&[Op::Wide as u32 | 255 << 8, 0], 0), Err(BcError::BadWide)); // wide of a byte that is no op
    let wide_get = Op::Wide as u32 | (Op::GetField as u32) << 8;
    assert_eq!(decode(&[wide_get, 0], 0), Err(BcError::BadWide)); // the raw word is missing
    assert_eq!(decode(&[Op::GetField as u32], 0), Err(BcError::End));
    // every one-word prefix of a valid stream either decodes or gives an error
    let mut ops = Vec::new();
    for i in samples(Op::GetField).iter().take(50) {
        encode(&mut ops, i).unwrap();
    }
    for cut in 0..ops.len() {
        for pc in 0..cut {
            let _ = decode(&ops[..cut], pc);
        }
    }
}

#[test]
fn handlers_and_source_positions() {
    let mut c = Code::new(4);
    c.handlers = vec![Handler { start: 4, end: 6, target: 12, reg: 3, depth: 0 }, Handler { start: 2, end: 10, target: 14, reg: 2, depth: 0 }];
    assert_eq!(c.handler_at(1), None);
    assert_eq!(c.handler_at(2).map(|h| h.target), Some(14));
    assert_eq!(c.handler_at(5).map(|h| h.target), Some(12), "the first range that holds the word is the innermost");
    assert_eq!(c.handler_at(6).map(|h| h.target), Some(14));
    assert_eq!(c.handler_at(10), None);
    c.lines = vec![(0, 10), (3, 25), (7, 40)];
    assert_eq!([0, 2, 3, 6, 7, 99].map(|p| c.source_at(p)), [Some(10), Some(10), Some(25), Some(25), Some(40), Some(40)]);
    assert_eq!(Code::new(1).source_at(0), None);
}

#[test]
fn the_disassembler_names_operands_constants_and_targets() {
    let mut c = Code::new(5);
    c.consts = vec![Const::Number(2.5), Const::String("hi\n".encode_utf16().collect()), Const::Name(7)];
    c.ics = vec![Cell::new(Ic::EMPTY)].into();
    let prog = [
        Ins::new(Op::LoadI, 0, 7, 0),
        Ins::new(Op::LoadK, 1, 0, 0),
        Ins::new(Op::Add, 2, 0, 1),
        Ins::new(Op::GetField, 3, 2, 0).with_x(2),
        Ins::new(Op::JmpIfNot, 3, 2, 0),
        Ins::new(Op::LoadK, 300, 1, 0),
        Ins::new(Op::Jmp, (-7i32) as u32, 0, 0),
        Ins::new(Op::GetEnv, 4, 1, 300),
        Ins::new(Op::Ret, 2, 0, 0),
    ];
    for i in &prog {
        encode(&mut c.ops, i).unwrap();
    }
    c.handlers.push(Handler { start: 0, end: 4, target: 9, reg: 4, depth: 1 });
    let want = "\
code nregs=5 words=12 consts=3 ics=1
0000  LoadI         r0, 7
0001  LoadK         r1, k0  ; 2.5
0002  Add           r2, r0, r1
0003  GetField      r3, r2, ic0, k2  ; name#7
0005  JmpIfNot      r3, -> 0008
0006  LoadK.w       r300, k1  ; \"hi\\n\"
0008  Jmp           -> 0002
0009  GetEnv.w      r4, 1, 300
0011  Ret           r2
handler 0000..0004 -> 0009 r4 depth 1
";
    assert_eq!(c.disassemble(), want);
    // a bad byte stops the listing and says so
    let mut bad = Code::new(1);
    bad.ops = vec![Op::Nop as u32, 0xff];
    assert_eq!(bad.disassemble(), "code nregs=1 words=2 consts=0 ics=0\n0000  Nop           \n0001  <BadOp(255)>\n");
}

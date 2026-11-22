// Tests of `verify.rs`. Each test makes a small valid function with the assembler, then breaks one thing.
use super::*;
use crate::atoms::SharedAtoms;
use crate::roots::RootMaps;
use crate::bytecode::{Asm, Handler, Ins, LoopHeader};
use crate::string::Chars;

/// A function of `nregs` registers (the first `nparams` are arguments), 4 atoms, and the code that `f` assembles. Each safepoint gets a
/// map of the arguments. The atoms live as long as the returned table.
fn build(nregs: u16, nparams: u16, f: impl FnOnce(&mut Asm)) -> (SharedAtoms, Code) {
    let shared = SharedAtoms::default();
    let mut c = Code::new(nregs, nparams);
    c.atoms = (0..4).map(|i| shared.intern(Chars::One(format!("n{i}").as_bytes())).unwrap()).collect();
    let mut a = Asm::default();
    f(&mut a);
    let out = a.finish().unwrap();
    c.words = out.words;
    c.loops = out.loops;
    maps(&mut c, |_| (0..nparams as usize).collect());
    (shared, c)
}

/// Give each safepoint instruction a root map. `live(pc)` says which registers.
fn maps(c: &mut Code, live: impl Fn(u32) -> Vec<usize>) {
    c.roots = RootMaps::new(c.nregs as usize);
    let mut pc = 0;
    while pc < c.words.len() {
        let (i, n) = decode(&c.words, pc).unwrap();
        if i.op.is_safepoint() {
            c.roots.add(pc as u32, live(pc as u32));
        }
        pc += n;
    }
}

fn ins(op: Op, a: u32, b: u32, c: u32) -> Ins {
    Ins::new(op, a, b, c)
}

/// r0 = argument. `r1 = 0; while (r1 < 10) { r2 = r0.n0; r1 += 1 } return r1`, with a site for the field.
fn counting_loop() -> (SharedAtoms, Code) {
    let (s, mut c) = build(4, 1, |a| {
        let (head, exit) = (a.label(), a.label());
        a.emit(ins(Op::LoadI, 1, 0, 0));
        a.bind(head);
        a.jump(ins(Op::JnLtI, 1, 0, 0).with_x(10), exit);
        a.emit(ins(Op::GetField, 2, 0, 0));
        a.emit(ins(Op::AddI, 1, 1, 1));
        a.jump(ins(Op::JmpBack, 0, 0, 0), head);
        a.bind(exit);
        a.emit(ins(Op::Ret, 1, 0, 0));
    });
    c.sites = vec![0];
    maps(&mut c, |_| vec![0, 1]);
    (s, c)
}

fn bad(c: &Code, what: &str) {
    let e = verify(c).expect_err("the verifier accepted bad code");
    assert!(e.contains(what), "wanted {what:?}, got {e:?}");
}

#[test]
fn a_valid_function_passes() {
    let (_s, c) = counting_loop();
    verify(&c).unwrap();
    assert_eq!(c.loops, [LoopHeader { head: 1, end: 5 }]);
    // r1 is read at the loop header, so even the map before the first field read must name it.
    let mut c = c;
    maps(&mut c, |pc| if pc >= 3 { vec![0, 1] } else { vec![0] });
    bad(&c, "root map misses live r1");
}

#[test]
fn operand_ranges_and_tables() {
    let one = |i: Ins| build(4, 1, |a| _ = (a.emit(i), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    for (i, what) in [
        (ins(Op::Mov, 4, 0, 0), "register out of range"),
        (ins(Op::Mov, 1, 4, 0), "register out of range"),
        (ins(Op::Nop, 1, 0, 0), "not used"),
        (ins(Op::LoadUndef, 1, 1, 0), "not used"),
        (ins(Op::SetFieldK, 0, 4, 0), "special constant"),
        (ins(Op::LoadN, 1, 0, 0), "no such number"),
        (ins(Op::LoadAtom, 1, 4, 0), "no such atom"),
        (ins(Op::Closure, 1, 0, 0), "no such function"),
        (ins(Op::NewObjLit, 1, 0, 0), "no such recipe"),
        (ins(Op::GetField, 1, 0, 0), "no such site"),
        (ins(Op::GetGlobal, 1, 0, 0), "no such site"),
        (ins(Op::Call, 1, 0, 3), "window out of range"),
        (ins(Op::CallU, 1, 3, 1), "window out of range"),
        (ins(Op::New, 1, 3, 1), "window out of range"),
        (ins(Op::NewArrLit, 1, 2, 3), "window out of range"),
        (ins(Op::Concat, 1, 0, 0), "Concat of no strings"),
        (ins(Op::Concat, 1, 2, 3), "window out of range"),
        (ins(Op::IterOpen, 2, 0, 0), "window out of range"),
        (ins(Op::IterClose, 2, 0, 0), "window out of range"),
    ] {
        let (_s, c) = one(i);
        bad(&c, what);
    }
    // The same ops with a legal operand pass (the window of Call: callee, this, 1 argument = r0..r2).
    let (_s, c) = one(ins(Op::Call, 3, 0, 1));
    verify(&c).expect_err("r1 and r2 are not written");
    let (_s, c) = build(4, 3, |a| _ = (a.emit(ins(Op::Call, 3, 0, 1)), a.emit(ins(Op::Ret, 3, 0, 0))));
    verify(&c).unwrap();
}

#[test]
fn a_site_has_an_atom_and_one_instruction() {
    let (_s, mut c) = counting_loop();
    c.sites = vec![4];
    bad(&c, "site 0 has no atom");
    let (_s, mut c) = counting_loop();
    c.sites = vec![0, 1];
    bad(&c, "site 1 is used 0 times");
    let (_s, mut c) = build(2, 1, |a| _ = (a.emit(ins(Op::GetField, 1, 0, 0)), a.emit(ins(Op::GetField, 1, 0, 0)), a.emit(ins(Op::Ret, 1, 0, 0))));
    c.sites = vec![0];
    bad(&c, "site 0 is used 2 times");
    c.recipes.push(&[9]);
    bad(&c, "a recipe names no atom");
}

#[test]
fn jumps_go_to_instructions_and_only_back_edges_go_back() {
    // A jump into the second word of a wide instruction.
    let (_s, c) = build(301, 0, |a| {
        a.emit(ins(Op::Jmp, 1, 0, 0));
        a.emit(ins(Op::Mov, 300, 0, 0));
        a.emit(ins(Op::RetUndef, 0, 0, 0));
    });
    bad(&c, "jump target is not an instruction");
    // A jump past the end, and before the start.
    let (_s, c) = build(2, 0, |a| _ = (a.emit(ins(Op::Jmp, 5, 0, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    bad(&c, "jump target is not an instruction");
    let (_s, c) = build(2, 0, |a| _ = (a.emit(ins(Op::Jmp, -3i32 as u32, 0, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    bad(&c, "jump target is not an instruction");
    // A backward Jmp, a backward conditional jump, a forward JmpBack, and a JmpBack to itself.
    for (op, off, what) in [
        (Op::Jmp, -2i32, "a jump back that is not JmpBack"),
        (Op::JmpIf, -2, "a jump back that is not JmpBack"),
        (Op::JmpBack, 1, "JmpBack goes forward"),
        (Op::JmpBack, -1, ""),
    ] {
        let (_s, c) = build(2, 1, |a| {
            a.emit(ins(Op::Nop, 0, 0, 0));
            a.emit(ins(op, if op == Op::JmpIf { 0 } else { off as u32 }, if op == Op::JmpIf { off as u32 } else { 0 }, 0));
            a.emit(ins(Op::RetUndef, 0, 0, 0));
            a.emit(ins(Op::RetUndef, 0, 0, 0));
        });
        if what.is_empty() {
            // `JmpBack` with offset -1 goes to word 1, its own word: a loop that covers itself must still have a table entry.
            bad(&c, "a back edge that no loop covers");
        } else {
            bad(&c, what);
        }
    }
}

#[test]
fn the_loop_table() {
    let t = |f: &dyn Fn(&mut Code), what: &str| {
        let (_s, mut c) = counting_loop();
        f(&mut c);
        bad(&c, what);
    };
    t(&|c| c.loops.clear(), "a back edge that no loop covers");
    t(&|c| c.loops[0].head = 2, "a bad header or end");
    t(&|c| c.loops[0].head = 3, "the end is not a JmpBack to the header");
    t(&|c| c.loops[0].end = 99, "a bad header or end");
    t(&|c| c.loops[0].end = 2, "a bad header or end");
    t(&|c| c.loops[0].end = 0, "a bad header or end");
    t(&|c| c.loops[0].end = 4, "the end is not a JmpBack to the header");
    t(&|c| c.loops.push(c.loops[0]), "not sorted and unique");
    // Two loops that cross: the second starts inside the first and ends after it.
    let (_s, mut c) = build(3, 1, |a| {
        let (h1, h2) = (a.label(), a.label());
        a.bind(h1);
        a.emit(ins(Op::Nop, 0, 0, 0));
        a.bind(h2);
        a.emit(ins(Op::Nop, 0, 0, 0));
        a.jump(ins(Op::JmpBack, 0, 0, 0), h1);
        a.jump(ins(Op::JmpBack, 0, 0, 0), h2);
        a.emit(ins(Op::RetUndef, 0, 0, 0));
    });
    // The assembler found: header 0 ends at word 2, header 1 ends at word 3. They cross.
    assert_eq!(c.loops, [LoopHeader { head: 0, end: 2 }, LoopHeader { head: 1, end: 3 }]);
    bad(&c, "crosses the end of another loop");
    // A back edge after the end of its loop, with the table holding the first one only.
    c.loops = vec![LoopHeader { head: 0, end: 2 }];
    bad(&c, "a back edge that no loop covers");
}

#[test]
fn handlers() {
    let mk = || {
        let (s, mut c) = build(3, 1, |a| {
            a.emit(ins(Op::LoadI, 1, 1, 0));
            a.emit(ins(Op::Add, 1, 1, 0));
            a.emit(ins(Op::Ret, 1, 0, 0));
            a.emit(ins(Op::Ret, 2, 0, 0));
        });
        maps(&mut c, |_| vec![0, 1]);
        (s, c)
    };
    let h = Handler { start: 1, end: 2, target: 3, reg: 2, depth: 0 };
    let (_s, mut c) = mk();
    c.handlers = vec![h];
    verify(&c).unwrap();
    for (f, what) in [
        (Handler { start: 2, end: 2, ..h }, "a bad range"),
        (Handler { end: 9, ..h }, "a bad range"),
        (Handler { target: 9, ..h }, "a bad range"),
        (Handler { reg: 3, ..h }, "a bad range"),
        (Handler { depth: 1, ..h }, "is below the depth"),
    ] {
        let (_s, mut c) = mk();
        c.handlers = vec![f];
        bad(&c, what);
    }
    // The handler register is written when the handler runs, so the target may read it; without the handler it is not.
    let (_s, mut c) = mk();
    c.handlers = vec![Handler { reg: 1, ..h }];
    bad(&c, "reads r2");
}

#[test]
fn source_positions() {
    let (_s, mut c) = counting_loop();
    c.lines = vec![(0, 5), (2, 9), (4, 10)];
    verify(&c).unwrap();
    assert_eq!((c.source_at(0), c.source_at(3), c.source_at(99)), (Some(5), Some(9), Some(10)));
    c.lines = vec![(2, 5), (2, 9)];
    bad(&c, "source positions");
    c.lines = vec![(0, 5), (99, 9)];
    bad(&c, "source positions");
}

#[test]
fn root_maps() {
    // A safepoint with no map.
    let (_s, mut c) = counting_loop();
    c.roots = RootMaps::new(4);
    bad(&c, "a safepoint with no root map");
    // A map for a word that is not an instruction, and for an instruction that is not a safepoint (the LoadI at word 0).
    let (_s, mut c) = counting_loop();
    c.roots.add(99, []);
    bad(&c, "not a safepoint instruction");
    let (_s, mut c) = counting_loop();
    maps(&mut c, |_| vec![0]);
    let mut r = RootMaps::new(4);
    r.add(0, []);
    for &p in c.roots.pcs() {
        r.add(p, [0]);
    }
    c.roots = r;
    bad(&c, "not a safepoint instruction");
    // The wrong register count.
    let (_s, mut c) = counting_loop();
    maps(&mut c, |_| vec![0]);
    let pcs: Vec<u32> = c.roots.pcs().to_vec();
    c.roots = RootMaps::new(5);
    pcs.iter().for_each(|p| c.roots.add(*p, [0]));
    bad(&c, "another register count");
    // A map that names a register that a path has not written: r2 is first written by the field read.
    let (_s, mut c) = counting_loop();
    maps(&mut c, |_| vec![0, 2]);
    bad(&c, "the root map names r2");
    // r1 is written by the first word, so a map may name it after that.
    let (_s, mut c) = counting_loop();
    maps(&mut c, |_| vec![0, 1]);
    verify(&c).unwrap();
    // A register that only one branch writes is not written on every path.
    let (_s, mut c) = build(4, 1, |a| {
        let (skip, join) = (a.label(), a.label());
        a.jump(ins(Op::JmpIf, 0, 0, 0), skip);
        a.emit(ins(Op::LoadI, 1, 7, 0));
        a.bind(skip);
        a.bind(join);
        a.emit(ins(Op::Add, 2, 0, 0));
        a.emit(ins(Op::Ret, 2, 0, 0));
    });
    maps(&mut c, |_| vec![0]);
    verify(&c).unwrap();
    maps(&mut c, |_| vec![0, 1]);
    bad(&c, "the root map names r1");
}

#[test]
fn reads_come_after_writes() {
    let (_s, c) = build(3, 1, |a| _ = (a.emit(ins(Op::Add, 2, 0, 1)), a.emit(ins(Op::Ret, 2, 0, 0))));
    bad(&c, "reads r1");
    let (_s, c) = build(3, 2, |a| _ = (a.emit(ins(Op::Add, 2, 0, 1)), a.emit(ins(Op::Ret, 2, 0, 0))));
    verify(&c).unwrap();
    // `Mov r1, r1` reads before it writes.
    let (_s, c) = build(3, 1, |a| _ = (a.emit(ins(Op::Mov, 1, 1, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    bad(&c, "reads r1");
    // A window: a call reads its callee, `this` and arguments, and an iterator record is written by `IterOpen` and read by `IterNext`.
    let (_s, mut c) = build(6, 1, |a| {
        let (done, again) = (a.label(), a.label());
        a.emit(ins(Op::IterOpen, 1, 0, 0));
        a.bind(again);
        a.jump(ins(Op::IterNext, 4, 0, 0).with_x(1), done);
        a.emit(ins(Op::Mov, 5, 4, 0));
        a.jump(ins(Op::JmpBack, 0, 0, 0), again);
        a.bind(done);
        a.emit(ins(Op::IterClose, 1, 0, 0));
        a.emit(ins(Op::RetUndef, 0, 0, 0));
    });
    maps(&mut c, |pc| if pc == 0 { vec![0] } else { vec![0, 1, 2, 3] });
    verify(&c).unwrap();
    // Reading one register of the window that was not written: `IterNext` with the record at r2 (r1 holds the written part).
    let (_s, c) = build(6, 1, |a| {
        let done = a.label();
        a.emit(ins(Op::IterOpen, 1, 0, 0));
        a.jump(ins(Op::IterNext, 5, 0, 0).with_x(2), done);
        a.bind(done);
        a.emit(ins(Op::RetUndef, 0, 0, 0));
    });
    bad(&c, "of its window");
    // `NewObjLit` reads as many registers as its recipe has keys.
    let (_s, mut c) = build(4, 1, |a| _ = (a.emit(ins(Op::NewObjLit, 3, 0, 0)), a.emit(ins(Op::Ret, 3, 0, 0))));
    c.recipes.push(&[0, 1]);
    bad(&c, "of its window");
    c.recipes = Default::default();
    c.recipes.push(&[0]);
    verify(&c).unwrap();
}

#[test]
fn control_flow_and_context_depth() {
    // A path past the end.
    let (_s, c) = build(2, 1, |a| _ = a.emit(ins(Op::Mov, 1, 0, 0)));
    bad(&c, "a path runs past the end");
    let (_s, c) = build(2, 1, |a| {
        let l = a.label();
        a.jump(ins(Op::JmpIf, 0, 0, 0), l);
        a.emit(ins(Op::RetUndef, 0, 0, 0));
        a.bind(l);
        a.emit(ins(Op::Nop, 0, 0, 0));
    });
    bad(&c, "a path runs past the end");
    // PopCtx at depth 0.
    let (_s, c) = build(2, 1, |a| _ = (a.emit(ins(Op::PopCtx, 0, 0, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    bad(&c, "PopCtx at depth 0");
    // The depth differs at a join: only one branch pushes a context.
    let (_s, c) = build(2, 1, |a| {
        let l = a.label();
        a.jump(ins(Op::JmpIf, 0, 0, 0), l);
        a.emit(ins(Op::PushCtx, 0, 2, 0));
        a.bind(l);
        a.emit(ins(Op::RetUndef, 0, 0, 0));
    });
    bad(&c, "context depth");
    // A balanced push and pop, and a context slot read with `GetCtx` in between.
    let (_s, c) = build(2, 1, |a| {
        a.emit(ins(Op::PushCtx, 0, 2, 0));
        a.emit(ins(Op::InitCtx, 0, 0, 1));
        a.emit(ins(Op::GetCtx, 1, 0, 1));
        a.emit(ins(Op::PopCtx, 0, 0, 0));
        a.emit(ins(Op::Ret, 1, 0, 0));
    });
    verify(&c).unwrap();
    // Code after a return is not reached, and the data flow does not look at it.
    let (_s, c) = build(2, 1, |a| _ = (a.emit(ins(Op::RetUndef, 0, 0, 0)), a.emit(ins(Op::Mov, 1, 1, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    verify(&c).unwrap();
}

#[test]
fn bad_words_and_registers() {
    let (_s, mut c) = counting_loop();
    c.words[0] = 255;
    bad(&c, "BadOp(255)");
    let (_s, mut c) = counting_loop();
    c.nparams = 5;
    bad(&c, "more parameters than registers");
    let (_s, mut c) = counting_loop();
    c.words.clear();
    bad(&c, "no code");
}

#[test]
fn windows_of_the_ops_for_spread_and_super() {
    // `CallSpread` reads 3 registers from `b` (callee, this, array); `NewSpread` 2; `CallSuper` reads `c`; `ObjRest` reads `x` from `c`.
    let ok = |i: Ins, written: u32| {
        let (_s, c) = build(6, 0, |a| {
            for r in 0..written {
                a.emit(ins(Op::LoadUndef, r, 0, 0));
            }
            a.emit(i);
            a.emit(ins(Op::RetUndef, 0, 0, 0));
        });
        let mut c = c;
        maps(&mut c, |pc| (0..written.min(pc)).map(|r| r as usize).collect());
        verify(&c)
    };
    assert!(ok(ins(Op::CallSpread, 5, 1, 0), 4).is_ok());
    assert!(ok(ins(Op::CallSpread, 5, 1, 0), 3).unwrap_err().contains("window"));
    assert!(ok(ins(Op::CallSpread, 5, 4, 0), 5).unwrap_err().contains("window out of range"));
    assert!(ok(ins(Op::NewSpread, 5, 1, 0), 3).is_ok());
    assert!(ok(ins(Op::NewSpread, 5, 1, 0), 2).unwrap_err().contains("window"));
    assert!(ok(ins(Op::CallSuper, 5, 1, 2), 4).is_ok());
    assert!(ok(ins(Op::CallSuper, 5, 1, 2), 2).unwrap_err().contains("window"));
    assert!(ok(ins(Op::ObjRest, 5, 0, 1).with_x(3), 4).is_ok());
    assert!(ok(ins(Op::ObjRest, 5, 0, 1).with_x(3), 3).unwrap_err().contains("window"));
    assert!(ok(ins(Op::ObjRest, 5, 0, 4).with_x(3), 5).unwrap_err().contains("window out of range"));
}

#[test]
fn a_template_recipe_may_hold_an_undefined_cooked_string() {
    let make = |keys: &[u32], op: Op| {
        let (_s, mut c) = build(2, 0, |a| _ = (a.emit(ins(op, 0, 0, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
        c.recipes.push(keys);
        c.sites = vec![0];
        maps(&mut c, |_| vec![]);
        c
    };
    verify(&make(&[u32::MAX, 1, 2, 3], Op::GetTemplate)).unwrap();
    bad(&make(&[0, u32::MAX, 2, 3], Op::GetTemplate), "recipe");
    bad(&make(&[0, 1, 2], Op::GetTemplate), "recipe");
    // Another op cannot name an undefined key.
    let (_s, mut c) = build(2, 0, |a| _ = (a.emit(ins(Op::NewObjLit, 0, 1, 0)), a.emit(ins(Op::RetUndef, 0, 0, 0))));
    c.recipes.push(&[u32::MAX]);
    bad(&c, "recipe");
}

/// r0 = argument, r1 a result. pc 0 `op r0, t0`; pc 1 and 2 are one wide `LoadI r1, 100000` (so 2 is no instruction start); pc 3 `Ret r1`;
/// pc 4 `RetUndef`; the code ends at 5. The tables are `tables`. A good target is 1 or 4; 3 is an instruction, but `r1` is not written on a path that goes there from the switch.
fn switch_code(op: Op, tables: Vec<u32>) -> (SharedAtoms, Code) {
    // `build` gives the atoms and the root maps; the words are written by hand, so that the `LoadI` has the wide form (a 17-bit immediate).
    let (s, mut c) = build(2, 1, |a| _ = a.emit(ins(Op::RetUndef, 0, 0, 0)));
    let mut words = Vec::new();
    crate::bytecode::encode(&mut words, &ins(op, 0, 0, 0)).unwrap();
    crate::bytecode::encode(&mut words, &ins(Op::LoadI, 1, 100_000, 0)).unwrap();
    crate::bytecode::encode(&mut words, &ins(Op::Ret, 1, 0, 0)).unwrap();
    crate::bytecode::encode(&mut words, &ins(Op::RetUndef, 0, 0, 0)).unwrap();
    assert_eq!(words.len(), 5);
    c.words = words;
    c.tables = tables;
    maps(&mut c, |_| vec![0]);
    (s, c)
}

#[test]
fn a_switch_table_passes_and_its_targets_are_the_successors() {
    // lo 1, default 4, targets 1 4 1: the values 1 and 3 go to pc 1, and 2 goes to the default.
    let (_s, c) = switch_code(Op::SwitchI, vec![1, 4, 3, 1, 4, 1]);
    verify(&c).unwrap();
    let (i, _) = decode(&c.words, 0).unwrap();
    assert_eq!(c.switch_targets(&i).unwrap(), [1, 4, 1, 4]);
    assert!(c.disassemble().contains("[1 -> 0001, 3 -> 0001, default -> 0004]"), "{}", c.disassemble());
    // The data flow follows each target: r1 is written on the path to 1 and not on the path to 3.
    let (_s, c) = switch_code(Op::SwitchI, vec![0, 4, 2, 1, 3]);
    bad(&c, "reads r1, which a path has not written");
}

#[test]
fn a_bad_integer_table_is_refused() {
    for (t, what) in [
        (vec![], "not inside the pool"),
        (vec![0, 4], "not inside the pool"),
        (vec![0, 4, 3, 1, 3], "not inside the pool"),
        (vec![0, 4, 0], "an empty table"),
        (vec![0, 2, 1, 1], "not a later instruction"),
        (vec![0, 4, 1, 2], "not a later instruction"),
        (vec![0, 4, 1, 0], "not a later instruction"),
        (vec![0, 4, 1, 5], "not a later instruction"),
        (vec![0, 9, 1, 1], "not a later instruction"),
        (vec![i32::MAX as u32, 4, 2, 1, 3], "past the largest integer"),
    ] {
        let (_s, c) = switch_code(Op::SwitchI, t.clone());
        bad(&c, what);
    }
    // The offset in the op names a table that is not there.
    let (_s, mut c) = switch_code(Op::SwitchI, vec![0, 4, 1, 1]);
    let mut words = Vec::new();
    crate::bytecode::encode(&mut words, &ins(Op::SwitchI, 0, 7, 0)).unwrap();
    c.words[0] = words[0];
    bad(&c, "not inside the pool");
    // The register of the discriminant is checked as any register is.
    let (_s, mut c) = switch_code(Op::SwitchI, vec![0, 4, 1, 1]);
    let mut words = Vec::new();
    crate::bytecode::encode(&mut words, &ins(Op::SwitchI, 2, 0, 0)).unwrap();
    c.words[0] = words[0];
    bad(&c, "register out of range");
}

/// The words of a string table: the default, `slots - 1` and the slots. `at` gives each entry `(atom, target)` its slot; the hash is the
/// hash of the atom.
fn str_table(c: &Code, default: u32, slots: usize, at: &[(usize, u32, u32)]) -> Vec<u32> {
    let mut t = vec![default, slots as u32 - 1];
    for _ in 0..slots {
        t.extend([0, crate::bytecode::STR_EMPTY, 0]);
    }
    for &(slot, atom, target) in at {
        t[2 + 3 * slot..2 + 3 * slot + 3].copy_from_slice(&[c.atoms[atom as usize].cached_hash(), atom, target]);
    }
    t
}

#[test]
fn a_string_table_passes_and_a_bad_one_is_refused() {
    use crate::bytecode::STR_EMPTY;
    let (_s, c) = switch_code(Op::SwitchA, vec![]);
    let home = |a: u32, slots: usize| c.atoms[a as usize].cached_hash() as usize & (slots - 1);
    // 8 slots, the atoms 0 and 1 each in its home slot or the next one.
    let (p0, p1) = (home(0, 8), home(1, 8));
    let p1 = if p1 == p0 { (p0 + 1) & 7 } else { p1 };
    let ok = str_table(&c, 4, 8, &[(p0, 0, 1), (p1, 1, 4)]);
    let lost = str_table(&c, 4, 8, &[((home(0, 8) + 1) & 7, 0, 1)]);
    let (_s, mut c) = switch_code(Op::SwitchA, ok.clone());
    verify(&c).unwrap();
    let (i, _) = decode(&c.words, 0).unwrap();
    let mut got = c.switch_targets(&i).unwrap();
    got.sort_unstable();
    assert_eq!(got, [1, 4, 4]);
    assert!(c.disassemble().contains("[a0 -> 0001, a1 -> 0004, default -> 0004]"), "{}", c.disassemble());

    let mut refuse = |mutate: &dyn Fn(&mut Vec<u32>), what: &str| {
        let mut tab = ok.clone();
        mutate(&mut tab);
        c.tables = tab;
        bad(&c, what);
    };
    // A slot count that is not a power of two, and a table that is cut off.
    refuse(&|t| t[1] = 6, "power of two");
    refuse(&|t| t.truncate(5), "power of two");
    // No empty slot: every slot holds an atom.
    refuse(&|t| (0..8).for_each(|s| t[2 + 3 * s + 1] = if t[2 + 3 * s + 1] == STR_EMPTY { 2 } else { t[2 + 3 * s + 1] }), "no empty slot");
    // An atom out of range, a wrong hash, a bad target, a bad default.
    refuse(&|t| t[2 + 3 * p0 + 1] = 9, "names no atom");
    refuse(&|t| t[2 + 3 * p0] ^= 1, "not the hash of its atom");
    refuse(&|t| t[2 + 3 * p0 + 2] = 2, "not a later instruction");
    refuse(&|t| t[0] = 0, "not a later instruction");
    // A slot that its own probe cannot find: its home slot is empty, and it sits in the slot after.
    refuse(&|t| *t = lost.clone(), "meets an empty slot first");
}

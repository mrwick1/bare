// Tests of the compiler: golden disassembly by construct, and the verifier on every function that it compiles.
use super::*;
use crate::parse::Limits;
use crate::verify::verify;

const FUNCTIONS: usize = usize::MAX;
/// Selects the code of the script itself in `golden`.
const SCRIPT: usize = FUNCTIONS;

/// Compile a script and all its functions, in the order of their ids. Every code passes the verifier. Gives the script and the functions.
fn build(src: &str) -> (Compiled, Vec<Compiled>) {
    let atoms = SharedAtoms::default();
    let (s, fs) = compile_all(src, &atoms, Limits::default()).expect("compile");
    verify(&s.code).unwrap_or_else(|e| panic!("script: {e}\n{}", s.code.disassemble()));
    for (id, f) in &fs {
        verify(&f.code).unwrap_or_else(|e| panic!("fn {id}: {e}\n{}", f.code.disassemble()));
    }
    (s, fs.into_iter().map(|(_, f)| f).collect())
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect()
}

/// The disassembly of function `which` (or of the script) must equal `expect`.
fn golden(src: &str, which: usize, expect: &str) {
    let (s, fs) = build(src);
    let c = if which == SCRIPT { &s } else { &fs[which] };
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    let got = c.code.disassemble();
    assert_eq!(lines(&got), lines(expect), "\n{got}");
}

/// The code of the function that `src` declares at its top level (the ids go in the order in which the functions end, so it is the last).
fn outer(src: &str) -> Compiled {
    build(src).1.pop().expect("a function")
}

#[test]
fn arithmetic_uses_immediates() {
    golden(
        r#"function f(a, b) { var x = a + 1, y = a * b - 3; return x - y; }"#,
        0,
        r#"
code nregs=5 nparams=2 words=5 sites=0
0000  AddI        r2, r0, 1
0001  Mul         r4, r0, r1
0002  SubI        r3, r4, 3
0003  Sub         r4, r2, r3
0004  Ret         r4
"#,
    );
}

#[test]
fn compare_jumps_take_constants_on_either_side() {
    golden(
        r#"function f(a) { if (a < 10) return 1; if (10 < a) return 2; if (a === null) return 3; if (undefined !== a) return 4; if (a == 'k') return 5; if (a >= 1.5) return 7; return 6; }"#,
        0,
        r#"
code nregs=2 nparams=1 words=26 sites=0
0000  JnLtI       r0, -> 0004, 10
0002  LoadI       r1, 1
0003  Ret         r1
0004  JnGtI       r0, -> 0008, 10
0006  LoadI       r1, 2
0007  Ret         r1
0008  JSNeNull    r0, -> 0011
0009  LoadI       r1, 3
0010  Ret         r1
0011  JSEqUndef   r0, -> 0014
0012  LoadI       r1, 4
0013  Ret         r1
0014  LoadAtom    r1, a0
0015  JNe         r0, r1, -> 0019
0017  LoadI       r1, 5
0018  Ret         r1
0019  LoadN       r1, n0
0020  JnGe        r0, r1, -> 0024
0022  LoadI       r1, 7
0023  Ret         r1
0024  LoadI       r1, 6
0025  Ret         r1
"#,
    );
}

#[test]
fn and_or_in_a_test_context_make_no_value() {
    golden(
        r#"function f(a, b, o) { var x = a; if (a < 10 && b > 2 || o) x++; return x; }"#,
        0,
        r#"
code nregs=4 nparams=3 words=8 sites=0
0000  Mov         r3, r0
0001  JnLtI       r0, -> 0005, 10
0003  JGtI        r1, -> 0006, 2
0005  JmpIfNot    r2, -> 0007
0006  Inc         r3, r3
0007  Ret         r3
"#,
    );
}

#[test]
fn logical_operators_in_value_context() {
    golden(
        r#"function f(a, b) { var x = a || b; var y = a && b; var z = a ?? b; return x + y + z; }"#,
        0,
        r#"
code nregs=7 nparams=2 words=12 sites=0
0000  Mov         r2, r0
0001  JmpIf       r2, -> 0003
0002  Mov         r2, r1
0003  Mov         r3, r0
0004  JmpIfNot    r3, -> 0006
0005  Mov         r3, r1
0006  Mov         r4, r0
0007  JmpNotNullish r4, -> 0009
0008  Mov         r4, r1
0009  Add         r6, r2, r3
0010  Add         r5, r6, r4
0011  Ret         r5
"#,
    );
}

#[test]
fn while_with_break_and_continue() {
    golden(
        r#"function f(x, y) { while (x < y) { x += 2; if (x == 7) break; if (x === null) continue; } return x; }"#,
        0,
        r#"
code nregs=2 nparams=2 words=8 sites=0
0000  JnLt        r0, r1, -> 0007
0002  AddI        r0, r0, 2
0003  JEqI        r0, -> 0007, 7
0005  JSEqNull    r0, -> 0006
0006  JmpBack     -> 0000
0007  Ret         r0
"#,
    );
}

#[test]
fn for_loop_on_registers() {
    golden(
        r#"function f() { var s = 0; for (var i = 0; i < 200000; i++) { s = (s + i * 3) % 1000003; } return s; }"#,
        0,
        r#"
code nregs=4 nparams=0 words=11 sites=0
0000  LoadI       r0, 0
0001  LoadI       r1, 0
0002  JnLtI       r1, -> 0010, 200000
0004  MulI        r3, r1, 3
0005  Add         r2, r0, r3
0006  ModX        r0, r2, 1000003
0008  Inc         r1, r1
0009  JmpBack     -> 0002
0010  Ret         r0
"#,
    );
}

#[test]
fn for_let_loop_with_a_closure_copies_the_context_fn0() {
    golden(
        r#"function f(a) { var s = 0; for (let i = 0; i < a; i++) { s += i; fs.push(function() { return i; }); } return s; }"#,
        0,
        r#"
code nregs=1 nparams=0 words=3 sites=0
0000  GetCtxTdz   r0, 0, 0, a0
0002  Ret         r0
"#,
    );
}

#[test]
fn for_in_and_for_of() {
    golden(
        r#"function f(o) { var s = 0; for (var k in o) { s += o[k]; if (s > 9) break; } for (const v of o) { s += v; if (v) continue; return s; } return s; }"#,
        0,
        r#"
code nregs=10 nparams=1 words=24 sites=0
0000  LoadUndef   r2
0001  LoadI       r1, 0
0002  ForInOpen   3, r0
0003  IterNext    r6, -> 0011, 3
0005  Mov         r2, r6
0006  GetElem     r7, r0, r2
0007  Add         r1, r1, r7
0008  JGtI        r1, -> 0011, 9
0010  JmpBack     -> 0003
0011  IterOpen    3, r0
0012  IterNext    r7, -> 0023, 3
0014  Mov         r8, r7
0015  Add         r1, r1, r8
0016  JmpIf       r8, -> 0020
0017  Mov         r9, r1
0018  IterClose   3, 0
0019  Ret         r9
0020  JmpBack     -> 0012
0021  IterClose   3, 1
0022  Throw       r6
0023  Ret         r1
"#,
    );
}

#[test]
fn try_catch_finally() {
    golden(
        r#"function f(a) { try { a(); } catch (e) { return e; } finally { a = 0; } return a; }"#,
        0,
        r#"
code nregs=4 nparams=1 words=11 sites=0
0000  Mov         r3, r0
0001  CallU       r2, 3, 0
0002  LoadI       r0, 0
0003  Ret         r0
0004  Mov         r2, r1
0005  Mov         r3, r2
0006  LoadI       r0, 0
0007  Ret         r3
0008  LoadI       r0, 0
0009  Throw       r1
0010  Ret         r0
"#,
    );
}

#[test]
fn break_and_return_run_the_finalizer() {
    golden(
        r#"function f(a) { for (var i = 0; i < 3; i++) { try { if (a) break; return i; } finally { a++; } } }"#,
        0,
        r#"
code nregs=4 nparams=1 words=12 sites=0
0000  LoadI       r1, 0
0001  JnLtI       r1, -> 0011, 3
0003  JmpIfNot    r0, -> 0006
0004  Inc         r0, r0
0005  RetUndef    
0006  Mov         r3, r1
0007  Inc         r0, r0
0008  Ret         r3
0009  Inc         r0, r0
0010  Throw       r2
0011  RetUndef    
"#,
    );
}

#[test]
fn switch_is_a_chain_of_compare_jumps() {
    golden(
        r#"function f(x) { switch (x) { case 1: return 'a'; case 'k': return 'b'; case null: break; default: return 'd'; } return 'e'; }"#,
        0,
        r#"
code nregs=2 nparams=1 words=15 sites=0
0000  JSEqI       r0, -> 0006, 1
0002  JSEqA       r0, -> 0008, a0
0004  JSEqNull    r0, -> 0010
0005  Jmp         -> 0011
0006  LoadAtom    r1, a1
0007  Ret         r1
0008  LoadAtom    r1, a2
0009  Ret         r1
0010  Jmp         -> 0013
0011  LoadAtom    r1, a3
0012  Ret         r1
0013  LoadAtom    r1, a4
0014  Ret         r1
"#,
    );
}

#[test]
fn switch_with_integer_cases_is_a_jump_table() {
    // Fallthrough from 1 to 2, `break` at the end of 2, `default` before the last case. The value 3 has no case: the table gives the default.
    golden(
        r#"function f(a) { var r = 0; switch (a) { case 1: r += 1; case 2: r += 2; break; default: r += 9; case 4: r += 4; } return r; }"#,
        0,
        r#"
code nregs=2 nparams=1 words=8 sites=0
0000  LoadI       r1, 0
0001  SwitchI     r0, t0
      [1 -> 0002, 2 -> 0003, 4 -> 0006, default -> 0005]
0002  AddI        r1, r1, 1
0003  AddI        r1, r1, 2
0004  Ret         r1
0005  AddI        r1, r1, 9
0006  AddI        r1, r1, 4
0007  Ret         r1
"#,
    );
    // The pool: lo 1, default pc 5, length 4, then the targets of 1, 2, 3 and 4.
    assert_eq!(outer(r#"function f(a) { var r = 0; switch (a) { case 1: r += 1; case 2: r += 2; break; default: r += 9; case 4: r += 4; } return r; }"#).code.tables, [1, 5, 4, 2, 3, 5, 6]);
}

#[test]
fn switch_with_string_cases_is_a_hash_table() {
    golden(
        r#"function g(x) { switch (x) { case 'a': return 1; case 'b': return 2; case 'c': return 3; } return 0; }"#,
        0,
        r#"
code nregs=2 nparams=1 words=9 sites=0
0000  SwitchA     r0, t0
      [a0 -> 0001, a1 -> 0003, a2 -> 0005, default -> 0007]
0001  LoadI       r1, 1
0002  Ret         r1
0003  LoadI       r1, 2
0004  Ret         r1
0005  LoadI       r1, 3
0006  Ret         r1
0007  LoadI       r1, 0
0008  Ret         r1
"#,
    );
    // 3 cases: 4 slots (a power of two, at least 4/3 for each case), so the pool is the default, the mask and 4 slots of 3 words.
    let c = outer(r#"function g(x) { switch (x) { case 'a': return 1; case 'b': return 2; case 'c': return 3; } return 0; }"#);
    assert_eq!((c.code.tables.len(), c.code.tables[0], c.code.tables[1]), (2 + 4 * 3, 7, 3));
}

#[test]
fn a_duplicate_case_gets_no_table_entry() {
    // The first `case 1` wins, as in a chain. The body of the second one is unreachable and is dropped.
    golden(
        r#"function h(x) { switch (x) { case 1: return 1; case 2: return 2; case 1: return 3; case 3: return 4; } return 0; }"#,
        0,
        r#"
code nregs=2 nparams=1 words=9 sites=0
0000  SwitchI     r0, t0
      [1 -> 0001, 2 -> 0003, 3 -> 0005, default -> 0007]
0001  LoadI       r1, 1
0002  Ret         r1
0003  LoadI       r1, 2
0004  Ret         r1
0005  LoadI       r1, 4
0006  Ret         r1
0007  LoadI       r1, 0
0008  Ret         r1
"#,
    );
}

#[test]
fn a_switch_keeps_the_chain_when_it_is_cheaper_or_cannot_be_a_table() {
    // `Some(op)`: the table op; `None`: the chain.
    let cases: &[(&str, Option<Op>)] = &[
        // The number of cases: 3 is the least; the same value twice counts once.
        ("switch (x) { case 1: return 1; case 2: return 2; case 3: return 3; }", Some(Op::SwitchI)),
        ("switch (x) { case 1: return 1; case 2: return 2; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case 2: return 3; case 1: return 4; }", None),
        ("switch (x) { case 'a': return 1; case 'b': return 2; case 'c': return 3; }", Some(Op::SwitchA)),
        ("switch (x) { case 'a': return 1; case 'b': return 2; }", None),
        ("switch (x) { case 'a': return 1; case 'b': return 2; case 'a': return 3; }", None),
        // The density: the span (largest minus least plus 1) must be below 3 for each distinct case. Here 3 cases.
        ("switch (x) { case 0: return 1; case 1: return 2; case 7: return 3; }", Some(Op::SwitchI)),
        ("switch (x) { case 0: return 1; case 1: return 2; case 8: return 3; }", None),
        ("switch (x) { case 1: return 1; case 10: return 2; case 100: return 3; case 1000: return 4; }", None),
        // Negative values and the ends of the 32-bit range.
        ("switch (x) { case -2: return 1; case -1: return 2; case 0: return 3; }", Some(Op::SwitchI)),
        ("switch (x) { case -2147483647: return 1; case 2147483647: return 2; case 0: return 3; }", None),
        ("switch (x) { case 2147483645: return 1; case 2147483646: return 2; case 2147483647: return 3; }", Some(Op::SwitchI)),
        // A case that is not an integer or string constant keeps the whole chain, where it is.
        ("switch (x) { case 1: return 1; case 2: return 2; case 3: return 3; case y: return 4; }", None),
        ("switch (x) { case y: return 4; case 1: return 1; case 2: return 2; case 3: return 3; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case 3: return g(); case 4: return 4; case g(): return 5; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case 3 + 1: return 3; case 5: return 4; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case 1.5: return 3; case 3: return 4; }", None),
        ("switch (x) { case 0: return 1; case 1: return 2; case -0: return 3; case 2: return 4; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case 'a': return 3; case 3: return 4; }", None),
        ("switch (x) { case 1: return 1; case 2: return 2; case null: return 3; case 3: return 4; }", None),
        ("switch (x) { case true: return 1; case false: return 2; case null: return 3; case undefined: return 4; }", None),
        // `default` anywhere, and an empty case.
        ("switch (x) { default: return 0; case 1: case 2: case 3: return 1; }", Some(Op::SwitchI)),
        ("switch (x) { case 1: case 2: default: case 3: return 1; }", Some(Op::SwitchI)),
        ("switch (x) { default: return 0; }", None),
    ];
    for (body, want) in cases {
        let src = format!("function f(x, y) {{ {body} return 9; }}");
        let c = outer(&src);
        let dis = c.code.disassemble();
        for op in [Op::SwitchI, Op::SwitchA] {
            assert_eq!(dis.contains(op.name()), *want == Some(op), "{body}\n{dis}");
        }
    }
}

#[test]
fn object_and_array_patterns() {
    golden(
        r#"function f(o) { var {a, b: {c}} = o; var [p, , q = 3, ...r] = o; return a + c + p + q; }"#,
        0,
        r#"
code nregs=13 nparams=1 words=48 sites=3
0000  LoadUndef   r1
0001  LoadUndef   r2
0002  LoadUndef   r3
0003  LoadUndef   r4
0004  LoadUndef   r5
0005  Mov         r6, r0
0006  GetField    r1, r6, s0
0007  GetField    r7, r6, s1
0008  GetField    r2, r7, s2
0009  Mov         r6, r0
0010  IterOpen    7, r6
0011  LoadFalse   r10
0012  JmpIf       r10, -> 0017
0013  IterNext    r11, -> 0016, 7
0015  Jmp         -> 0018
0016  LoadTrue    r10
0017  LoadUndef   r11
0018  Mov         r3, r11
0019  JmpIf       r10, -> 0024
0020  IterNext    r11, -> 0023, 7
0022  Jmp         -> 0025
0023  LoadTrue    r10
0024  LoadUndef   r11
0025  JmpIf       r10, -> 0030
0026  IterNext    r11, -> 0029, 7
0028  Jmp         -> 0031
0029  LoadTrue    r10
0030  LoadUndef   r11
0031  Mov         r4, r11
0032  JSNeUndef   r4, -> 0034
0033  LoadI       r4, 3
0034  NewArray    r11, 0
0035  JmpIf       r10, -> 0041
0036  IterNext    r12, -> 0040, 7
0038  ArrayPush   r11, r12
0039  JmpBack     -> 0035
0040  LoadTrue    r10
0041  Mov         r5, r11
0042  JmpIf       r10, -> 0044
0043  IterClose   7, 0
0044  Add         r8, r1, r2
0045  Add         r7, r8, r3
0046  Add         r6, r7, r4
0047  Ret         r6
"#,
    );
}

#[test]
fn labels_break_and_continue_outer_loops() {
    golden(
        r#"function f(n) { outer: for (var i = 0; i < n; i++) { for (var j = 0; j < n; j++) { if (j == 2) continue outer; if (i == 3) break outer; } } }"#,
        0,
        r#"
code nregs=3 nparams=1 words=17 sites=0
0000  LoadUndef   r1
0001  LoadUndef   r2
0002  LoadI       r1, 0
0003  JnLt        r1, r0, -> 0016
0005  LoadI       r2, 0
0006  JnLt        r2, r0, -> 0014
0008  JEqI        r2, -> 0014, 2
0010  JEqI        r1, -> 0016, 3
0012  Inc         r2, r2
0013  JmpBack     -> 0006
0014  Inc         r1, r1
0015  JmpBack     -> 0003
0016  RetUndef    
"#,
    );
}

#[test]
fn captured_variables_live_in_context_slots_fn0() {
    golden(
        r#"function mk(n) { var c = n; let d = 1; return function() { c = c + d; return c; }; }"#,
        0,
        r#"
code nregs=3 nparams=0 words=7 sites=0
0000  GetCtx      r1, 0, 0
0001  GetCtxTdz   r2, 0, 1, a0
0003  Add         r0, r1, r2
0004  SetCtx      r0, 0, 0
0005  GetCtx      r0, 0, 0
0006  Ret         r0
"#,
    );
}

#[test]
fn captured_variables_live_in_context_slots_fn1() {
    golden(
        r#"function mk(n) { var c = n; let d = 1; return function() { c = c + d; return c; }; }"#,
        1,
        r#"
code nregs=2 nparams=1 words=6 sites=0
0000  PushCtx     2
0001  InitCtx     r0, 0, 0
0002  LoadI       r1, 1
0003  InitCtx     r1, 0, 1
0004  Closure     r1, f0
0005  Ret         r1
"#,
    );
}

#[test]
fn let_read_early_by_a_closure_starts_as_the_hole_fn0() {
    golden(
        r#"function f() { var g = function() { return z; }; let z = 1; { let w = 2; return w + z; } }"#,
        0,
        r#"
code nregs=1 nparams=0 words=3 sites=0
0000  GetCtxTdz   r0, 0, 0, a0
0002  Ret         r0
"#,
    );
}

#[test]
fn let_read_early_by_a_closure_starts_as_the_hole_fn1() {
    golden(
        r#"function f() { var g = function() { return z; }; let z = 1; { let w = 2; return w + z; } }"#,
        1,
        r#"
code nregs=4 nparams=0 words=8 sites=0
0000  PushCtx     1
0001  Closure     r0, f0
0002  LoadI       r1, 1
0003  InitCtx     r1, 0, 0
0004  LoadI       r1, 2
0005  GetCtx      r3, 0, 0
0006  Add         r2, r1, r3
0007  Ret         r2
"#,
    );
}

#[test]
fn array_literal_is_one_op() {
    golden(
        r#"function f(a) { return [1, 2, a, 'z']; }"#,
        0,
        r#"
code nregs=6 nparams=1 words=6 sites=0
0000  LoadI       r2, 1
0001  LoadI       r3, 2
0002  Mov         r4, r0
0003  LoadAtom    r5, a0
0004  NewArrLit   r1, 2, 4
0005  Ret         r1
"#,
    );
}

#[test]
fn object_literal_is_one_op() {
    golden(
        r#"function f(a) { return {x: 1, y: a, 'z': 2}; }"#,
        0,
        r#"
code nregs=5 nparams=1 words=5 sites=0
0000  LoadI       r2, 1
0001  Mov         r3, r0
0002  LoadI       r4, 2
0003  NewObjLit   r1, 2, h0
0004  Ret         r1
"#,
    );
}

#[test]
fn template_literal_joins_strings() {
    golden(
        r#"function f(a) { return `a${a}b${a + 1}`; }"#,
        0,
        r#"
code nregs=6 nparams=1 words=8 sites=0
0000  LoadAtom    r2, a0
0001  Mov         r3, r0
0002  ToStr       r3, r3
0003  LoadAtom    r4, a1
0004  AddI        r5, r0, 1
0005  ToStr       r5, r5
0006  Concat      r1, 2, 4
0007  Ret         r1
"#,
    );
}

#[test]
fn member_stores_of_constants() {
    golden(
        r#"function f(o, k) { o.a = 1; o.b = 'x'; o.c = null; o[k] = o.a; o[0] = k; var t = o[3]; return t; }"#,
        0,
        r#"
code nregs=4 nparams=2 words=9 sites=4
0000  SetFieldI   r0, 1, s0
0001  SetFieldA   r0, a1, s1
0002  SetFieldK   r0, 1, s2
0003  GetField    r3, r0, s3
0004  SetElem     r0, r1, r3
0005  LoadI       r3, 0
0006  SetElem     r0, r3, r1
0007  GetElemI    r2, r0, 3
0008  Ret         r2
"#,
    );
}

#[test]
fn calls_methods_new_and_optional_chains() {
    golden(
        r#"function f(o, k) { return o.m(1, 2) + o?.q?.r + new o.C(k); }"#,
        0,
        r#"
code nregs=9 nparams=2 words=17 sites=4
0000  Mov         r6, r0
0001  GetField    r5, r6, s0
0002  LoadI       r7, 1
0003  LoadI       r8, 2
0004  Call        r4, 5, 2
0005  JmpNullish  r0, -> 0010
0006  GetField    r6, r0, s1
0007  JmpNullish  r6, -> 0010
0008  GetField    r5, r6, s2
0009  Jmp         -> 0011
0010  LoadUndef   r5
0011  Add         r3, r4, r5
0012  GetField    r5, r0, s3
0013  Mov         r6, r1
0014  New         r4, 5, 1
0015  Add         r2, r3, r4
0016  Ret         r2
"#,
    );
}

#[test]
fn update_expressions() {
    golden(
        r#"function f(o, k) { var a = o.x++; --o.y; o[k] += 2; o[k]--; k++; return a + k++; }"#,
        0,
        r#"
code nregs=5 nparams=2 words=19 sites=4
0000  GetField    r3, r0, s0
0001  ToNumeric   r3, r3
0002  Inc         r4, r3
0003  SetField    r0, r4, s1
0004  Mov         r2, r3
0005  GetField    r3, r0, s2
0006  Dec         r3, r3
0007  SetField    r0, r3, s3
0008  GetElem     r3, r0, r1
0009  AddI        r3, r3, 2
0010  SetElem     r0, r1, r3
0011  GetElem     r3, r0, r1
0012  Dec         r3, r3
0013  SetElem     r0, r1, r3
0014  Inc         r1, r1
0015  ToNumeric   r4, r1
0016  Inc         r1, r4
0017  Add         r3, r2, r4
0018  Ret         r3
"#,
    );
}

#[test]
fn parameter_defaults_and_patterns_fn0() {
    golden(
        r#"function f(a, b = 2, {c, d} = a) { return () => a + b + c; }"#,
        0,
        r#"
code nregs=4 nparams=0 words=6 sites=0
0000  GetCtx      r2, 0, 0
0001  GetCtx      r3, 0, 1
0002  Add         r1, r2, r3
0003  GetCtx      r2, 0, 2
0004  Add         r0, r1, r2
0005  Ret         r0
"#,
    );
}

#[test]
fn script_top_level_uses_globals() {
    golden(
        r#"var s = 0; for (var i = 0; i < 10; i++) { s += i; } function g() { return s; }"#,
        SCRIPT,
        r#"
code nregs=2 nparams=0 words=21 sites=12
0000  DeclGlobal  s0
0001  DeclGlobal  s1
0002  DeclGlobal  s2
0003  Closure     r0, f0
0004  SetGlobal   r0, s3
0005  LoadI       r0, 0
0006  SetGlobal   r0, s4
0007  LoadI       r0, 0
0008  SetGlobal   r0, s5
0009  GetGlobal   r0, s6
0010  JnLtI       r0, -> 0020, 10
0012  GetGlobal   r0, s7
0013  GetGlobal   r1, s8
0014  Add         r0, r0, r1
0015  SetGlobal   r0, s9
0016  GetGlobal   r0, s10
0017  Inc         r0, r0
0018  SetGlobal   r0, s11
0019  JmpBack     -> 0009
0020  RetUndef    
"#,
    );
}

#[test]
fn eval_load_sources_compile_and_verify() {
    let cases = [
        include_str!("../examples/data/eval_loop.js"),
        include_str!("../examples/data/eval_loopfn.js"),
        include_str!("../examples/data/eval_calls.js"),
        include_str!("../examples/data/eval_fib.js"),
        include_str!("../examples/data/eval_closures.js"),
        include_str!("../examples/data/eval_objects.js"),
        include_str!("../examples/data/eval_webpack.js"),
        include_str!("../examples/data/eval_throws.js"),
        include_str!("../examples/data/eval_concat.js"),
    ];
    for src in cases {
        let (s, fs) = build(src);
        assert!(s.unsupported.is_empty() && fs.iter().all(|f| f.unsupported.is_empty()), "{src}");
    }
}

/// Many shapes of control flow and scopes. `build` runs the verifier on each function: no path reads an unwritten register, the
/// context depth agrees at every join, and every root map names only registers that every path wrote.
#[test]
fn control_flow_and_scope_shapes_pass_the_verifier() {
    let (s, fs) = build(SMOKE);
    assert!(s.unsupported.is_empty());
    assert_eq!(fs.len(), 46, "36 functions and their inner functions");
    for f in &fs {
        assert!(f.unsupported.is_empty(), "{:?}", f.unsupported);
    }
}

const SMOKE: &str = r#"
function t1(a) { l: { if (a) break l; a = 1; } return a; }
function t2(a) { for (var i = 0; i < 3; i++) { switch (i) { case 0: continue; case 1: break; default: a++; } } return a; }
function t3(a) { for (var i = 0; i < 3; i++) { try { try { if (a) continue; } finally { a++; } } finally { a--; } } }
function t4(a) { try { return 1; } finally { for (var i = 0; i < 2; i++) { if (i) break; a++; } } }
function t5(a) { try { throw a; } catch (e) { try { throw e; } finally { a = 2; } } finally { a = 3; } }
function t6(a) { for (let i = 0; i < 3; i++) { fs.push(() => i); if (a) break; if (!a) continue; } }
function t7(a) { for (const [k, v] of a) { fs.push(() => k + v); if (k) return v; } }
function t8(a) { switch (a) { case 1: let x = 1; fs.push(() => x); case 2: x = 2; break; default: } }
function t9(a) { try { a(); } catch ({message}) { fs.push(() => message); } }
function t10(a) { for (var k in a) { try { if (k) continue; } catch (e) { break; } finally { a = 1; } } }
function t11(a) { while (true) { try { return a; } finally { if (a) break; } } return 2; }
function t12(a) { do { if (a) continue; a++; } while (a < 5); }
function t13(a) { for (let i = 0, j = 1; i < a; i++, j--) { let k = i + j; fs.push(() => k); } }
function t14(a) { { function inner() { return a; } let z = inner; } return a; }
function t15(a) { 'use strict'; { function inner() { return a; } inner(); } return a; }
function t16(a) { var f = function g() { return 1; }; return f; }
function t17(a) { a = a || {}; a.b = a.b || []; return a?.b?.[0]?.(1); }
function t18(a, b) { return a ? b ? 1 : 2 : b ? 3 : 4; }
function t19(a, b) { return (a, b, a + b) * (a - b) / (a % b) ** 2; }
function t20(a) { var x; if (a) { x = 1; } else { x = 2; } return x; }
function t21(a) { let x; for (;;) { x = a; if (x) break; } return x; }
function t22(a) { for (var x of a) for (var y of x) { if (y) break; else continue; } }
function t23(a) { try { for (var x of a) { if (x) return x; } } finally { a = 0; } }
function t24(a) { outer: for (var x of a) { for (var y of x) { if (y) continue outer; if (x) break outer; } } }
function t25(a) { var o = {a, b: a, [a]: 1, c() { return 1; }}; return o; }
function t26(a) { return a in {} || a instanceof Object || !(a < 1) || !(a >= 2); }
function t27(a) { var s = 0; for (var i = a.length; i--;) s += i; return s; }
function t28(a) { if (a) ; else if (!a) { } else return 1; }
function t29(a) { return typeof a === 'string' ? a : typeof a == 'undefined'; }
function t30(a) { let b = a; { let b = 1; b++; } return b; }
function t31(a) { switch (a) { default: a++; case 1: a--; } }
function t32(a) { var i = 0; while (i < 10) { if (i++ > 5) break; } return i; }
function t33(a) { a.b.c = a.d.e += a.f; a[a.g] ||= 3; a.h &&= 4; a.i ??= 5; }
function t34(a) { var x = 1; x ||= a; x &&= 2; x ??= 3; x **= 2; x <<= 1; return x; }
function t35(a) { for (var i = 0; i < 2; i++) { let c = i; var f = function() { return c; }; if (f()) break; } }
function t36(a) { try { throw 1; } catch { return 2; } }
"#;

#[test]
fn the_arguments_object_is_made_at_entry() {
    let src = r#"function f() { return arguments.length + arguments[0]; }"#;
    golden(
        src,
        0,
        r#"
code nregs=4 nparams=0 words=5 sites=1
0000  CreateArgs  r0
0001  GetField    r2, r0, s0
0002  GetElemI    r3, r0, 0
0003  Add         r1, r2, r3
0004  Ret         r1
"#,
    );
}

#[test]
fn a_rest_parameter_and_a_spread_call() {
    let src = r#"function f(a, ...r) { return g(...a, 1); }"#;
    golden(
        src,
        0,
        r#"
code nregs=7 nparams=2 words=9 sites=1
0000  CreateRest  r1, 1
0001  GetGlobal   r3, s0
0002  LoadUndef   r4
0003  NewArray    r5, 0
0004  ArraySpread r5, r0
0005  LoadI       r6, 1
0006  ArrayPush   r5, r6
0007  CallSpread  r2, 3
0008  Ret         r2
"#,
    );
}

#[test]
fn spread_in_new_and_in_array_literals() {
    let src = r#"function f(a) { return [new C(...a), [1, , ...a]]; }"#;
    golden(
        src,
        0,
        r#"
code nregs=6 nparams=1 words=11 sites=1
0000  GetGlobal   r4, s0
0001  NewArray    r5, 0
0002  ArraySpread r5, r0
0003  NewSpread   r2, 4
0004  NewArray    r3, 0
0005  LoadI       r4, 1
0006  ArrayPush   r3, r4
0007  ArrayHole   r3
0008  ArraySpread r3, r0
0009  NewArrLit   r1, 2, 2
0010  Ret         r1
"#,
    );
}

#[test]
fn a_named_function_expression_reads_its_own_name() {
    let src = r#"var g = function fact(n) { return n ? n * fact(n - 1) : 1; };"#;
    golden(
        src,
        0,
        r#"
code nregs=6 nparams=1 words=9 sites=0
0000  LoadCallee  r1
0001  JmpIfNot    r0, -> 0007
0002  Mov         r4, r1
0003  SubI        r5, r0, 1
0004  CallU       r3, 4, 1
0005  Mul         r2, r0, r3
0006  Ret         r2
0007  LoadI       r2, 1
0008  Ret         r2
"#,
    );
}

#[test]
fn regexp_and_bigint_literals() {
    let src = r#"function f(o) { return /x+/gi.test(o) + 10n; }"#;
    golden(
        src,
        0,
        r#"
code nregs=6 nparams=1 words=7 sites=1
0000  NewRegExp   r4, a0, a1
0001  GetField    r3, r4, s0
0002  Mov         r5, r0
0003  Call        r2, 3, 1
0004  LoadBigInt  r3, a3
0005  Add         r1, r2, r3
0006  Ret         r1
"#,
    );
}

#[test]
fn a_tagged_template() {
    let src = r#"function f(o) { return tag`a${o}b`; }"#;
    golden(
        src,
        0,
        r#"
code nregs=5 nparams=1 words=5 sites=2
0000  GetGlobal   r2, s0
0001  GetTemplate r3, h0, s1
0002  Mov         r4, r0
0003  CallU       r1, 2, 2
0004  Ret         r1
"#,
    );
}

#[test]
fn object_literal_accessors_proto_and_spread() {
    let src = r#"function f(x) { return { get a() { return 1; }, set a(v) {}, __proto__: null, ...x, m() { return super.m(); } }; }"#;
    golden(
        src,
        3,
        r#"
code nregs=3 nparams=1 words=14 sites=3
0000  NewObject   r1, 5
0001  Closure     r2, f0
0002  DefAccessor r1, r2, s0, 0
0004  Closure     r2, f1
0005  DefAccessor r1, r2, s1, 1
0007  LoadNull    r2
0008  SetProto    r1, r2
0009  CopyProps   r1, r0
0010  Closure     r2, f2
0011  SetHome     r2, r1
0012  DefField    r1, r2, s2
0013  Ret         r1
"#,
    );
}

#[test]
fn object_rest_pattern() {
    let src = r#"function f(o) { var {a, ...r} = o; return r; }"#;
    golden(
        src,
        0,
        r#"
code nregs=6 nparams=1 words=9 sites=1
0000  LoadUndef   r1
0001  LoadUndef   r2
0002  Mov         r3, r0
0003  LoadAtom    r4, a0
0004  GetField    r1, r3, s0
0005  ObjRest     r5, r3, 4, 1
0007  Mov         r2, r5
0008  Ret         r2
"#,
    );
}

#[test]
fn class_methods_fields_and_static_elements() {
    let src = r#"class A { static a = 1; static { this.b = 2; } [k] = 3; m() { return 1; } }"#;
    golden(
        src,
        SCRIPT,
        r#"
code nregs=6 nparams=0 words=35 sites=6
0000  CheckLex    s0
0001  LoadHole    r0
0002  PushCtx     1
0003  DefaultCtor r2, 0
0004  LoadAtom    r3, a0
0005  SetFnName   r2, r3, 0
0006  MakeClass   r2
0007  GetField    r3, r2, s1
0008  GetGlobal   r4, s2
0009  ToKey       r5, r3, r4
0010  InitCtx     r5, 0, 0
0011  Closure     r4, f0
0012  DefMethod   r3, r4, s3
0013  Closure     r4, f1
0014  SetFields   r2, r4
0015  Closure     r4, f2
0016  Mov         r5, r2
0017  Call        r4, 4, 0
0018  Closure     r4, f3
0019  Mov         r5, r2
0020  Call        r4, 4, 0
0021  Mov         r1, r2
0022  PopCtx
0023  Mov         r0, r1
0024  Jmp         -> 0030
0025  PushCtx     1
0026  InitCtx     r0, 0, 0
0027  DeclLex     0, s4
0028  PopCtx
0029  Throw       r1
0030  PushCtx     1
0031  InitCtx     r0, 0, 0
0032  DeclLex     0, s5
0033  PopCtx
0034  RetUndef
"#,
    );
    golden(
        src,
        0,
        r#"
code nregs=2 nparams=0 words=4 sites=1
0000  LoadThis    r0
0001  LoadI       r1, 1
0002  DefField    r0, r1, s0
0003  RetUndef
"#,
    );
    golden(
        src,
        1,
        r#"
code nregs=1 nparams=0 words=3 sites=1
0000  LoadThis    r0
0001  SetFieldI   r0, 2, s0
0002  RetUndef
"#,
    );
    golden(
        src,
        2,
        r#"
code nregs=3 nparams=0 words=5 sites=0
0000  LoadThis    r0
0001  GetCtx      r1, 0, 0
0002  LoadI       r2, 3
0003  DefElem     r0, r1, r2
0004  RetUndef
"#,
    );
    golden(
        src,
        3,
        r#"
code nregs=1 nparams=0 words=2 sites=0
0000  LoadI       r0, 1
0001  Ret         r0
"#,
    );
}

#[test]
fn a_derived_class_calls_super_in_an_arrow() {
    let src = r#"class A extends B { constructor() { var f = () => super(); f(); } }"#;
    golden(
        src,
        SCRIPT,
        r#"
code nregs=5 nparams=0 words=19 sites=5
0000  CheckLex    s0
0001  LoadHole    r0
0002  GetGlobal   r2, s1
0003  Closure     r3, f0
0004  MakeDerived r3, r2
0005  GetField    r4, r3, s2
0006  Mov         r1, r3
0007  Mov         r0, r1
0008  Jmp         -> 0014
0009  PushCtx     1
0010  InitCtx     r0, 0, 0
0011  DeclLex     0, s3
0012  PopCtx
0013  Throw       r1
0014  PushCtx     1
0015  InitCtx     r0, 0, 0
0016  DeclLex     0, s4
0017  PopCtx
0018  RetUndef
"#,
    );
    golden(
        src,
        0,
        r#"
code nregs=3 nparams=0 words=5 sites=0
0000  CallSuper   r0, 1, 0
0001  GetCtx      r2, 0, 0
0002  CheckUnbound r2
0003  InitCtx     r0, 0, 0
0004  Ret         r0
"#,
    );
    golden(
        src,
        1,
        r#"
code nregs=3 nparams=0 words=7 sites=0
0000  PushCtx     1
0001  Closure     r0, f0
0002  Mov         r2, r0
0003  CallU       r1, 2, 0
0004  LoadUndef   r1
0005  GetCtx      r2, 0, 0
0006  RetDerived  r1, r2
"#,
    );
}

#[test]
fn super_property_reads_and_writes() {
    let src = r#"class A extends B { m() { super.x = 1; super.y += 2; super[k] = 3; return super.z++; } }"#;
    golden(
        src,
        SCRIPT,
        r#"
code nregs=6 nparams=0 words=24 sites=6
0000  CheckLex    s0
0001  LoadHole    r0
0002  GetGlobal   r2, s1
0003  DefaultCtor r3, 1
0004  LoadAtom    r4, a0
0005  SetFnName   r3, r4, 0
0006  MakeDerived r3, r2
0007  GetField    r4, r3, s2
0008  Closure     r5, f0
0009  SetHome     r5, r4
0010  DefMethod   r4, r5, s3
0011  Mov         r1, r3
0012  Mov         r0, r1
0013  Jmp         -> 0019
0014  PushCtx     1
0015  InitCtx     r0, 0, 0
0016  DeclLex     0, s4
0017  PopCtx
0018  Throw       r1
0019  PushCtx     1
0020  InitCtx     r0, 0, 0
0021  DeclLex     0, s5
0022  PopCtx
0023  RetUndef
"#,
    );
    golden(
        src,
        0,
        r#"
code nregs=4 nparams=0 words=18 sites=6
0000  LoadThis    r0
0001  LoadI       r1, 1
0002  SetSuper    r0, r1, s0
0003  LoadThis    r0
0004  GetSuper    r1, r0, s1
0005  AddI        r1, r1, 2
0006  SetSuper    r0, r1, s2
0007  LoadThis    r0
0008  GetGlobal   r1, s3
0009  LoadI       r2, 3
0010  SetSuperElem r0, r1, r2
0011  LoadThis    r1
0012  GetSuper    r2, r1, s4
0013  ToNumeric   r2, r2
0014  Inc         r3, r2
0015  SetSuper    r1, r3, s5
0016  Mov         r0, r2
0017  Ret         r0
"#,
    );
}

#[test]
fn private_names_methods_and_accessors() {
    let src = r#"class A { #x = 1; static #m() {} #n() { return 1; } get #g() { return this.#x; } inc() { this.#x++; this.#x += 2; return #x in this; } }"#;
    golden(
        src,
        SCRIPT,
        r#"
code nregs=6 nparams=0 words=41 sites=5
0000  CheckLex    s0
0001  LoadHole    r0
0002  PushCtx     6
0003  NewPrivateName r2, a1
0004  InitCtx     r2, 0, 0
0005  NewPrivateName r2, a2
0006  InitCtx     r2, 0, 1
0007  NewPrivateName r2, a3
0008  InitCtx     r2, 0, 2
0009  NewPrivateName r2, a4
0010  InitCtx     r2, 0, 4
0011  DefaultCtor r2, 0
0012  LoadAtom    r3, a0
0013  SetFnName   r2, r3, 0
0014  MakeClass   r2
0015  GetField    r3, r2, s1
0016  Closure     r4, f0
0017  GetCtx      r5, 0, 1
0018  DefPrivateMethod r2, r5, r4
0019  Closure     r4, f1
0020  InitCtx     r4, 0, 3
0021  Closure     r4, f2
0022  InitCtx     r4, 0, 5
0023  Closure     r4, f3
0024  DefMethod   r3, r4, s2
0025  Closure     r4, f4
0026  SetFields   r2, r4
0027  Mov         r1, r2
0028  PopCtx
0029  Mov         r0, r1
0030  Jmp         -> 0036
0031  PushCtx     1
0032  InitCtx     r0, 0, 0
0033  DeclLex     0, s3
0034  PopCtx
0035  Throw       r1
0036  PushCtx     1
0037  InitCtx     r0, 0, 0
0038  DeclLex     0, s4
0039  PopCtx
0040  RetUndef
"#,
    );
    golden(
        src,
        0,
        r#"
code nregs=3 nparams=0 words=12 sites=0
0000  LoadThis    r0
0001  GetCtx      r1, 0, 2
0002  GetCtx      r2, 0, 3
0003  DefPrivateMethod r0, r1, r2
0004  GetCtx      r1, 0, 4
0005  GetCtx      r2, 0, 5
0006  DefPrivateAccessor r0, r1, r2, 0
0008  GetCtx      r1, 0, 0
0009  LoadI       r2, 1
0010  DefPrivate  r0, r1, r2
0011  RetUndef
"#,
    );
    golden(
        src,
        1,
        r#"
code nregs=1 nparams=0 words=1 sites=0
0000  RetUndef
"#,
    );
    golden(
        src,
        2,
        r#"
code nregs=1 nparams=0 words=2 sites=0
0000  LoadI       r0, 1
0001  Ret         r0
"#,
    );
    golden(
        src,
        3,
        r#"
code nregs=3 nparams=0 words=4 sites=0
0000  LoadThis    r1
0001  GetCtx      r2, 0, 0
0002  GetPrivate  r0, r1, r2
0003  Ret         r0
"#,
    );
    golden(
        src,
        4,
        r#"
code nregs=3 nparams=0 words=14 sites=0
0000  LoadThis    r0
0001  GetCtx      r1, 0, 0
0002  GetPrivate  r2, r0, r1
0003  Inc         r2, r2
0004  SetPrivate  r0, r1, r2
0005  LoadThis    r0
0006  GetCtx      r1, 0, 0
0007  GetPrivate  r2, r0, r1
0008  AddI        r2, r2, 2
0009  SetPrivate  r0, r1, r2
0010  LoadThis    r1
0011  GetCtx      r2, 0, 0
0012  HasPrivate  r0, r1, r2
0013  Ret         r0
"#,
    );
}

#[test]
fn new_target_in_an_arrow_is_captured() {
    let src = r#"function f() { return () => new.target; }"#;
    golden(
        src,
        0,
        r#"
code nregs=1 nparams=0 words=2 sites=0
0000  GetCtx      r0, 0, 0
0001  Ret         r0
"#,
    );
    golden(
        src,
        1,
        r#"
code nregs=1 nparams=0 words=5 sites=0
0000  PushCtx     1
0001  LoadNewTarget r0
0002  InitCtx     r0, 0, 0
0003  Closure     r0, f0
0004  Ret         r0
"#,
    );
}

#[test]
fn a_class_expression_name_is_a_binding_of_the_class() {
    let src = r#"var C = class N { m() { return N; } };"#;
    golden(
        src,
        SCRIPT,
        r#"
code nregs=4 nparams=0 words=14 sites=4
0000  DeclGlobal  s0
0001  PushCtx     1
0002  DefaultCtor r1, 0
0003  LoadAtom    r2, a1
0004  SetFnName   r1, r2, 0
0005  MakeClass   r1
0006  GetField    r2, r1, s1
0007  Closure     r3, f0
0008  DefMethod   r2, r3, s2
0009  InitCtx     r1, 0, 0
0010  Mov         r0, r1
0011  PopCtx      
0012  SetGlobal   r0, s3
0013  RetUndef
"#,
    );
    golden(
        src,
        0,
        r#"
code nregs=1 nparams=0 words=3 sites=0
0000  GetCtxTdz   r0, 0, 0, a0
0002  Ret         r0
"#,
    );
}
#[test]
fn field_initializers_and_static_blocks_are_ready_children_of_the_class() {
    // The text of an initializer is in the tree of the function that holds the class, so the parent compiles it: `ready` points to
    // `Compiled::inline` (the driver `compile_all` takes the code from there, and `build` verifies it). The initializer reads `a` of the function around it, so `a` is a context slot.
    let (s, fs) = build("function f() { const a = 1; return class { x = a; static y = a; static { a; } }; }");
    assert!(s.children.iter().all(|c| c.ready.is_none()));
    let f = fs.last().expect("f");
    assert!(f.unsupported.is_empty(), "{:?}", f.unsupported);
    assert_eq!(f.children.len(), 3);
    assert!(f.children.iter().all(|c| c.ready.is_some()));
    assert!(f.code.disassemble().contains("PushCtx"), "{}", f.code.disassemble());
}

#[test]
fn every_construct_of_a_class_passes_the_verifier() {
    let srcs = [
        "class A { constructor(x) { this.x = x; } static of(x) { return new A(x); } get v() { return this.x; } set v(y) { this.x = y; } }",
        "class A extends B { x = super.y; static z = super.w; constructor() { super(...arguments); } }",
        "class A { static #p = 1; static get #q() { return A.#p; } static set #q(v) { A.#p = v; } static m() { return A.#q; } }",
        "class A { ['a' + 1]() {} static [Symbol.iterator]() {} get [k]() { return 1; } 1() {} 'b c'() {} }",
        "function f() { class A { m() { return () => this; } } return class extends A { constructor() { var a = () => super(); a(); return {}; } }; }",
        "class A { x = () => this; y = function() { return this; }; z = class { w = 1; }; }",
        "var C = class { static a = C; }",
        "class A extends B { constructor() { try { super(); return; } finally { this.x = 1; } } }",
        "function f() { for (let i = 0; i < 3; i++) { class A { m() { return i; } static s = i; } g(A); } { class Z {} } }",
        "function f(a = arguments[1], ...[b, c]) { return () => arguments.length + b; }",
        "function f({a, ...b}, [c, ...d]) { ({x: a.y, ...b.z} = c); }",
        "var o = { get [k]() { return 1; }, set 5(v) {}, __proto__, '__proto__': 1, ...a, ...b, m() { return super.m(); } };",
    ];
    for src in srcs {
        let (s, fs) = build(src);
        assert!(s.unsupported.is_empty(), "{src}: {:?}", s.unsupported);
        for f in &fs {
            assert!(f.unsupported.is_empty(), "{src}: {:?}", f.unsupported);
        }
    }
}

#[test]
fn constructs_the_compiler_does_not_build_are_listed() {
    let cases = [
        ("with (a) { b; }", "with"),
        ("eval('1');", "direct eval or with"),
    ];
    for (body, what) in cases {
        let f = outer(&format!("function f(a) {{ {body} }}"));
        assert!(f.unsupported.contains(&what), "{body}: {:?}", f.unsupported);
    }
}

#[test]
fn handler_ranges_leave_out_the_inlined_finalizers() {
    // The `try` block is 7..20 and the `catch` clause 24..30. The finalizer is inlined after each of them (20..23, 30..33) and once more
    // for an exception (34..37). Neither inlined copy is inside a range, so an exception in it goes to the next handler out.
    let (s, _) = build("var c = 0; for (var i = 0; i < 9; i++) { try { if (i % 3 == 0) throw new Error('e' + i); c += 1; } catch (e) { c += 2; } finally { c++; } }");
    let h: Vec<(u32, u32, u32)> = s.code.handlers.iter().map(|h| (h.start, h.end, h.target)).collect();
    let text = s.code.disassemble();
    let at = |op: &str| text.lines().filter(|l| l.contains(op)).count();
    assert_eq!(h.len(), 3, "{text}");
    // Catch first, finalizer second, both for the block; then the catch clause for the finalizer.
    assert_eq!((h[0].0, h[0].1), (h[1].0, h[1].1), "{h:?}");
    assert_eq!(h[2].0, h[0].2, "the catch clause starts where the first handler goes: {h:?}");
    assert!(h[2].1 <= h[1].2 && h[0].1 <= h[2].0, "{h:?}\n{text}");
    assert_eq!(at("Throw"), 2, "the throw of the source and the rethrow after the finalizer: {text}");
}

#[test]
fn a_root_map_names_the_registers_written_before_the_safepoint() {
    let f = outer("function f(a, b) { var c = g(a); return c + b; }");
    let code = &f.code;
    let mut pc = 0;
    let mut seen = Vec::new();
    while pc < code.words.len() {
        let (ins, len) = crate::bytecode::decode(&code.words, pc).unwrap();
        if let Some(m) = code.roots.find(pc as u32) {
            seen.push((ins.op, m.slots().collect::<Vec<_>>()));
        }
        pc += len;
    }
    // `GetGlobal` of `g` has the two parameters; the call has them and the callee and argument registers; `c` is not written yet.
    assert_eq!(seen[0], (Op::GetGlobal, vec![0, 1]), "{seen:?}");
    let call = seen.iter().find(|(op, _)| *op == Op::CallU).expect("a call");
    assert!(call.1.starts_with(&[0, 1]) && !call.1.contains(&2), "{seen:?}");
    // The `Add` is a safepoint (it can call `valueOf`) and by then `c` is live.
    let add = seen.iter().find(|(op, _)| *op == Op::Add).expect("an add");
    assert!(add.1.contains(&2), "{seen:?}");
}

#[test]
fn big_literals_are_built_one_by_one() {
    let items: Vec<String> = (0..70).map(|i| i.to_string()).collect();
    let f = outer(&format!("function f() {{ return [{}]; }}", items.join(",")));
    let text = f.code.disassemble();
    assert!(text.contains("NewArray") && text.contains("ArrayPush") && !text.contains("NewArrLit"), "{text}");
    let f = outer("function f(k) { return {a: 1, [k]: 2, b: 3}; }");
    let text = f.code.disassemble();
    assert!(text.contains("NewObject") && text.contains("DefElem") && text.contains("DefField"), "{text}");
}

#[test]
fn a_let_that_no_code_reads_early_has_no_hole() {
    let f = outer("function f() { let a = 1; const b = a + 1; return b; }");
    assert!(!f.code.disassemble().contains("LoadHole"));
    let f = outer("function f() { var g = function() { return a; }; let a = 1; return g(); }");
    assert!(f.code.disassemble().contains("PushCtx"));
    let f = outer("function f(c) { if (c) return a; let a = 1; return a; }");
    let text = f.code.disassemble();
    assert!(text.contains("LoadHole") && text.contains("CheckInit"), "{text}");
}

#[test]
fn a_call_with_too_many_arguments_is_refused() {
    let args: Vec<&str> = vec!["1"; 70_000];
    let src = format!("function f() {{ return g({}); }}", args.join(","));
    let atoms = SharedAtoms::default();
    let r = compile_all(&src, &atoms, Limits::default());
    assert!(matches!(r, Err(ref e) if e.contains("Limit")), "{:?}", r.map(|_| ()));
}

#[test]
fn the_script_top_level_var_and_function_are_global_and_let_is_not() {
    let (s, _) = build("var s = 1; let t = 2; function g() {} s + t;");
    let text = s.code.disassemble();
    // `t` stays in a register while the script runs, and the script hands it over to the global lexical environment at the end.
    assert!(text.matches("SetGlobal").count() >= 2 && text.contains("GetGlobal") && !text.contains("GetCtx") && !text.contains("SetCtx"), "{text}");
    assert!(text.contains("CheckLex") && text.contains("DeclLex"), "{text}");
    let (s, _) = build("let t = 2; const u = 3; function g() { return t + u; } g();");
    let text = s.code.disassemble();
    // Only the function `g` is a global: `t` and `u` are slots of the script context.
    assert!(text.contains("PushCtx") && text.matches("SetGlobal").count() == 1, "{text}");
}

/// The `pc` of each instruction of `op` in `code`.
fn pcs_of(code: &Code, op: Op) -> Vec<u32> {
    let mut v = Vec::new();
    let mut pc = 0;
    while pc < code.words.len() {
        let (ins, len) = crate::bytecode::decode(&code.words, pc).unwrap();
        if ins.op == op {
            v.push(pc as u32);
        }
        pc += len;
    }
    v
}

fn covered(code: &Code, pc: u32) -> bool {
    code.handlers.iter().any(|h| h.start <= pc && pc < h.end)
}

/// Is the `IterClose` at `pc` inside a range whose handler closes the same iterator? (A handler of another loop around it is right.)
fn closed_twice(code: &Code, pc: u32) -> bool {
    let rec = crate::bytecode::decode(&code.words, pc as usize).unwrap().0.a;
    code.handlers.iter().any(|h| {
        let t = crate::bytecode::decode(&code.words, h.target as usize).unwrap().0;
        h.start <= pc && pc < h.end && t.op == Op::IterClose && t.a == rec
    })
}

#[test]
fn a_script_var_without_an_initializer_is_declared() {
    let (s, _) = build("var x; var y = 1; function g() {} if (0) { var z; }");
    let text = s.code.disassemble();
    // One declaration for each `var` name (also one in a block) and for the function, before any statement.
    assert_eq!(text.matches("DeclGlobal").count(), 4, "{text}");
    assert!(text.lines().skip(1).take(4).all(|l| l.contains("DeclGlobal")), "{text}");
    // Not in a function.
    let f = outer("function f() { var x; return x; }");
    assert!(!f.code.disassemble().contains("DeclGlobal"));
}

#[test]
fn a_break_out_of_for_of_closes_the_iterator_once() {
    // `return()` can throw. The close that a `break` or `return` makes is not in the range of the loop's own handler: an exception from
    // it would be caught by that handler and the iterator would be closed again.
    let f = outer("function f(o) { for (const v of o) { if (v) break; if (g(v)) return v; } }");
    let code = &f.code;
    let closes = pcs_of(code, Op::IterClose);
    assert_eq!(closes.len(), 3, "break, return, and the handler: {}", code.disassemble());
    assert!(closes.iter().all(|&pc| !covered(code, pc)), "{:?}\n{}", code.handlers, code.disassemble());
    assert!(!code.handlers.is_empty());
    // The same for a labeled `continue` that leaves an inner loop.
    let f = outer("function f(o, p) { a: for (const v of o) { for (const w of p) { if (w) continue a; } } }");
    let code = &f.code;
    let closes = pcs_of(code, Op::IterClose);
    assert_eq!(closes.len(), 3, "the continue and the two handlers: {}", code.disassemble());
    assert!(closes.iter().all(|&pc| !closed_twice(code, pc)), "{:?}\n{}", code.handlers, code.disassemble());
}

#[test]
fn an_array_pattern_closes_the_iterator_when_a_binding_throws() {
    let f = outer("function f(o) { var [a, b = g()] = o; return a + b; }");
    let code = &f.code;
    let text = code.disassemble();
    assert!(!code.handlers.is_empty(), "{text}");
    // A throw from `next()` does not close the iterator; a throw from a default does; the normal close is outside.
    assert!(pcs_of(code, Op::IterNext).iter().all(|&pc| !covered(code, pc)), "{:?}\n{text}", code.handlers);
    let closes = pcs_of(code, Op::IterClose);
    assert_eq!(closes.len(), 2, "{text}");
    assert!(closes.iter().all(|&pc| !covered(code, pc)), "{:?}\n{text}", code.handlers);
    assert!(covered(code, pcs_of(code, Op::GetGlobal)[0]), "the default is covered: {:?}\n{text}", code.handlers);
    // When nothing in the pattern can throw, no handler.
    let f = outer("function f(o) { var [a, b] = o; return a + b; }");
    assert!(f.code.handlers.is_empty(), "{}", f.code.disassemble());
    // A rest element, and an assignment to a member.
    let f = outer("function f(o, p) { [p.x, ...p.y] = o; }");
    assert!(!f.code.handlers.is_empty());
}

#[test]
fn a_scope_with_many_locals_compiles_in_linear_time() {
    let src = |n: usize| {
        let mut src = String::from("function f() {");
        for i in 0..n {
            src.push_str(&format!("var v{i} = {};", i % 9));
        }
        src.push_str("return v0 + v1; }");
        src
    };
    let atoms = SharedAtoms::default();
    // Compare two sizes on the same machine, not a wall-time limit: a loaded machine slows both. Linear work gives a ratio near 10,
    // quadratic work near 100.
    let t = std::time::Instant::now();
    compile_all(&src(6_000), &atoms, Limits::default()).expect("compile");
    let small = t.elapsed();
    let t = std::time::Instant::now();
    let (_, fs) = compile_all(&src(60_000), &atoms, Limits::default()).expect("compile");
    let big = t.elapsed();
    // The verifier keeps a register set for each instruction: too much for 60,000 registers, so it is not run here.
    assert!((60_000..=60_010).contains(&fs[0].1.code.nregs), "{}", fs[0].1.code.nregs);
    // More than the format has registers for: refused, and not after minutes.
    let r = compile_all(&src(70_000), &atoms, Limits::default());
    assert!(matches!(r, Err(ref e) if e.contains("Limit")), "{:?}", r.map(|_| ()));
    assert!(big < small * 40 + std::time::Duration::from_millis(200), "6,000 locals: {small:?}, 60,000 locals: {big:?}");
}

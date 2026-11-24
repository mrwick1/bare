use super::tests::check;
use crate::regexp::{ExecError, FUEL_PER_BLOCK, Scratch, compile, exec};

#[test]
fn suffix_rejection_updates_last_index() {
    check(&[
        (
            "var p = /(a+)(b|c)$/g; p.lastIndex = 1; var r = [p.test('xaaab'), p.lastIndex]; p.lastIndex = 1; r.push(p.test('xaaad'), p.lastIndex); r = r.join(',');",
            "r",
            "true,5,false,0",
        ),
        (
            "var p = /(a+)(b|c)$/y; p.lastIndex = 1; var r = [p.test('xaaac'), p.lastIndex]; p.lastIndex = 1; r.push(p.test('xaaad'), p.lastIndex); r = r.join(',');",
            "r",
            "true,5,false,0",
        ),
        (
            "var p = /b$/g; var n = 0; p.lastIndex = {valueOf() { ++n; return 0; }}; var r = [p.test('x'), p.lastIndex, n].join(',');",
            "r",
            "false,0,1",
        ),
        (
            "var p = /b$/g; Object.freeze(p); var r; try { p.test('a'); } catch (e) { r = e.name; }",
            "r",
            "TypeError",
        ),
    ]);
}

#[test]
fn suffix_rejection_respects_script_exec() {
    check(&[
        (
            "var p = /b$/g; p.exec = function(s) { this.lastIndex = s.length; return {}; }; var r = [p.test('aaa'), p.lastIndex].join(',');",
            "r",
            "true,3",
        ),
        (
            "var p = /b$/g; RegExp.prototype.exec = function(s) { this.lastIndex = s.length; return null; }; var r = [p.test('ab'), p.lastIndex].join(',');",
            "r",
            "false,2",
        ),
    ]);
}

#[test]
fn suffix_rejection_keeps_captures_and_backreferences() {
    check(&[
        (
            r"var p = /(?<pair>.)\1(b|c)$/; var r = [p.test('aab'), p.test('ab'), p.test('λλc'), p.test('λλδ'), p.exec('λλc').groups.pair].join(',');",
            "r",
            "true,false,true,false,λ",
        ),
        (
            r"var r = [/(a|)b?$/.test('x'), /a$|b/.test('bx'), /a$/m.test('a\nx'), /a$/i.test('A'), /\u03bb$/u.test('λ')].join(',');",
            "r",
            "true,true,true,true,true",
        ),
    ]);
}

#[test]
fn rejected_suffix_does_not_spend_backtracking_fuel() {
    let img = compile(&"^(a+)(b|c)$".encode_utf16().collect::<Vec<_>>(), "").unwrap();
    let mut scratch = Scratch::new();
    let mut out = [0; 6];
    let mut fuel = 0;
    let mut input = vec![b'a'; 1024];
    assert_eq!(exec(&img, &input, 0, &mut scratch, &mut fuel, &mut out), Ok(false));

    // A viable suffix still uses the ordinary matcher and its full block charges.
    input.push(b'b');
    let mut fuel = u64::MAX;
    assert_eq!(exec(&img, &input, 0, &mut scratch, &mut fuel, &mut out), Ok(true));
    let cost = u64::MAX - fuel;
    assert!(cost >= FUEL_PER_BLOCK);
    let mut below = cost - 1;
    assert_eq!(exec(&img, &input, 0, &mut scratch, &mut below, &mut out), Err(ExecError::Fuel));
    let mut exact = cost;
    assert_eq!(exec(&img, &input, 0, &mut scratch, &mut exact, &mut out), Ok(true));
    assert_eq!(exact, 0);
    assert_eq!(out, [0, 1025, 0, 1024, 1024, 1025]);
}

#[test]
fn viable_suffix_obeys_step_limit() {
    let img = compile(&"^(a+)+b$".encode_utf16().collect::<Vec<_>>(), "").unwrap();
    let mut input = vec![b'a'; 32];
    input.extend(b"cb");
    let mut out = [0; 4];
    let mut scratch = Scratch::new();
    let mut fuel = u64::MAX;
    assert_eq!(exec(&img, &input, 0, &mut scratch, &mut fuel, &mut out), Err(ExecError::Steps));
}

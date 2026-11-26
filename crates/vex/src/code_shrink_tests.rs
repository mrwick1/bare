// Tests of the exact tables of a code (`docs/internal/design-code-memory.md`, C3): a complete code has no spare capacity.
use crate::atoms::SharedAtoms;

#[test]
fn a_compiled_code_has_no_spare_capacity() {
    let shared = SharedAtoms::default();
    let src = "function f(a, b) { var s = 0; for (var i = 0; i < a; i++) { switch (i % 3) { case 0: s += 1.5; break; case 1: s += b.x; break; default: s += 'k'.length; } } \
               try { g(); } catch (e) { s++; } return [s, {p: 1, q: [2, 3]}, function () { return s; }]; } var r = f(9, {x: 2});";
    let prog = crate::script::compile(src, &shared, Default::default()).unwrap();
    for c in &prog.codes {
        let spare = [
            c.words.capacity() - c.words.len(),
            c.numbers.capacity() - c.numbers.len(),
            c.atoms.capacity() - c.atoms.len(),
            c.funcs.capacity() - c.funcs.len(),
            c.tables.capacity() - c.tables.len(),
            c.sites.capacity() - c.sites.len(),
            c.loops.capacity() - c.loops.len(),
            c.handlers.capacity() - c.handlers.len(),
            c.lines.capacity() - c.lines.len(),
        ];
        assert_eq!(spare, [0; 9], "{} words", c.words.len());
    }
}

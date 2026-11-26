// Tests of the shared stub records (`docs/internal/design-code-memory.md`, C1): a function that never ran has no record of its own,
// and it still gives its own `name`, `length` and source text.
use super::*;

#[test]
fn stubs_of_equal_shape_share_one_record() {
    let mut vm = load("function a(x, y) { return x; } function b(p, q) { return q; } var f = function (u, v) {};").unwrap();
    let prog = &vm.realm.prog;
    let stubs: Vec<usize> = (0..prog.len()).filter(|&i| prog[i].is_stub()).collect();
    assert_eq!(stubs.len(), 3, "a, b and the function expression wait");
    assert!(stubs.iter().all(|&i| std::ptr::eq(&prog[i], &prog[stubs[0]])), "one record for the three");
    assert_eq!(stubs.iter().map(|&i| prog.name_len(i).1).collect::<Vec<_>>(), [2, 2, 2]);
    assert!(vm.run_main(0).is_ok());
}

#[test]
fn a_stub_gives_its_own_name_length_and_text_before_its_first_call() {
    let src = "function alpha(a, b, c) { return a; }\n\
               var beta = function (x) { return x; };\n\
               var arrow = (p, q) => p;\n\
               var r = [alpha.name, alpha.length, beta.name, beta.length, arrow.name, arrow.length, String(alpha)].join('|');\n\
               var e; try { new arrow(1, 2); } catch (x) { e = x instanceof TypeError; }\n\
               var after = alpha(7) + beta(8) + arrow(9);";
    let mut vm = run(src).unwrap();
    assert_eq!(vm.global_text("r"), "alpha|3|beta|1|arrow|2|function alpha(a, b, c) { return a; }");
    assert_eq!(vm.global_text("e"), "true", "`new` on an arrow stub throws a TypeError");
    assert_eq!(vm.global_text("after"), "24", "each stub compiles to its own code");
}

#[test]
fn a_set_on_a_stub_keeps_the_shared_record() {
    use crate::bytecode::encode;
    let stub = |len: u16, span: u32| {
        let mut c = Code::new(len.max(1), len);
        encode(&mut c.words, &Ins::new(Op::Lazy, 0, 0, 0)).unwrap();
        (c.fn_len, c.span) = (len, (span, span + 5));
        c
    };
    let mut codes = Codes::from(vec![stub(2, 0), stub(2, 10), stub(2, 20)]);
    assert!(std::ptr::eq(&codes[0], &codes[2]));
    codes.set(0, Code::new(3, 2));
    codes.free_retired();
    // The other stubs still read the shared record, not an empty code.
    assert_eq!((codes[1].nparams, codes[2].nregs, codes[0].nregs), (2, 2, 3));
    assert_eq!((codes.span(1), codes.span(2), codes.name_len(2).1), (Some((10, 15)), Some((20, 25)), 2));
}

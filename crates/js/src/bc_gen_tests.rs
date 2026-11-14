//! Tests of the code generator and the verifier: golden disassembly (`tools/fixtures/js/bc-golden.txt`; `BC_BLESS=1` writes it
//! again), the coverage of the corpora, random programs of the subset, and code that the verifier must reject.

use super::super::bc_verify::verify;
use super::*;
use crate::bc::encode;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};
use std::fmt::Write;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(256 << 20).spawn(f).unwrap().join().unwrap()
}

/// Every function of the program, in source order: its number and name, then its code or the reason it does not compile.
fn listing(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{e:?}: {src}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    let _ = it.run();
    it.bc.rf_off = true;
    let root = it.bc_root().unwrap();
    let mut out = String::new();
    let mut todo = it.bc_walk(None, root).funcs;
    todo.reverse();
    while let Some((f, s)) = todo.pop() {
        let mut inner = it.bc_walk(Some(f), s).funcs;
        inner.reverse();
        todo.extend(inner);
        let name = it.names[it.func_name[f.index()] as usize].clone();
        let _ = writeln!(out, "fn{} {}", f.index(), if name.is_empty() { "(anonymous)" } else { &name });
        match it.bc_compile(f) {
            Ok(c) => {
                verify(&c).unwrap_or_else(|e| panic!("{e}\n{}", c.disassemble()));
                out += &with_names(&it, &c.disassemble());
            }
            Err(e) => {
                let _ = writeln!(out, "unsupported: {e}");
            }
        }
    }
    out
}

/// `name#12` in a listing becomes `name:foo`, so the text does not depend on the order of the interned names.
fn with_names(it: &Interp, s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("name#") {
        out += &rest[..i];
        let r = &rest[i + 5..];
        let d = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        out += &format!("name:{}", it.names[r[..d].parse::<usize>().unwrap()]);
        rest = &r[d..];
    }
    out + rest
}

#[test]
fn golden_disassembly() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/js/bc-golden.txt");
    let text = std::fs::read_to_string(path).unwrap();
    // Cases: `=== name`, the source, `---`, the listing.
    let mut cases: Vec<(String, String, String)> = Vec::new();
    let mut in_src = false;
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            cases.push((n.to_string(), String::new(), String::new()));
            in_src = true;
        } else if line == "---" {
            in_src = false;
        } else if let Some(c) = cases.last_mut() {
            let s = if in_src { &mut c.1 } else { &mut c.2 };
            *s += line;
            *s += "\n";
        }
    }
    assert!(cases.len() >= 60, "{} programs", cases.len());
    let got = big(move || cases.into_iter().map(|(n, src, want)| { let l = listing(&src); (n, src, want, l) }).collect::<Vec<_>>());
    if std::env::var_os("BC_BLESS").is_some() {
        let mut t = String::new();
        for (n, src, _, l) in &got {
            let _ = write!(t, "=== {n}\n{src}---\n{l}");
        }
        std::fs::write(path, t).unwrap();
        return;
    }
    let bad: Vec<String> = got.iter().filter(|g| g.2 != g.3).map(|g| format!("=== {}\n{}---\nwant:\n{}got:\n{}", g.0, g.1, g.2, g.3)).collect();
    assert!(bad.is_empty(), "{} of {} listings differ (BC_BLESS=1 writes them):\n{}", bad.len(), got.len(), bad.join("\n"));
}

fn add(a: &mut GenCheck, b: GenCheck) {
    a.functions += b.functions;
    a.compiled += b.compiled;
    a.words += b.words;
    a.bad.extend(b.bad);
    for (w, n) in b.unsupported {
        match a.unsupported.iter_mut().find(|x| x.0 == w) {
            Some(x) => x.1 += n,
            None => a.unsupported.push((w, n)),
        }
    }
}

/// The share of the functions of the corpora that compile (the coverage number of the plan), and the verifier on each of them.
#[test]
fn every_function_of_the_corpora_compiles_or_says_why() {
    let r = big(|| {
        let mut total = GenCheck::default();
        for text in [
            include_str!("../../../tools/fixtures/js/eval-cases.txt"),
            include_str!("../../../tools/fixtures/js/statements-cases.txt"),
            include_str!("../../../tools/fixtures/js/class-cases.txt"),
            include_str!("../../../tools/fixtures/js/subclass-cases.txt"),
            include_str!("../../../tools/fixtures/js/generator-cases.txt"),
            include_str!("../../../tools/fixtures/js/iterator-cases.txt"),
            include_str!("../../../tools/fixtures/js/async-cases.txt"),
            include_str!("../../../tools/fixtures/js/tagged-cases.txt"),
            include_str!("../../../tools/fixtures/js/arrobj-cases.txt"),
        ] {
            let mut src = String::new();
            for line in text.lines().chain(["=== end"]) {
                if line.starts_with("=== ") {
                    if let Ok(prog) = parse_script(&src, Limits::default())
                        && let Ok(mut it) = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 300_000, ..Default::default() })
                    {
                        let _ = it.run();
                        let _ = it.drain_microtasks();
                        add(&mut total, it.bc_gen_check());
                    }
                    src.clear();
                } else {
                    src += line;
                    src += "\n";
                }
            }
        }
        total
    });
    total_report(&r);
    assert!(r.bad.is_empty(), "{} functions fail the verifier:\n{}", r.bad.len(), r.bad.join("\n"));
    assert!(r.functions > 2000 && r.compiled * 2 > r.functions, "coverage too low: {r:?}");
}

fn total_report(r: &GenCheck) {
    let mut u = r.unsupported.clone();
    u.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("{} functions, {} compile ({:.1}%), {} words; unsupported: {u:?}", r.functions, r.compiled, 100.0 * r.compiled as f64 / r.functions.max(1) as f64, r.words);
}

/// A small random generator (xorshift), so the test needs no crate and gives the same programs each run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

/// Random code of the subset. `lets` counts the `let` and `const` names, so each is new.
struct Prog {
    r: Rng,
    lets: usize,
}

const NAMES: &[&str] = &["a", "b", "v0", "v1", "v2", "o", "g", "C"];
const TARGETS: &[&str] = &["a", "b", "v0", "v1", "v2"];

impl Prog {
    fn expr(&mut self, d: usize) -> String {
        if d == 0 {
            return match self.r.below(6) {
                0 => format!("{}", self.r.below(100_000)),
                1 => "'s'".into(),
                2 => self.r.pick(&["this", "true", "null", "1.5"]).into(),
                _ => self.r.pick(NAMES).into(),
            };
        }
        let d = d - 1;
        match self.r.below(17) {
            0 => format!("({} {} {})", self.expr(d), self.r.pick(&["+", "-", "*", "/", "%", "**", "==", "!==", "<", ">=", "<<", ">>>", "&", "|", "^", "in", "instanceof"]), self.expr(d)),
            1 => format!("({} {} {})", self.expr(d), self.r.pick(&["&&", "||", "??"]), self.expr(d)),
            2 => format!("({} ? {} : {})", self.expr(d), self.expr(d), self.expr(d)),
            3 => format!("({} {})", self.r.pick(&["-", "+", "!", "~", "typeof", "void"]), self.expr(d)),
            4 => {
                let t = match self.r.below(3) {
                    0 => self.r.pick(TARGETS).to_string(),
                    1 => format!("o.{}", self.r.pick(&["p", "q"])),
                    _ => format!("o[{}]", self.expr(d)),
                };
                format!("({t} {} {})", self.r.pick(&["=", "+=", "-=", "||=", "&&=", "??="]), self.expr(d))
            }
            5 => match self.r.below(3) {
                0 => format!("{}{}", self.r.pick(TARGETS), self.r.pick(&["++", "--"])),
                1 => format!("{}o.p", self.r.pick(&["++", "--"])),
                _ => format!("o[{}]++", self.expr(d)),
            },
            6 => match self.r.below(4) {
                0 => format!("g({}, {})", self.expr(d), self.expr(d)),
                1 => format!("o.m({})", self.expr(d)),
                2 => format!("o[{}]()", self.expr(d)),
                _ => format!("(o.m)({})", self.expr(d)),
            },
            7 => format!("new C({})", self.expr(d)),
            8 => format!("o.{}", self.r.pick(&["p", "length"])),
            9 => format!("o[{}]", self.expr(d)),
            10 => format!("{{ a: {}, 'b c': {}, 7: {}, g, m() {{ return {} }}, f: function () {{}} }}", self.expr(d), self.expr(d), self.expr(d), self.expr(d)),
            11 => format!("[{}, {}]", self.expr(d), self.expr(d)),
            12 => match self.r.below(3) {
                0 => format!("(function (x) {{ {} }})", self.list(d, false, false)),
                1 => format!("((x) => ({}))", self.expr(d)),
                _ => format!("(() => {{ {} }})", self.list(d, false, false)),
            },
            13 => format!("`t${{{}}}u${{{}}}`", self.expr(d), self.expr(d)),
            14 => format!("({}, {})", self.expr(d), self.expr(d)),
            _ => self.expr(d),
        }
    }

    /// A list of statements, where declarations are allowed.
    fn list(&mut self, d: usize, lp: bool, sw: bool) -> String {
        let n = self.r.below(4);
        (0..n).map(|_| self.stmt(d, lp, sw, true)).collect::<Vec<_>>().join(" ")
    }

    /// One statement. `lp`: inside a loop. `sw`: inside a loop or a switch. `decl`: a declaration may stand here.
    fn stmt(&mut self, d: usize, lp: bool, sw: bool, decl: bool) -> String {
        let e = self.r.below(3);
        if d == 0 {
            return format!("({});", self.expr(e));
        }
        let d = d - 1;
        match self.r.below(19) {
            0 if decl => {
                self.lets += 1;
                let n = self.lets;
                format!("{} l{n} = {};", self.r.pick(&["let", "const"]), self.expr(e))
            }
            1 if decl => {
                self.lets += 1;
                let n = self.lets;
                format!("function h{n}(x) {{ {} }}", self.list(d, false, false))
            }
            2 => format!("if ({}) {} else {}", self.expr(e), self.stmt(d, lp, sw, false), self.stmt(d, lp, sw, false)),
            3 => format!("while ({}) {}", self.expr(e), self.stmt(d, true, true, false)),
            4 => format!("do {{ {} }} while ({});", self.list(d, true, true), self.expr(e)),
            5 => {
                self.lets += 1;
                let i = format!("i{}", self.lets);
                format!("for ({} {i} = 0; {i} < {}; {i}++) {{ {} v0 = () => {i}; }}", self.r.pick(&["let", "var"]), self.expr(e), self.list(d, true, true))
            }
            6 => format!("for (;;) {{ {} break; }}", self.list(d, true, true)),
            7 if sw => "break;".into(),
            8 if lp => "continue;".into(),
            9 => format!("return {};", self.expr(e)),
            10 => format!("throw {};", self.expr(e)),
            11 => match self.r.below(3) {
                0 => format!("try {{ {} }} catch (e) {{ {} }}", self.list(d, lp, sw), self.list(d, lp, sw)),
                1 => format!("try {{ {} }} finally {{ {} }}", self.list(d, lp, sw), self.list(d, lp, sw)),
                _ => format!("try {{ {} }} catch {{ {} }} finally {{ {} }}", self.list(d, lp, sw), self.list(d, lp, sw), self.list(d, lp, sw)),
            },
            12 => format!("switch ({}) {{ case {}: {} case 2: {} default: {} }}", self.expr(e), self.expr(e), self.list(d, lp, true), self.list(d, lp, true), self.list(d, lp, true)),
            13 => format!("{{ {} }}", self.list(d, lp, sw)),
            14 => format!("{} = {};", self.r.pick(TARGETS), self.expr(e)),
            _ => format!("({});", self.expr(e)),
        }
    }
}

/// Random programs of the subset: every function compiles and passes the verifier.
#[test]
fn random_programs_of_the_subset_compile_and_verify() {
    big(|| {
        let mut p = Prog { r: Rng(0x9e37_79b9_7f4a_7c15), lets: 0 };
        let (mut funcs, mut words) = (0, 0);
        for _ in 0..1500 {
            let body = p.list(4, false, false);
            let src = format!("'use strict'; function f(a, b) {{ var v0, v1, v2; {body} }}");
            let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| panic!("{e:?}: {src}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
            let root = it.bc_root().unwrap();
            let mut todo = it.bc_walk(None, root).funcs;
            while let Some((f, s)) = todo.pop() {
                todo.extend(it.bc_walk(Some(f), s).funcs);
                let c = it.bc_compile(f).unwrap_or_else(|e| panic!("{e}: {src}"));
                verify(&c).unwrap_or_else(|e| panic!("{e}: {src}\n{}", c.disassemble()));
                assert!(!c.disassemble().contains('<'), "{src}");
                funcs += 1;
                words += c.ops.len();
            }
        }
        eprintln!("{funcs} functions, {words} words");
        assert!(funcs > 3000);
    });
}

/// Code with one fault each: the verifier names it.
#[test]
fn the_verifier_rejects_broken_code() {
    let code = |nregs: u16, ins: &[Ins]| {
        let mut c = Code::new(nregs);
        for i in ins {
            encode(&mut c.ops, i).unwrap();
        }
        c
    };
    let i = Ins::new;
    let ok = code(2, &[i(Op::LoadI, 0, 1, 0), i(Op::PushEnv, 0, 3, 0), i(Op::JmpIf, 0, 1, 0), i(Op::PopEnv, 0, 0, 0), i(Op::PopEnv, 0, 0, 0), i(Op::Ret, 1, 0, 0)]);
    // The two paths meet at depth 1 and 0: a fault, which shows that the check runs.
    assert!(verify(&ok).unwrap_err().contains("depth"));
    let good = code(2, &[i(Op::LoadI, 0, 1, 0), i(Op::PushEnv, 0, 3, 0), i(Op::JmpIf, 0, 1, 0), i(Op::LoadI, 1, 2, 0), i(Op::PopEnv, 0, 0, 0), i(Op::Ret, 1, 0, 0)]);
    assert_eq!(verify(&good), Ok(()));
    let cases: Vec<(Code, &str)> = vec![
        (code(1, &[i(Op::PopEnv, 0, 0, 0), i(Op::RetUndef, 0, 0, 0)]), "depth 0"),
        (code(1, &[i(Op::LoadI, 0, 1, 0)]), "past the end"),
        (code(1, &[i(Op::Jmp, 5, 0, 0), i(Op::RetUndef, 0, 0, 0)]), "jump target"),
        (code(1, &[i(Op::Mov, 0, 1, 0), i(Op::RetUndef, 0, 0, 0)]), "register"),
        (code(4, &[i(Op::Call, 0, 1, 2), i(Op::RetUndef, 0, 0, 0)]), "argument registers"),
        (code(1, &[i(Op::LoadK, 0, 0, 0), i(Op::RetUndef, 0, 0, 0)]), "constant"),
        (code(1, &[i(Op::Ret, 0, 7, 0)]), "not used"),
    ];
    for (c, want) in cases {
        let e = verify(&c).unwrap_err();
        assert!(e.contains(want), "{e} does not say {want}");
    }
    // A jump into the second word of a wide instruction.
    let mut c = code(300, &[i(Op::Jmp, 1, 0, 0), i(Op::LoadI, 299, 1, 0), i(Op::RetUndef, 0, 0, 0)]);
    assert!(verify(&c).unwrap_err().contains("jump target"));
    // A name operand that is a number, an unused inline cache, a handler past the end.
    c = code(1, &[i(Op::GetGlobal, 0, 0, 0), i(Op::RetUndef, 0, 0, 0)]);
    c.consts.push(Const::Number(1.0));
    assert!(verify(&c).unwrap_err().contains("not a name"));
    c.consts[0] = Const::Name(1);
    assert_eq!(verify(&c), Ok(()));
    c.ics = vec![Cell::new(Ic::EMPTY)].into();
    assert!(verify(&c).unwrap_err().contains("inline cache"));
    c.ics = Box::new([]);
    c.handlers.push(Handler { start: 0, end: 9, target: 1, reg: 0, depth: 0 });
    assert!(verify(&c).unwrap_err().contains("handler"));
    c.handlers[0] = Handler { start: 0, end: 1, target: 1, reg: 0, depth: 1 };
    assert!(verify(&c).unwrap_err().contains("depth"));
}

/// A small function with a loop and a call is a register function: no environment, fused jumps, `CallR`.
#[test]
fn a_simple_function_is_a_register_function() {
    big(|| {
        let src = "function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); } fib(3);";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let _ = it.run();
        let root = it.bc_root().unwrap();
        let (f, _) = it.bc_walk(None, root).funcs[0];
        let c = it.bc_compile(f).unwrap();
        verify(&c).unwrap_or_else(|e| panic!("{e}\n{}", c.disassemble()));
        let text = c.disassemble();
        assert!(c.regfn, "{text}");
        for want in ["CallR", "JnLtK", "SubK"] {
            assert!(text.contains(want), "{want}\n{text}");
        }
        assert!(!text.contains("Env"), "{text}");
    });
}

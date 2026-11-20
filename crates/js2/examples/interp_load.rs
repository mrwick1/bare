//! Micro loops of the interpreter (plan step S2.6), shaped like the hot rows of `crates/js/examples/eval_load.rs` (`loopfn`, `calls`, `fib`,
//! `closures`, `objects`), written as bytecode by hand (there is no compiler yet). Each case checks its result against the same sum in
//! Rust and exits with status 1 on a mismatch. It prints the number of ops that the run executed (counted from the program: the ops of
//! one loop turn times the turns, plus the entry and exit), so `instructions / ops` is the cost of one op.
//!   interp_load CASE N [--source]      CASE: loopfn | calls | fib | closures | objects      N: turns (fib: the argument)
//! `--source` prints the expected result and the JavaScript that the old engine runs for the same case (one function, locals only), for
//! `tools/js/instr.sh` on both sides. Count with `tools/js/instr.sh`; run N = 0 for the setup, and subtract it.
#![allow(dead_code)]
mod prelude {
    pub use bare_js2::atoms::SharedAtoms;
    pub use bare_js2::bytecode::{Asm, Code, Handler, Ins, Label, Op, Recipes, decode};
    pub use bare_js2::roots::RootMaps;
    pub use bare_js2::string::{Chars, Str};
    pub use bare_js2::verify::verify;
    pub use std::collections::BTreeSet;
    pub use std::sync::Arc;
}
#[path = "../src/asm_util.rs"]
mod asm_util;
use asm_util::F;
use bare_js2::interp::{Program, Vm};
use bare_js2::value::Value;
use prelude::*;
use std::time::Instant;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

/// The program, the expected result, the ops that it runs, the JavaScript of the old engine.
struct Case {
    codes: Vec<Code>,
    expect: f64,
    ops: u64,
    js: String,
}

fn loopfn(sh: &Arc<SharedAtoms>, n: i32) -> Case {
    let mut f = F::new(sh, 5, 0);
    let (head, done) = (f.label(), f.label());
    f.li(0, 0);
    f.li(1, 0);
    f.bind(head);
    f.ji(Op::JnLtI, 1, n, done);
    f.e(Op::MulI, 3, 1, 3);
    f.e(Op::Add, 3, 0, 3);
    f.li(4, 1000003);
    f.e(Op::Mod, 0, 3, 4);
    f.e(Op::Inc, 1, 1, 0);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.e(Op::Ret, 0, 0, 0);
    let mut s = 0u64;
    for i in 0..n as u64 {
        s = (s + i * 3) % 1000003;
    }
    Case {
        codes: vec![f.build()],
        expect: s as f64,
        ops: 2 + 7 * n as u64 + 2,
        js: format!("function f() {{ var s = 0; for (var i = 0; i < {n}; i++) {{ s = (s + i * 3) % 1000003; }} return s; }} f()"),
    }
}

fn calls(sh: &Arc<SharedAtoms>, n: i32) -> Case {
    let mut m = F::new(sh, 6, 0);
    let (head, done) = (m.label(), m.label());
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, 1);
    m.li(2, 0);
    m.bind(head);
    m.ji(Op::JnLtI, 2, n, done);
    m.e(Op::Mov, 3, 0, 0);
    m.e(Op::Mov, 4, 1, 0);
    m.e(Op::Mov, 5, 2, 0);
    m.e(Op::CallU, 1, 3, 2);
    m.e(Op::Inc, 2, 2, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::Ret, 1, 0, 0);
    let mut g = F::new(sh, 4, 2);
    g.e(Op::MulI, 2, 0, 31);
    g.e(Op::Add, 2, 2, 1);
    g.li(3, 65521);
    g.e(Op::Mod, 2, 2, 3);
    g.e(Op::Ret, 2, 0, 0);
    let mut s = 1u64;
    for i in 0..n as u64 {
        s = (s * 31 + i) % 65521;
    }
    Case {
        codes: vec![m.build(), g.build()],
        expect: s as f64,
        ops: 4 + (7 + 5) * n as u64 + 2,
        js: format!("function f(a, b) {{ return (a * 31 + b) % 65521; }} function g() {{ var s = 1; for (var i = 0; i < {n}; i++) s = f(s, i); return s; }} g()"),
    }
}

fn fib_of(n: u64) -> u64 {
    if n < 2 { n } else { fib_of(n - 1) + fib_of(n - 2) }
}

fn fib(sh: &Arc<SharedAtoms>, k: i32) -> Case {
    let mut m = F::new(sh, 4, 0);
    let site = m.site("fib");
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::SetGlobal, 0, site, 0);
    m.e(Op::LoadUndef, 1, 0, 0);
    m.li(2, k);
    m.e(Op::Call, 3, 0, 1);
    m.e(Op::Ret, 3, 0, 0);
    let mut f = F::new(sh, 6, 1);
    let base = f.label();
    f.ji(Op::JLtI, 0, 2, base);
    for d in [1, 2] {
        let s = f.site("fib");
        f.e(Op::GetGlobal, 1, s, 0);
        f.e(Op::LoadUndef, 2, 0, 0);
        f.e(Op::SubI, 3, 0, d);
        f.e(Op::Call, if d == 1 { 4 } else { 5 }, 1, 1);
    }
    f.e(Op::Add, 4, 4, 5);
    f.e(Op::Ret, 4, 0, 0);
    f.bind(base);
    f.e(Op::Ret, 0, 0, 0);
    let leaves = fib_of(k as u64 + 1);
    Case {
        codes: vec![m.build(), f.build()],
        expect: fib_of(k as u64) as f64,
        ops: 6 + leaves * 2 + (leaves - 1) * 11,
        js: format!("function fib(n) {{ return n < 2 ? n : fib(n - 1) + fib(n - 2); }} fib({k})"),
    }
}

fn closures(sh: &Arc<SharedAtoms>, n: i32) -> Case {
    let mut m = F::new(sh, 10, 0);
    let (h1, d1, h2, d2) = (m.label(), m.label(), m.label(), m.label());
    m.funcs = vec![1];
    m.e(Op::NewArray, 0, 0, 0);
    m.e(Op::Closure, 2, 0, 0);
    m.li(1, 0);
    m.bind(h1);
    m.ji(Op::JnLtI, 1, n, d1);
    m.e(Op::Mov, 3, 2, 0);
    m.e(Op::Mov, 4, 1, 0);
    m.e(Op::CallU, 5, 3, 1);
    m.e(Op::ArrayPush, 0, 5, 0);
    m.e(Op::Inc, 1, 1, 0);
    m.j(Op::JmpBack, 0, 0, h1);
    m.bind(d1);
    m.li(6, 0);
    m.li(7, 0);
    m.bind(h2);
    m.ji(Op::JnLtI, 6, n, d2);
    m.e(Op::GetElem, 8, 0, 6);
    m.e(Op::CallU, 9, 8, 0);
    m.e(Op::Add, 7, 7, 9);
    m.e(Op::Inc, 6, 6, 0);
    m.j(Op::JmpBack, 0, 0, h2);
    m.bind(d2);
    m.e(Op::Ret, 7, 0, 0);
    let mut mk = F::new(sh, 2, 1);
    mk.funcs = vec![2];
    mk.e(Op::PushCtx, 0, 1, 0);
    mk.e(Op::InitCtx, 0, 0, 0);
    mk.e(Op::Closure, 1, 0, 0);
    mk.e(Op::PopCtx, 0, 0, 0);
    mk.e(Op::Ret, 1, 0, 0);
    let mut c = F::new(sh, 2, 0);
    c.e(Op::GetCtx, 0, 0, 0);
    c.e(Op::MulI, 0, 0, 7);
    c.e(Op::AddI, 0, 0, 1);
    c.li(1, 10007);
    c.e(Op::Mod, 0, 0, 1);
    c.e(Op::SetCtx, 0, 0, 0);
    c.e(Op::Ret, 0, 0, 0);
    let s: u64 = (0..n as u64).map(|j| (j * 7 + 1) % 10007).sum();
    Case {
        codes: vec![m.build(), mk.build(), c.build()],
        expect: s as f64,
        ops: 5 + (7 + 5) * n as u64 + 6 * n as u64 + 7 * n as u64 + 3,
        js: format!(
            "function mk(n) {{ var c = n; return function() {{ c = (c * 7 + 1) % 10007; return c; }}; }} function g() {{ var fs = []; for (var i = 0; i < {n}; i++) fs.push(mk(i)); var s = 0; for (var j = 0; j < {n}; j++) s += fs[j](); return s; }} g()"
        ),
    }
}

fn objects(sh: &Arc<SharedAtoms>, n: i32) -> Case {
    let mut f = F::new(sh, 8, 0);
    let (ka, kb) = (f.atom("a"), f.atom("b"));
    f.recipes.push(&[ka, kb]);
    let (head, done) = (f.label(), f.label());
    f.li(0, 0);
    f.li(1, 0);
    f.bind(head);
    f.ji(Op::JnLtI, 1, n, done);
    f.e(Op::Mov, 4, 1, 0);
    f.li(5, 1);
    f.e(Op::NewObjLit, 3, 4, 0);
    f.get(6, 3, "a");
    f.get(7, 3, "b");
    f.e(Op::Add, 6, 6, 7);
    f.set(3, 6, "c");
    f.get(7, 3, "c");
    f.e(Op::Add, 0, 0, 7);
    f.e(Op::Inc, 1, 1, 0);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.e(Op::Ret, 0, 0, 0);
    let s: u64 = (0..n as u64).map(|i| i + 1).sum();
    Case {
        codes: vec![f.build()],
        expect: s as f64,
        ops: 2 + 12 * n as u64 + 2,
        js: format!("function g() {{ var s = 0; for (var i = 0; i < {n}; i++) {{ var o = {{ a: i, b: 1 }}; o.c = o.a + o.b; s += o.c; }} return s; }} g()"),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (Some(name), Some(n)) = (a.get(1), a.get(2).and_then(|s| s.parse::<i32>().ok())) else { die("usage: interp_load CASE N [--source]".into()) };
    let sh = Arc::new(SharedAtoms::default());
    let c = match name.as_str() {
        "loopfn" => loopfn(&sh, n),
        "calls" => calls(&sh, n),
        "fib" => fib(&sh, n),
        "closures" => closures(&sh, n),
        "objects" => objects(&sh, n),
        _ => die(format!("unknown case {name}")),
    };
    if a.get(3).is_some_and(|s| s == "--source") {
        println!("{}\n{}", c.expect, c.js);
        return;
    }
    let t0 = Instant::now();
    let mut vm = Vm::new(Program { codes: c.codes }, sh);
    let r = vm.run_main(0).unwrap_or_else(|e| die(format!("run: {e:?}")));
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    let got = r.as_f64().unwrap_or_else(|| die(format!("result {r:?}")));
    if got != c.expect {
        die(format!("{name} {n}: result {got} != expected {}", c.expect));
    }
    let _ = Value::UNDEFINED;
    println!("{name} n={n} result=ok ops={} collections={} wall={ms:.1}ms (setup included; wall time is not a measure here)", c.ops, vm.realm.collections);
}

//! Cost of a suspended frame (`docs/design-js2-async.md` 5.1 and G0), written as bytecode by hand (there is no compiler for `function*`
//! yet). A generator keeps 5 live registers across each `yield`. Each case checks its result and exits with status 1 on a mismatch.
//!   gen_load CASE N        CASE: next | call | host        N: turns
//! - `next`: a loop that calls `g.next()` and reads `.value` (one native call, one resume, one `Yield`, one result object).
//! - `call`: the same loop with a plain function in place of the generator (the cost of a call and a return, to subtract).
//! - `host`: a Rust loop that resumes the generator through `Vm::resume`, with no result object (one `Yield` and one resume).
//! Count with `tools/js/instr.sh`; run N = 0 for the setup, and subtract it.
#![allow(dead_code)]
mod prelude {
    pub use bare_js2::atoms::SharedAtoms;
    pub use bare_js2::bytecode::{Asm, Code, GK_GEN, Handler, Ins, Label, NO_REG, Op, Recipes, decode};
    pub use bare_js2::roots::RootMaps;
    pub use bare_js2::string::{Chars, Str};
    pub use bare_js2::verify::verify;
    pub use std::collections::BTreeSet;
    pub use std::sync::Arc;
}
#[path = "../src/asm_util.rs"]
mod asm_util;
use asm_util::F;
use bare_js2::generator::MODE_NEXT;
use bare_js2::interp::{Program, Vm};
use bare_js2::value::Value;
use prelude::*;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

/// The generator: `function* (n) { for (var i = 0; i < n; i++) yield i; }`, with three more registers that stay live.
fn generator(sh: &Arc<SharedAtoms>) -> Code {
    let mut g = F::new(sh, 10, 1);
    let (head, cont, done) = (g.label(), g.label(), g.label());
    g.gen_init(&[0]);
    g.li(1, 0);
    g.li(2, 1);
    g.li(3, 2);
    g.li(4, 3);
    g.bind(head);
    g.j(Op::JGe, 1, 0, done);
    g.yield_(1, 5, 6, &[0, 1, 2, 3, 4]);
    g.ji(Op::JSEqI, 6, 0, cont);
    g.e(Op::Throw, 5, 0, 0);
    g.bind(cont);
    g.e(Op::Inc, 1, 1, 0);
    g.j(Op::JmpBack, 0, 0, head);
    g.bind(done);
    g.e(Op::LoadUndef, 7, 0, 0);
    g.e(Op::RetGen, 7, 0, 0);
    g.build()
}

/// `function (n) { return n; }`
fn plain(sh: &Arc<SharedAtoms>) -> Code {
    let mut h = F::new(sh, 4, 1);
    h.e(Op::Ret, 0, 0, 0);
    h.build()
}

/// The main function that makes the generator, then loops `n` times over `g.next().value`.
fn main_next(sh: &Arc<SharedAtoms>, n: i32) -> Code {
    let mut m = F::new(sh, 12, 0);
    let (head, done) = (m.label(), m.label());
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::Mov, 3, 0, 0);
    m.li(4, n);
    m.e(Op::CallU, 5, 3, 1);
    m.li(6, 0);
    m.li(7, 0);
    m.get(2, 5, "next"); // read once: a miss on the prototype chain is the cost of a property read, not of a generator
    m.bind(head);
    m.ji(Op::JnLtI, 7, n, done);
    m.e(Op::Mov, 8, 2, 0);
    m.e(Op::Mov, 9, 5, 0);
    m.e(Op::Call, 10, 8, 0);
    m.get(11, 10, "value");
    m.e(Op::Add, 6, 6, 11);
    m.e(Op::Inc, 7, 7, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::Ret, 6, 0, 0);
    m.build()
}

/// The same loop over a plain call: `s += h(i)`.
fn main_call(sh: &Arc<SharedAtoms>, n: i32) -> Code {
    let mut m = F::new(sh, 12, 0);
    let (head, done) = (m.label(), m.label());
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(6, 0);
    m.li(7, 0);
    m.bind(head);
    m.ji(Op::JnLtI, 7, n, done);
    m.e(Op::Mov, 8, 0, 0);
    m.e(Op::Mov, 9, 7, 0);
    m.e(Op::CallU, 10, 8, 1);
    m.e(Op::Add, 6, 6, 10);
    m.e(Op::Inc, 7, 7, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::Ret, 6, 0, 0);
    m.build()
}

/// Makes the generator and returns it.
fn main_make(sh: &Arc<SharedAtoms>, n: i32) -> Code {
    let mut m = F::new(sh, 6, 0);
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, n);
    m.e(Op::CallU, 2, 0, 1);
    m.e(Op::Ret, 2, 0, 0);
    m.build()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (Some(name), Some(n)) = (a.get(1), a.get(2).and_then(|s| s.parse::<i32>().ok())) else { die("usage: gen_load CASE N".into()) };
    let sh = Arc::new(SharedAtoms::default());
    let expect: f64 = (0..n as u64).sum::<u64>() as f64;
    match name.as_str() {
        "next" | "call" => {
            let codes = if name == "next" { vec![main_next(&sh, n), generator(&sh)] } else { vec![main_call(&sh, n), plain(&sh)] };
            let mut vm = Vm::new(Program { codes }, sh);
            let r = vm.run_main(0).unwrap_or_else(|e| die(format!("run: {e:?}")));
            let got = r.as_f64().unwrap_or_else(|| die(format!("result {r:?}")));
            if got != expect {
                die(format!("{name} {n}: result {got} != expected {expect}"));
            }
            println!("{name} n={n} result=ok collections={}", vm.realm.collections);
        }
        "host" => {
            let mut vm = Vm::new(Program { codes: vec![main_make(&sh, n), generator(&sh)] }, sh);
            let g = vm.run_main(0).unwrap_or_else(|e| die(format!("run: {e:?}")));
            let mut s = 0.0;
            for _ in 0..n {
                let (v, done) = vm.resume(g, MODE_NEXT, Value::UNDEFINED).unwrap_or_else(|e| die(format!("resume: {e:?}")));
                if done {
                    die("the generator ended early".into());
                }
                s += v.as_f64().unwrap_or_else(|| die(format!("value {v:?}")));
            }
            if s != expect {
                die(format!("host {n}: result {s} != expected {expect}"));
            }
            println!("host n={n} result=ok collections={}", vm.realm.collections);
        }
        _ => die(format!("unknown case {name}")),
    }
}

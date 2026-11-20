//! Instructions of one op, measured on its own (plan step S2.9). A loop of N turns holds K copies of the op; the cost of the op is
//! (instructions with K copies - instructions with K = 0) / (N * K), by `tools/js/instr.sh` (or `perf stat -e instructions:u`).
//!   op_cost CASE K N        the program prints `ok`; exit status 1 when the result is wrong
//!   op_cost list            the names of the cases
//! The cost includes the dispatch of the op (the load of the word, the jump) and what its arm does on the fast path. It does not
//! include the loop (the K = 0 run holds `JnLtI`, `Inc` and `JmpBack` only, and is subtracted).
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
use bare_js2::interp::{Program, Vm};
use prelude::*;

const CASES: &[&str] = &[
    "nop", "mov", "loadi", "loadi_wide", "loadundef", "addi", "add", "add_dbl", "muli", "mod", "modi", "inc", "jlti_no", "jlti_yes", "jsne_null_no", "jmpifnot_no",
    "jmp", "getfield", "setfield", "getglobal", "setglobal", "getctx", "setctx", "getelemi", "getelem", "lt", "jgt_no", "lt_jmpifnot", "getglobal_jgt", "getfield_getfield", "mov_mov", "inc_jmpback_pair", "newobjlit", "call0", "call2", "callu0", "closure", "newobj", "strictneq",
];

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).is_some_and(|s| s == "list") {
        println!("{}", CASES.join(" "));
        return;
    }
    let (name, k, n) = (a[1].as_str(), a[2].parse::<usize>().unwrap(), a[3].parse::<i32>().unwrap());
    let sh = Arc::new(SharedAtoms::default());
    let mut f = F::new(&sh, 16, 0);
    f.numbers = vec![1.5];
    let (head, done) = (f.label(), f.label());
    let ctx = matches!(name, "getctx" | "setctx");
    let mut codes = vec![];
    f.li(0, 3);
    f.li(1, 0);
    f.li(3, 7);
    f.li(12, 1);
    f.e(Op::LoadN, 6, 0, 0);
    let ka = f.atom("a");
    f.recipes.push(&[ka]);
    f.e(Op::NewObjLit, 4, 12, 0);
    f.e(Op::NewArray, 5, 0, 0);
    f.e(Op::ArrayPush, 5, 0, 0);
    f.e(Op::ArrayPush, 5, 0, 0);
    f.li(2, 0);
    if name.starts_with("call") || name == "callu0" {
        let mut g = F::new(&sh, 3, if name == "call2" { 2 } else { 0 });
        g.e(if name == "call2" { Op::Ret } else { Op::RetUndef }, 0, 0, 0);
        codes.push(g);
        f.funcs = vec![1];
        f.e(Op::Closure, 8, 0, 0);
        f.e(Op::LoadUndef, 9, 0, 0);
        f.li(10, 1);
        f.li(11, 2);
    }
    if name == "closure" {
        let mut g = F::new(&sh, 1, 0);
        g.e(Op::RetUndef, 0, 0, 0);
        codes.push(g);
        f.funcs = vec![1];
    }
    if name == "getglobal" || name == "setglobal" || name == "getglobal_jgt" {
        let s = f.site("g");
        f.e(Op::DeclGlobal, 0, s, 0);
        let s = f.site("g");
        f.e(Op::SetGlobal, 0, s, 0);
    }
    if ctx {
        f.e(Op::PushCtx, 0, 1, 0);
        f.e(Op::InitCtx, 0, 0, 0);
    }
    f.bind(head);
    f.ji(Op::JnLtI, 1, n, done);
    for _ in 0..k {
        match name {
            "nop" => drop(f.e(Op::Nop, 0, 0, 0)),
            "mov" => drop(f.e(Op::Mov, 2, 0, 0)),
            "loadi" => drop(f.li(2, 5)),
            "loadi_wide" => drop(f.li(2, 1000003)),
            "loadundef" => drop(f.e(Op::LoadUndef, 2, 0, 0)),
            "addi" => drop(f.e(Op::AddI, 2, 0, 5)),
            "add" => drop(f.e(Op::Add, 2, 0, 3)),
            "add_dbl" => drop(f.e(Op::Add, 2, 6, 6)),
            "muli" => drop(f.e(Op::MulI, 2, 0, 5)),
            "mod" => drop(f.e(Op::Mod, 2, 3, 0)),
            "modi" => drop(f.e(Op::ModI, 2, 3, 2)),
            "inc" => drop(f.e(Op::Inc, 2, 0, 0)),
            "jlti_no" => drop(f.ji(Op::JLtI, 0, 0, done)),
            "jlti_yes" => {
                let l = f.label();
                f.ji(Op::JnLtI, 0, 0, l);
                f.bind(l);
            }
            "jsne_null_no" => drop(f.j(Op::JSEqNull, 0, 0, done)),
            "jmpifnot_no" => drop(f.j(Op::JmpIfNot, 0, 0, done)),
            "jmp" => {
                let l = f.label();
                f.j(Op::Jmp, 0, 0, l);
                f.bind(l);
            }
            "getfield" => drop(f.get(2, 4, "a")),
            "setfield" => drop(f.set(4, 0, "a")),
            "getglobal" => {
                let s = f.site("g");
                f.e(Op::GetGlobal, 2, s, 0);
            }
            "setglobal" => {
                let s = f.site("g");
                f.e(Op::SetGlobal, 0, s, 0);
            }
            "getctx" => drop(f.e(Op::GetCtx, 2, 0, 0)),
            "setctx" => drop(f.e(Op::SetCtx, 0, 0, 0)),
            "lt" => drop(f.e(Op::Lt, 2, 0, 3)),
            "jgt_no" => drop(f.j(Op::JGt, 0, 3, done)),
            "lt_jmpifnot" => {
                f.e(Op::Lt, 2, 0, 3);
                f.j(Op::JmpIfNot, 2, 0, done);
            }
            "mov_mov" => {
                f.e(Op::Mov, 2, 0, 0);
                f.e(Op::Mov, 7, 3, 0);
            }
            "getfield_getfield" => {
                f.get(2, 4, "a");
                f.get(7, 4, "a");
            }
            "getglobal_jgt" => {
                let s = f.site("g");
                f.e(Op::GetGlobal, 2, s, 0);
                f.j(Op::JGt, 2, 3, done);
            }
            "inc_jmpback_pair" => {
                f.e(Op::Inc, 2, 0, 0);
                let l = f.label();
                f.j(Op::Jmp, 0, 0, l);
                f.bind(l);
            }
            "getelem" => drop(f.e(Op::GetElem, 2, 5, 12)),
            "newobjlit" => drop(f.e(Op::NewObjLit, 2, 12, 0)),
            "getelemi" => drop(f.e(Op::GetElemI, 2, 5, 1)),
            "call0" => drop(f.e(Op::Call, 2, 8, 0)),
            "call2" => drop(f.e(Op::CallU, 2, 8, 2)),
            "callu0" => drop(f.e(Op::CallU, 2, 8, 0)),
            "closure" => drop(f.e(Op::Closure, 2, 0, 0)),
            "newobj" => drop(f.e(Op::NewObject, 2, 1, 0)),
            "strictneq" => drop(f.e(Op::StrictNe, 2, 0, 3)),
            _ => panic!("unknown case {name}"),
        }
    }
    f.e(Op::Inc, 1, 1, 0);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    if ctx {
        f.e(Op::PopCtx, 0, 0, 0);
    }
    f.e(Op::Ret, 0, 0, 0);
    let mut all = vec![f.build()];
    all.extend(codes.into_iter().map(F::build));
    let mut vm = Vm::new(Program { codes: all }, sh);
    let r = vm.run_main(0).unwrap_or_else(|e| panic!("run: {e:?}"));
    if r.as_f64() != Some(3.0) {
        eprintln!("FAIL: result {r:?}");
        std::process::exit(1);
    }
    println!("ok {name} k={k} n={n}");
}

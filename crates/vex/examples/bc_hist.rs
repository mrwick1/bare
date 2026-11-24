//! Op histogram of the compiler (S2.5), static: how many instructions of each op the compiler emits for a source.
//!
//!   bc_hist [-v] [PATH...]
//!
//! With no path it compiles the sources of the `eval_load` cases (`examples/data/eval_*.js`, the same text as the old engine's
//! `eval_load CASE 1 --source`). With paths it compiles those files. Every complete function (one with no unbuilt construct) must pass the verifier, or the
//! program exits with 1. `-v` also prints the disassembly. The old engine's counts (`tools/js/bc_hist.sh` counts executed ops; the old
//! compiler dumps are static) are in `docs/progress.md`.

use bare_js2::atoms::SharedAtoms;
use bare_js2::bytecode::decode;
use bare_js2::compile::{Compiled, compile_all};
use bare_js2::parse::Limits;
use bare_js2::verify::verify;
use std::collections::BTreeMap;
use std::time::Instant;

const CASES: [(&str, &str); 9] = [
    ("loop", include_str!("data/eval_loop.js")),
    ("loopfn", include_str!("data/eval_loopfn.js")),
    ("calls", include_str!("data/eval_calls.js")),
    ("fib", include_str!("data/eval_fib.js")),
    ("closures", include_str!("data/eval_closures.js")),
    ("objects", include_str!("data/eval_objects.js")),
    ("webpack", include_str!("data/eval_webpack.js")),
    ("throws", include_str!("data/eval_throws.js")),
    ("concat", include_str!("data/eval_concat.js")),
];

#[derive(Default)]
struct Tally {
    functions: usize,
    /// Functions that hold a construct the compiler does not build: their code is not complete.
    partial: usize,
    words: usize,
    ins: usize,
    script_words: usize,
    script_ins: usize,
    nregs: usize,
    root_bytes: usize,
    ops: BTreeMap<&'static str, usize>,
    unsupported: BTreeMap<&'static str, usize>,
}

impl Tally {
    fn add(&mut self, c: &Compiled, script: bool, verbose: bool, name: &str) {
        // The code of a partial function is not complete (a register that an unbuilt construct would write is not written), so only a
        // complete function must pass the verifier.
        if c.unsupported.is_empty()
            && let Err(e) = verify(&c.code)
        {
            eprintln!("VERIFY FAILED in {name}: {e}\n{}", c.code.disassemble());
            std::process::exit(1);
        }
        if verbose {
            println!("-- {name}\n{}handlers {:?}\nunsupported {:?}", c.code.disassemble(), c.code.handlers, c.unsupported);
        }
        let mut pc = 0;
        let mut n = 0;
        while pc < c.code.words.len() {
            let (ins, len) = decode(&c.code.words, pc).expect("decode");
            *self.ops.entry(ins.op.name()).or_default() += 1;
            n += 1;
            pc += len;
        }
        if script {
            self.script_words += c.code.words.len();
            self.script_ins += n;
        } else {
            self.functions += 1;
            self.words += c.code.words.len();
            self.ins += n;
        }
        self.nregs += c.code.nregs as usize;
        self.root_bytes += c.code.roots.bytes();
        self.partial += usize::from(!c.unsupported.is_empty());
        for u in &c.unsupported {
            *self.unsupported.entry(u).or_default() += 1;
        }
    }
}

fn run(name: &str, src: &str, verbose: bool, all: &mut BTreeMap<&'static str, usize>) {
    let atoms = SharedAtoms::default();
    let t = Instant::now();
    let (script, fns) = match compile_all(src, &atoms, Limits::default()) {
        Ok(r) => r,
        Err(e) => {
            println!("{name}: compile failed: {e}");
            return;
        }
    };
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let mut t = Tally::default();
    t.add(&script, true, verbose, &format!("{name} script"));
    for (id, f) in &fns {
        t.add(f, false, verbose, &format!("{name} fn {id}"));
    }
    println!(
        "{name}: functions={} (partial {}) words={} ins={} | script words={} ins={} | nregs={} root_bytes={} | compile {ms:.2} ms | unsupported={:?}",
        t.functions, t.partial, t.words, t.ins, t.script_words, t.script_ins, t.nregs, t.root_bytes, t.unsupported
    );
    println!("  ops: {}", top(&t.ops, 14));
    for (o, n) in &t.ops {
        *all.entry(o).or_default() += n;
    }
}

/// The `n` most frequent ops, as text.
fn top(ops: &BTreeMap<&'static str, usize>, n: usize) -> String {
    let mut v: Vec<_> = ops.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter().take(n).map(|(o, c)| format!("{o} {c}")).collect::<Vec<_>>().join(", ")
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.first().is_some_and(|a| a == "-v");
    if verbose {
        args.remove(0);
    }
    let mut all = BTreeMap::new();
    if args.is_empty() {
        for (n, s) in CASES {
            run(n, s, verbose, &mut all);
        }
        println!("all cases: {} ins; ops: {}", all.values().sum::<usize>(), top(&all, 20));
    } else {
        for p in args {
            let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"));
            run(&p, &s, verbose, &mut all);
        }
    }
}

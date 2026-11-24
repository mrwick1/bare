//! The differential runner: the same script on the old engine and on the new one (`bare-vex`); exit status 1 on a difference.
//!
//!   diff_run [-v] [FILE...]   with no file: the whole corpus (see `src/diff.rs`); with files: each file is a program, and what it
//!                             `print`s is compared (a file name is its case name)
//!
//! Known differences are in `tools/fixtures/js/diff-known.txt`; `tools/js/diff_check.sh` builds and runs this.
use bare_js_diff::diff::*;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.first().is_some_and(|a| a == "-v");
    if verbose {
        args.remove(0);
    }
    let root = root();
    let (cases, known) = if args.is_empty() {
        (corpus(&root), known(&root))
    } else {
        (args.iter().map(|f| Case { name: f.clone(), src: std::fs::read_to_string(f).expect("read the file"), global: String::new() }).collect(), vec![])
    };
    // The engines use deep recursion on some programs: the same stack as the other runners.
    let r = std::thread::Builder::new().stack_size(256 << 20).spawn(move || run_all(&cases, &known)).unwrap().join().unwrap();
    print(&r, verbose);
    if !r.ok() {
        std::process::exit(1);
    }
}

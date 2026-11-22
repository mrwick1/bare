//! Measure one source compiled in multiple fresh realms. Pass `cold` to clear the cache before each realm.
//! Usage: code_cache_load FILE REALMS [cold]
use bare_js2::{code_cache, script};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("source file");
    let count: usize = args.next().expect("realm count").parse().expect("integer realm count");
    let cold = args.next().is_some_and(|s| s == "cold");
    let source = std::fs::read_to_string(file).expect("read source");
    let start = Instant::now();
    for _ in 0..count {
        if cold {
            code_cache::clear();
        }
        std::hint::black_box(script::load(&source).expect("compile source"));
    }
    println!("{count} realms in {:?} ({:?})", start.elapsed(), code_cache::counts());
}

//! Cost of `new Error` in a large one-line script. Run one fresh process per case: error_cost BYTES COUNT.
use bare_js::{eval::{EvalLimits, Interp}, heap::Heap};
#[path = "common/exec.rs"]
mod exec;

fn main() {
    let mut a = std::env::args().skip(1);
    let bytes: usize = a.next().expect("bytes").parse().unwrap();
    let n: usize = a.next().expect("count").parse().unwrap();
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000_000, ..Default::default() }).unwrap();
        exec::set(&mut it);
        let pad = format!("/*{}*/", "x".repeat(bytes));
        let src = format!("{pad}function make(){{return new Error('load')}} var c=0; for(var i=0;i<{n};i++){{make();c++}} c");
        let v = it.run_script(&src).unwrap();
        assert_eq!(it.display(v), n.to_string());
    }).unwrap().join().unwrap();
}

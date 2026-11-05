//! Bounded Error.stack load. Run one fresh process per scale.
use bare_js::{eval::{EvalLimits, Interp}, heap::Heap};
use std::time::Instant;

fn main() {
    let n: usize = std::env::args().nth(1).expect("count").parse().unwrap();
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000_000, ..Default::default() }).unwrap();
        let base = it.heap.stats().live_objects;
        let started = Instant::now();
        let src = format!("function make(){{return new Error('load')}} var count=0; for(var i=0;i<{n};i++){{var e=make();var lines=e.stack.split('\\n');if(lines.length!==3||lines[0]!=='Error: load'||e.stack.length>2048)throw 1;count++}} e=null;count");
        let value = it.run_script(&src).unwrap();
        assert_eq!(it.display(value), n.to_string());
        let run = started.elapsed();
        let before = it.heap.stats().live_objects;
        it.collect(&[]).unwrap();
        let after = it.heap.stats().live_objects;
        assert!(after < base + 16 && before > after);
        let rss = std::fs::read_to_string("/proc/self/status").unwrap();
        let peak = rss.lines().find(|s| s.starts_with("VmHWM:")).unwrap();
        println!("errors={n} checksum=ok run_ms={:.1} objects_before={before} objects_after={after} {peak}", run.as_secs_f64()*1000.0);
    }).unwrap().join().unwrap();
}

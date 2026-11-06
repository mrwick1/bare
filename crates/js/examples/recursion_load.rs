//! Load test for deep recursion under the browser session limits (`EvalLimits::session`). One case at one depth per process,
//! so the peak memory belongs to that run. The thread has the stack of a real run (`main.rs`): 8 MiB, 64 MiB in a debug build.
//!
//!   recursion_load CASE DEPTH     CASE: react | plain | runaway
//!
//!   react    the React commit walk: `commitMutationEffectsOnFiber` and `recursivelyTraverseMutationEffects` call each other
//!            once for each level of a chain of DEPTH fibers (2 JS calls a level), with a `switch`, an `if`, a `for` loop and
//!            a `try`. Each fiber has a leaf sibling. DEPTH: 100, 1000, 2000.
//!   plain    one function that calls itself DEPTH times (DEPTH calls). DEPTH: 1000, 9000.
//!   runaway  a function that calls itself with no end; DEPTH is ignored. The run must end as `CallDepth`, the ceiling.
//!
//! The checksum is the same count made in Rust. Prints run time, peak resident memory and the native stack that the run used.

use bare_js::eval::{EvalLimits, Interp, Error, LimitKind};
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use std::time::Instant;

const COMMIT: &str = "
var MASK = 3;
function commitMutationEffectsOnFiber(fiber, root) {
  switch (fiber.tag) {
    case 0: case 11:
      recursivelyTraverseMutationEffects(root, fiber);
      root.count = root.count + 1;
      break;
    case 5:
      try { recursivelyTraverseMutationEffects(root, fiber); } catch (e) { root.err = e; }
      root.count = root.count + 1;
      break;
    default:
      recursivelyTraverseMutationEffects(root, fiber);
  }
}
function recursivelyTraverseMutationEffects(root, parentFiber) {
  if (parentFiber.subtreeFlags & MASK) {
    var child = parentFiber.child;
    for (; child !== null; child = child.sibling) commitMutationEffectsOnFiber(child, root);
  }
}
";

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn run_case(name: String, depth: usize) {
    // The checksum of `react`: tags cycle 0, 5, 3, 11; tags 0, 5 and 11 count; every leaf counts.
    let counted = (0..depth).filter(|i| i % 4 != 2).count() + depth;
    let (src, expect) = match name.as_str() {
        "react" => (
            format!(
                "{COMMIT} var tags = [0, 5, 3, 11], root = {{ tag: 3, subtreeFlags: 3, child: null, sibling: null, count: 0 }}, tail = root;
                 for (var i = 0; i < {depth}; i++) {{ var n = {{ tag: tags[i % 4], subtreeFlags: 3, child: null, sibling: null }}; n.sibling = {{ tag: 0, subtreeFlags: 0, child: null, sibling: null }}; tail.child = n; tail = n; }}
                 commitMutationEffectsOnFiber(root, root); root.count"
            ),
            counted.to_string(),
        ),
        "plain" => (format!("function f(n) {{ return n === 0 ? 0 : 1 + f(n - 1) }} f({depth})"), depth.to_string()),
        "runaway" => ("function f(n) { return f(n + 1) + 1 } f(0)".to_string(), "limit:CallDepth".to_string()),
        _ => die(format!("unknown case {name}")),
    };
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse: {e}")));
    let limits = EvalLimits { max_steps: u64::MAX / 4, ..EvalLimits::session() };
    let mut it = Interp::new(&prog, Heap::default(), limits).unwrap_or_else(|e| die(format!("setup: {e:?}")));
    let (rss0, t) = (status_kib("VmRSS:"), Instant::now());
    let got = match it.run() {
        Ok(v) => it.display(v),
        Err(Error::Limit(LimitKind::CallDepth)) => "limit:CallDepth".to_string(),
        Err(e) => die(format!("run: {e:?}")),
    };
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    if got != expect {
        die(format!("{name} {depth}: result {got} != expected {expect}"));
    }
    let st = it.stats();
    println!(
        "{name:8} {depth:>6} checksum=ok run={run_ms:8.2}ms steps={} peak_rss={:.1}MiB rss_growth={:.1}MiB",
        st.steps,
        status_kib("VmHWM:") as f64 / 1024.0,
        (status_kib("VmHWM:") - rss0.min(status_kib("VmHWM:"))) as f64 / 1024.0,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: recursion_load CASE DEPTH".into());
    }
    let depth: usize = a[2].parse().unwrap_or_else(|_| die("depth".into()));
    let name = a[1].clone();
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || run_case(name, depth)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}

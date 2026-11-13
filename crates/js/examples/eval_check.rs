//! Run the cases of `tools/fixtures/js/eval-cases.txt` with the bare-js evaluator and print `name<TAB>result`,
//! in the same format as `tools/js/eval_oracle.mjs` (Node is the oracle). `tools/js/eval_check.sh` diffs the two.
//!
//!   eval_check CASES_FILE
//!   eval_check -e 'SOURCE'     (one script, for debugging)
//!   EVAL_STACK_KIB=N           native stack of the run thread (to measure stack use per call level)
//!   EVAL_CHECK_STEPS=N         step budget of each case (default 500,000)
//!
//! `unsupported:WHAT` is printed when a construct or builtin is not implemented. `limit` for an execution limit.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
#[path = "common/exec.rs"]
mod exec;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", esc(&it.display(v))),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Symbol(_) => "symbol".into(),
        Value::BigInt(_) | Value::BigHeap(_) => format!("bigint:{}", it.display(v)),
        Value::Object(_) => it.display(v),
    }
}

/// Run one source. Public so the load tool and tests can share the format.
fn run(src: &str) -> String {
    let prog = match parse_script(src, Limits::default()) {
        Ok(p) => p,
        Err(e) => return format!("parse-error:{}", esc(e.msg)),
    };
    let mut it = match Interp::new(&prog, Heap::default(), EvalLimits { max_steps: std::env::var("EVAL_CHECK_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(500_000), ..Default::default() }) {
        Ok(i) => i,
        Err(Error::Unsupported(u)) => return format!("unsupported:{}", u.what),
        Err(e) => return format!("setup-error:{e:?}"),
    };
    exec::set(&mut it);
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) => {
            if matches!(v, Value::Object(_)) {
                let name = it.get_str(v, "name");
                let msg = it.get_str(v, "message");
                format!("throw:{}:{}", esc(&name), esc(&msg))
            } else {
                format!("throw-value:{}", show(&it, v))
            }
        }
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(Error::Limit(_)) => "limit".into(),
        Err(e) => format!("internal-error:{e:?}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The default 8 MiB main stack is enough for max_call_depth 100, but run on a named big stack to be safe.
    let t = std::thread::Builder::new().stack_size(std::env::var("EVAL_STACK_KIB").ok().and_then(|v| v.parse::<usize>().ok()).map_or(64 << 20, |k| k << 10)).spawn(move || {
        if args.get(1).map(String::as_str) == Some("-e") {
            println!("{}", run(&args[2]));
            return;
        }
        let text = std::fs::read_to_string(&args[1]).expect("cases file");
        let mut cases: Vec<(String, Vec<&str>)> = Vec::new();
        for line in text.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                cases.push((n.trim().to_string(), Vec::new()));
            } else if let Some(c) = cases.last_mut() {
                c.1.push(line);
            }
        }
        for (name, lines) in cases {
            println!("{name}\t{}", run(lines.join("\n").trim_end_matches('\n')));
        }
    });
    t.unwrap().join().unwrap();
}

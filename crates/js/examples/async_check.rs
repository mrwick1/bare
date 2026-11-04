//! Run the cases of `tools/fixtures/js/async-cases.txt` and print `name<TAB>pre ## post`, the format of
//! `tools/js/promise_oracle.mjs` (Node is the oracle). `tools/js/async_check.sh` diffs the two.
//! The last expression of a case is a reader function. It is called before and after `drain_microtasks`.
//!
//!   async_check CASES_FILE
//!   async_check -e 'SOURCE'     (one script)

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn fail(it: &mut Interp, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Error::Throw(v) => format!("throw-value:{}", esc(&it.display(v))),
        Error::Unsupported(u) => format!("unsupported:{}", u.what),
        Error::Limit(_) => "limit".into(),
        e => format!("internal-error:{e:?}"),
    }
}

fn run(src: &str) -> String {
    let prog = match parse_script(src, Limits::default()) {
        Ok(p) => p,
        Err(e) => return format!("parse-error:{}", esc(e.msg)),
    };
    let mut it = match Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }) {
        Ok(i) => i,
        Err(e) => return format!("setup-error:{e:?}"),
    };
    let reader = match it.run() {
        Ok(v) => v,
        Err(e) => return fail(&mut it, e),
    };
    let read = |it: &mut Interp| match it.call(reader, Value::Undefined, &[]) {
        Ok(v) => Ok(esc(&it.display(v))),
        Err(e) => Err(fail(it, e)),
    };
    let pre = match read(&mut it) {
        Ok(s) => s,
        Err(s) => return s,
    };
    if let Err(e) = it.drain_microtasks() {
        return fail(&mut it, e);
    }
    match read(&mut it) {
        Ok(post) => format!("{pre} ## {post}"),
        Err(s) => s,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let t = std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
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

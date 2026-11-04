//! Run the cases of `tools/fixtures/js/scheduler-cases.txt` in a `Browser` with a virtual clock and print
//! `name<TAB>result`, the format of `tools/js/scheduler_oracle.mjs`. After the script, the microtask queue is
//! drained and the clock advances 1000 ms (at most 10000 tasks). `tools/js/scheduler_check.sh` diffs the two.
//!
//!   scheduler_check CASES_FILE

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn run(src: &str) -> String {
    let mut b = match Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()) {
        Ok(b) => b,
        Err(e) => return format!("setup-error:{e:?}"),
    };
    b.use_virtual_clock();
    let reader = match b.run_script(src) {
        Ok(v) => v,
        Err(e) => return fail(&mut b, e),
    };
    if let Err(e) = b.drain().and_then(|_| b.advance_time(1000.0, 10_000).map(|_| 0)) {
        return fail(&mut b, e);
    }
    match b.it.call(reader, Value::Undefined, &[]) {
        Ok(v) => esc(&b.it.display(v)),
        Err(e) => fail(&mut b, e),
    }
}

fn fail(b: &mut Browser, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&b.it.get_str(v, "name")), esc(&b.it.get_str(v, "message"))),
        Error::Throw(v) => format!("throw-value:{}", esc(&b.it.display(v))),
        Error::Unsupported(u) => format!("unsupported:{}", u.what),
        Error::Limit(_) => "limit".into(),
        e => format!("internal-error:{e:?}"),
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: scheduler_check CASES_FILE");
    let text = std::fs::read_to_string(&path).expect("read cases");
    let mut cases: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            cases.push((name.trim().to_string(), Vec::new()));
        } else if let Some(c) = cases.last_mut() {
            c.1.push(line);
        }
    }
    for (name, lines) in cases {
        let src = lines.join("\n");
        let src = src.trim_end_matches('\n').to_string();
        let child = std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(&src)).unwrap();
        println!("{name}\t{}", child.join().unwrap());
    }
}

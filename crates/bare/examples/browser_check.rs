//! Run the cases of `tools/fixtures/js/browser-event-cases.txt` in a `Browser` and print `name<TAB>result`, the format
//! of `tools/js/browser_oracle.mjs` (Node is the oracle for plain EventTarget/Event). `tools/js/browser_check.sh` diffs the two.
//!
//!   browser_check CASES_FILE

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
    let r = b.run_script(src);
    let reader = match r {
        Ok(v) => v,
        Err(e) => return fail(&mut b, e),
    };
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
    let path = std::env::args().nth(1).expect("cases file");
    let t = std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
        let text = std::fs::read_to_string(path).expect("cases file");
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

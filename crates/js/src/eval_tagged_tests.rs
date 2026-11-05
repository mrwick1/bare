//! Tagged template tests. The corpus (`tools/fixtures/js/tagged-cases.txt`) is checked against what Node gave
//! (`tagged-cases.expected`, made by `tools/js/eval_oracle.mjs`; `tools/js/tagged_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: GC of the site objects and sites in more than one script.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(Value::String(s)) => format!("string:{}", esc(&it.display(Value::String(s)))),
        Ok(v @ Value::Number(_)) => format!("number:{}", it.display(v)),
        Ok(v @ Value::Bool(_)) => format!("boolean:{}", it.display(v)),
        Ok(Value::Undefined) => "undefined".into(),
        Ok(Value::Null) => "null".into(),
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/tagged-cases.txt"), include_str!("../../../tools/fixtures/js/tagged-cases.expected"));
    big(move || {
        let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in cases.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                list.push((n.trim(), Vec::new()));
            } else if let Some(c) = list.last_mut() {
                c.1.push(line);
            }
        }
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(list.len(), want.len());
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }).expect("setup");
            let r = it.run();
            let got = format!("{name}\t{}", show(&mut it, r));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

#[test]
fn a_site_object_survives_a_collect_when_no_script_holds_it() {
    big(|| {
        let mut it = realm(EvalLimits::default());
        // Only the site table keeps the strings object alive: the WeakMap entry would go with it.
        out(&mut it, "function t(s) { return s; } function f() { return t`a${1}b`; } var w = new WeakMap(); w.set(f(), 1);");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "w.has(f()) + ':' + f().raw.join('|') + ':' + f().length"), "string:true:a|b:2");
    });
}

#[test]
fn sites_are_per_script_and_a_function_keeps_the_site_of_its_own_script() {
    big(|| {
        let mut it = realm(EvalLimits::default());
        // Both scripts put the tagged template at the same expression index.
        out(&mut it, "function t(s) { return s; } var a = t`x`;");
        out(&mut it, "var b = t`x`;");
        assert_eq!(out(&mut it, "a === b"), "boolean:false");
        out(&mut it, "function f() { return t`y`; } var c = f();");
        assert_eq!(out(&mut it, "c === f() && f() !== a && f()[0]"), "string:y");
    });
}

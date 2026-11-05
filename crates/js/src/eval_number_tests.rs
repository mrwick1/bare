//! Tests of Number, Math, the URI and global functions and `String.fromCodePoint`. The corpus
//! (`tools/fixtures/js/number-cases.txt`) is checked against what Node gave (`number-cases.expected`, made by
//! `tools/js/eval_oracle.mjs`; `tools/js/number_check.sh` re-runs Node and the matrix cases; `tools/js/math_check.sh`
//! checks the fdlibm port on about 75,000 inputs). The tests below cover what Node cannot show: limits and GC.

use super::*;
use crate::heap::{Heap, HeapLimits};
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

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

/// The corpus without the matrix cases (too slow for a debug build; `number_check.sh` runs them).
#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/number-cases.txt"), include_str!("../../../tools/fixtures/js/number-cases.expected"));
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
            if name.ends_with("_matrix") {
                continue;
            }
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).expect("setup");
            let r = it.run();
            let got = format!("{name}\t{}", show(&mut it, r));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// `encodeURI` and `escape` check the exact output size against the string cap before they make the text.
#[test]
fn encodings_respect_the_string_cap() {
    big(|| {
        let heap = Heap::new(HeapLimits { max_string_units: 100, ..Default::default() }).unwrap();
        let mut it = Interp::realm(heap, EvalLimits::default()).unwrap();
        out(&mut it, "var a = new Array(91).join('a'), e = new Array(34).join('\\u00e9'), f = new Array(35).join('\\u00e9');");
        // kept units count 1: 90 letters fit, 33 units of 3 fit, 34 do not
        assert_eq!(out(&mut it, "escape(a).length + ':' + escape(e).length"), "string:90:99");
        assert_eq!(out(&mut it, "escape(f)"), "limit:Heap(StringLength)");
        assert_eq!(out(&mut it, "encodeURI(a).length"), "number:90");
        assert_eq!(out(&mut it, "encodeURI(new Array(17).join('\\u00e9')).length"), "number:96");
        assert_eq!(out(&mut it, "encodeURI(new Array(18).join('\\u00e9'))"), "limit:Heap(StringLength)");
    });
}

/// Every global that scans its argument charges the scan before it reads the string.
#[test]
fn scans_charge_steps() {
    big(|| {
        for call in ["parseFloat(s)", "encodeURI(s)", "decodeURI(s)", "decodeURIComponent(s)", "escape(s)", "unescape(s)"] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.run_script("var s = new Array(10000).join('1');").unwrap();
            it.limits.max_steps = 100;
            assert!(matches!(it.run_script(call), Err(Error::Limit(LimitKind::Steps))), "{call}");
        }
    });
}

/// Math and the shared parse functions stay alive through a collect after their globals are deleted.
#[test]
fn math_and_parse_functions_survive_gc() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        out(&mut it, "var m = Math, n = Number; delete globalThis.Math; delete globalThis.Number; delete globalThis.parseFloat;");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "m.pow(2, 10) + ':' + m.sin(1) + ':' + n.parseFloat('1.5x') + ':' + (1.005).toFixed(2) + ':' + m.PI"), "string:1024:0.8414709848078965:1.5:1.00:3.141592653589793");
        assert_eq!(out(&mut it, "typeof parseFloat + ':' + (n.parseInt === parseInt)"), "string:undefined:true");
    });
}

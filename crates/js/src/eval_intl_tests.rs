//! Intl.DateTimeFormat tests. The corpus (`tools/fixtures/js/intl-cases.txt`) is checked against what Node gave
//! (`intl-cases.expected`, made by `tools/js/eval_oracle.mjs` with TZ=UTC; `tools/js/intl_check.sh` re-runs Node and all
//! matrix cases). The tests below cover what Node cannot show: GC of formatters, the cap, the gates and locale fallback.

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

fn realm() -> Interp<'static> {
    let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
    it.set_default_time_zone("UTC").unwrap();
    it
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

/// The corpus without the matrix cases (too slow for a debug build), plus one matrix case.
#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/intl-cases.txt"), include_str!("../../../tools/fixtures/js/intl-cases.expected"));
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
            if name.starts_with("matrix_") && *name != "matrix_en-US_w2_y1" {
                continue;
            }
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).expect("setup");
            it.set_default_time_zone("UTC").unwrap();
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
fn formatters_and_their_bound_format_are_collected_when_dead() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = new Intl.DateTimeFormat('en-US', { hour: 'numeric' }); var g = keep.format; for (var i = 0; i < 50; i++) new Intl.DateTimeFormat('en').format; 0;");
        assert_eq!(it.intl_tracked(), 51);
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked(), 1);
        // the formatter keeps its bound format: the getter gives the same function after a collect
        out(&mut it, "g = null;");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "var a = keep.format; a === keep.format && a(0)"), "string:12 AM");
        // the bound format keeps its formatter
        out(&mut it, "g = keep.format; keep = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked(), 1);
        assert_eq!(out(&mut it, "g(3600000)"), "string:1 AM");
        out(&mut it, "g = null; a = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked(), 0);
    });
}

#[test]
fn formatters_are_capped_before_the_object_is_made() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_formatters: 3, ..Default::default() });
        assert_eq!(out(&mut it, "var a = [1, 2, 3].map(function () { return new Intl.DateTimeFormat(); }); a.length"), "number:3");
        let objects = it.heap.stats().live_objects;
        assert_eq!(out(&mut it, "try { new Intl.DateTimeFormat(); } catch (e) { 'caught' }"), "limit:Sc(Formatters)");
        assert_eq!(it.heap.stats().live_objects, objects, "the object is not made past the cap");
        // toLocaleString makes no tracked formatter
        assert_eq!(out(&mut it, "new Date(0).toLocaleString('en-US')"), "string:1/1/1970, 12:00:00 AM");
        out(&mut it, "a = null;");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "typeof new Intl.DateTimeFormat()"), "string:object");
    });
}

#[test]
fn gates_refuse_what_bare_cannot_format() {
    big(|| {
        let mut it = realm();
        for (src, what) in [
            ("new Intl.DateTimeFormat('en', { era: 'short' })", "Intl.DateTimeFormat option era"),
            ("new Intl.DateTimeFormat('en', { timeZoneName: 'short' })", "Intl.DateTimeFormat option timeZoneName"),
            ("new Intl.DateTimeFormat('en-u-hc-h23')", "Intl: a locale with an extension (-u-, -t-, -x-)"),
            ("new Intl.DateTimeFormat('en', { timeStyle: 'long' }).format(0)", "Intl.DateTimeFormat: a time zone name (timeStyle full or long)"),
            ("new Intl.DateTimeFormat('en', { year: 'numeric' }).format(Date.UTC(-5, 0))", "Intl.DateTimeFormat: a year before 1 (an era)"),
        ] {
            assert_eq!(out(&mut it, src), format!("unsupported:{what}"), "{src}");
        }
    });
}

#[test]
fn a_locale_bare_lacks_falls_back_as_ecma402_allows() {
    big(|| {
        let mut it = realm();
        let r = out(&mut it, "['fr', ['fr', 'en-GB'], 'en-AU', 'de-DE-u'].map(function (l) { try { return new Intl.DateTimeFormat(l).resolvedOptions().locale; } catch (e) { return e.message; } }).join()");
        assert_eq!(r, "string:en-US,en-GB,en,Invalid language tag: de-de-u");
        assert_eq!(out(&mut it, "Intl.DateTimeFormat.supportedLocalesOf(['fr', 'en-AU', 'en-GB']).join()"), "string:en-AU,en-GB");
    });
}

#[test]
fn zones_come_from_the_system_and_bad_names_throw() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "new Intl.DateTimeFormat('en-US', { timeZone: 'Asia/Kolkata', hour: 'numeric', minute: 'numeric' }).format(0)"), "string:5:30 AM");
        assert_eq!(out(&mut it, "new Intl.DateTimeFormat().resolvedOptions().timeZone"), "string:UTC");
        it.set_default_time_zone("America/New_York").unwrap();
        assert_eq!(out(&mut it, "new Date(0).toLocaleString('en-US')"), "string:12/31/1969, 7:00:00 PM");
        assert!(it.set_default_time_zone("No/Such").is_err());
        assert_eq!(out(&mut it, "try { new Intl.DateTimeFormat('en', { timeZone: 'No/Such' }); } catch (e) { e.message }"), "string:Invalid time zone specified: No/Such");
    });
}

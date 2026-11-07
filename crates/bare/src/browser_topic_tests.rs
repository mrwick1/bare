//! The Chrome-checked cases of the DOM APIs of this change. Each topic has a case file (`tools/fixtures/ui/<topic>-cases.txt`) and the
//! answers of headless Chrome (`chrome-<topic>.txt`, made by `tools/ui/chrome_topic.mjs`). A case is `=== name`, then optional `@html` and
//! `@css` lines, then a script (lines before the first case are a prelude of every script) whose last expression is a reader function. The host runs the script in a 400 x 300 window, moves the
//! virtual clock 400 ms (as Chrome waits 400 ms), calls the reader and compares its string with the one of Chrome.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

pub(crate) struct Case {
    pub name: String,
    pub html: String,
    pub css: String,
    pub script: String,
    /// The document URL (`@url` in the prelude), or empty for a document with none.
    pub url: String,
}

pub(crate) fn parse(cases: &str) -> Vec<Case> {
    let mut out: Vec<Case> = Vec::new();
    let mut pre = String::new();
    let mut url = String::new();
    for line in cases.lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            out.push(Case { name: n.trim().into(), html: String::new(), css: String::new(), script: pre.clone(), url: url.clone() });
        } else if out.is_empty() {
            if let Some(u) = line.strip_prefix("@url ") {
                url = u.trim().into();
                continue;
            }
            pre += line;
            pre += "\n";
        } else if let Some(k) = out.last_mut() {
            if let Some(h) = line.strip_prefix("@html ") {
                k.html += h;
            } else if let Some(c) = line.strip_prefix("@css ") {
                k.css += c;
            } else {
                k.script += line;
                k.script += "\n";
            }
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// The answer of this host for one case, in the format of the Chrome file.
pub(crate) fn run(k: &Case) -> String {
    let html = format!("<!doctype html><html><head><style>{}</style></head><body>{}</body></html>", k.css, k.html);
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    if !k.url.is_empty() {
        b.set_document_url(&k.url).unwrap();
    }
    b.set_viewport(400.0, 300.0).unwrap();
    let out = (|| -> Result<String, Error> {
        let reader = b.run_script(&k.script)?;
        b.advance_time(400.0, 10_000)?;
        let v = b.it.call(reader, Value::Undefined, &[])?;
        Ok(b.it.display(v))
    })();
    esc(&match out {
        Ok(s) => s,
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    })
}

/// Compare every case of a topic with Chrome. Gives the number of cases. A case with no Chrome line fails.
pub(crate) fn check(topic: &str, cases: &str, chrome: &str) -> usize {
    let cases = parse(cases);
    let chrome: std::collections::HashMap<&str, &str> = chrome.lines().filter_map(|l| l.split_once('\t')).collect();
    let mut bad = Vec::new();
    for k in &cases {
        let want = chrome.get(k.name.as_str()).copied().unwrap_or("<no chrome line>");
        let got = run(k);
        if got != want {
            bad.push(format!("{topic}/{}: chrome {want:?} bare {got:?}", k.name));
        }
    }
    assert!(bad.is_empty(), "{} of {} cases differ:\n{}", bad.len(), cases.len(), bad.join("\n"));
    cases.len()
}

macro_rules! topic_test {
    ($fn:ident, $topic:literal, $n:expr) => {
        #[test]
        fn $fn() {
            let n = $crate::browser_topic_tests::check(
                $topic,
                include_str!(concat!("../../../tools/fixtures/ui/", $topic, "-cases.txt")),
                include_str!(concat!("../../../tools/fixtures/ui/chrome-", $topic, ".txt")),
            );
            assert_eq!(n, $n);
        }
    };
}

pub(crate) use topic_test;

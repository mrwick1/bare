//! Feature census (cargo feature `census`, plan step P0.1 of `docs/plan-engine-rewrite.md`). It counts each syntax kind that
//! the parser keeps (each lazy function body once, when it is parsed for its first call) and each native built-in call.
//! When a realm ends, the totals of the process so far are written to the file named by `BARE_JS_CENSUS`, one
//! `count<TAB>key` line each, the largest first. The last realm to end writes the full list.
//! Not counted: calls to promise functions and to the host, and built-ins that the bytecode lane runs without a call.

use crate::ast::Program;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;

static COUNTS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

fn add(key: String, n: u64) {
    *COUNTS.lock().unwrap().get_or_insert_default().entry(key).or_default() += n;
}

/// The variant name of `d`, with its operator when it has one: `Binary { op: Add, .. }` gives `Binary.Add`.
fn kind(d: &dyn Debug) -> String {
    let s = format!("{d:?}");
    let name = &s[..s.find([' ', '(', '{']).unwrap_or(s.len())];
    match s.split_once("op: ") {
        Some((_, r)) => format!("{name}.{}", &r[..r.find([',', ' ']).unwrap_or(r.len())]),
        None => name.to_string(),
    }
}

/// Count the nodes of a tree that the parser made. In the tree of a lazy function (`lazy`), the function itself is the last one, and
/// the first parse counted it already.
pub(crate) fn program(p: &Program, lazy: bool) {
    let mut m: HashMap<String, u64> = HashMap::new();
    let mut put = |pre: &str, d: &dyn Debug| *m.entry(format!("{pre}.{}", kind(d))).or_default() += 1;
    p.exprs.iter().for_each(|e| put("expr", &e.kind));
    p.stmts.iter().for_each(|s| put("stmt", &s.kind));
    p.pats.iter().for_each(|x| put("pat", &x.kind));
    p.elems.iter().for_each(|e| put("class", &e.kind));
    p.funcs[..p.funcs.len() - lazy as usize].iter().for_each(|f| *m.entry(format!("func.{:?}.{:?}", f.kind, f.mode)).or_default() += 1);
    m.into_iter().for_each(|(k, n)| add(k, n));
}

/// Count one call of a native built-in: `Ao(MathFloor)` gives `native.Ao.MathFloor`.
pub(crate) fn native(n: &dyn Debug) {
    add(format!("native.{}", format!("{n:?}").replace('(', ".").replace(')', "")), 1);
}

/// Writes the totals when the realm that owns it ends.
#[derive(Default)]
pub(crate) struct Flush;

impl Drop for Flush {
    fn drop(&mut self) {
        if let Some(path) = std::env::var_os("BARE_JS_CENSUS") {
            let _ = std::fs::write(path, list());
        }
    }
}

/// The totals so far, one `count<TAB>key` line each, the largest first.
fn list() -> String {
    let g = COUNTS.lock().unwrap();
    let mut v: Vec<_> = g.iter().flat_map(|m| m.iter()).collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter().map(|(k, n)| format!("{n}\t{k}\n")).collect()
}

#[cfg(test)]
mod tests {
    use crate::eval::{EvalLimits, Interp};
    use crate::heap::Heap;
    use crate::parse::{parse_script, Limits};

    fn count(list: &str, key: &str) -> u64 {
        list.lines().find_map(|l| l.split_once('\t').filter(|x| x.1 == key)).map_or(0, |x| x.0.parse().unwrap())
    }

    #[test]
    fn the_census_counts_syntax_kinds_and_native_calls() {
        let src = "var s = 0; for (const x of [1, 2]) s = s + Math.floor(x / 3); class C { m() {} } s";
        let before = super::list();
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let after = super::list();
        // Other tests add to the same totals at the same time, so each key grows by at least the count of this script.
        let grew = |k: &str| count(&after, k) - count(&before, k);
        assert!(grew("stmt.ForOf") >= 1);
        assert!(grew("expr.Binary.Add") >= 1);
        assert!(grew("expr.Binary.Div") >= 1);
        assert!(grew("class.Method") >= 1);
        assert!(grew("func.Method.Env") + grew("func.Method.Plain") >= 1);
        assert!(grew("native.Ao.MathFloor") >= 2, "{after}");
    }
}

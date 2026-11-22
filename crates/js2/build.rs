//! Generate the prelude facts with the same parser that the engine uses.

#![allow(dead_code)]

#[path = "src/ast.rs"]
mod ast;
#[path = "src/facts.rs"]
mod facts;
#[path = "src/lex.rs"]
mod lex;
#[path = "src/parse.rs"]
mod parse;
#[path = "src/regexp_check.rs"]
mod regexp_check;

use facts::Facts;
use parse::{Limits, parse_script_lazy};
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/prelude.js");
    let prelude = std::fs::read_to_string("src/prelude.js").expect("read src/prelude.js");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    std::fs::write(out.join("prelude_facts.rs"), render(&prelude)).expect("write prelude_facts.rs");
}

/// Render the parser facts as static Rust tables.
fn render(prelude: &str) -> String {
    let mut facts = Facts::new();
    parse_script_lazy(prelude, Limits::default(), &mut facts).expect("parse src/prelude.js");
    let mut s = String::new();
    s.push_str("// Generated from src/prelude.js.\n\n");
    s.push_str("use crate::ast::{FuncKind as K, FuncMode as M, Span as S};\nuse crate::facts::{FnFacts as F, Facts};\n\n");
    let sp = |s: Option<crate::ast::Span>| match s {
        Some(n) => format!("Some(S {{ start: {}, end: {} }})", n.start, n.end),
        None => "None".into(),
    };
    writeln!(s, "static FNS: [F; {}] = [", facts.fns.len()).unwrap();
    for f in &facts.fns {
        writeln!(
            s,
            "    F {{ start: {}, parse_at: {}, end: {}, name: {}, kind: K::{:?}, mode: M::{:?}, flags: {}, strict: {}, strict_out: {}, simple: {}, no_in: {}, ctx: {}, length: {}, sub: {}, caps: {}, ncaps: {} }},",
            f.start, f.parse_at, f.end, sp(f.name), f.kind, f.mode, f.flags, f.strict, f.strict_out, f.simple, f.no_in, f.ctx, f.length, f.sub, f.caps, f.ncaps
        )
        .unwrap();
    }
    s.push_str("];\n\n");
    writeln!(s, "static CAPS: [S; {}] = [", facts.caps.len()).unwrap();
    for c in &facts.caps {
        writeln!(s, "    S {{ start: {}, end: {} }},", c.start, c.end).unwrap();
    }
    s.push_str("];\n\n");
    let tops: Vec<_> = facts.children(None).collect();
    s.push_str("/// The top-level functions of `prelude.js`: the name and the id of its facts.\n");
    writeln!(s, "pub static TOP: [(&str, u32); {}] = [", tops.len()).unwrap();
    for id in tops.into_iter().rev() {
        let name = facts.get(id).name.expect("a top-level function has a name");
        let name = &prelude[name.start as usize..name.end as usize];
        writeln!(s, "    ({name:?}, {id}),").unwrap();
    }
    s.push_str("];\n\n");
    writeln!(
        s,
        "static BY_START: [u32; {}] = {:?};\n",
        facts.by_start.len(),
        facts.by_start
    )
    .unwrap();
    s.push_str("/// The facts of `prelude.js`, as a lazy pre-parse of it gives them.\n");
    writeln!(
        s,
        "pub fn facts() -> Facts {{\n    Facts {{ fns: FNS.to_vec(), caps: CAPS.to_vec(), by_start: BY_START.to_vec(), preparsed: {}, module: {} }}\n}}",
        facts.preparsed, facts.module
    )
    .unwrap();
    s
}

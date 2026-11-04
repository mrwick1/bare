//! Compare the lexer with the Acorn token oracle on one file (S2). Scanning only: nothing is run.
//!
//!   node tools/js_lex_oracle.mjs FILE > FILE.tokens
//!   cargo run --release -p bare-js --example lex_check -- FILE FILE.tokens
//!
//! The oracle line gives the lexical goal for each token (the job of the parser). The lexer must then
//! find the same class, byte span and `newline_before`. Every string, template, number, name and regexp
//! is also decoded. Any difference exits with 1. The last line is `key=value`.

use bare_js::lex::*;
use std::process::exit;

fn fail(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    exit(1)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (Some(file), Some(oracle)) = (a.get(1), a.get(2)) else { fail("usage: lex_check FILE ORACLE".into()) };
    let src = std::fs::read_to_string(file).unwrap_or_else(|e| fail(format!("{file}: {e}")));
    let oracle = std::fs::read_to_string(oracle).unwrap_or_else(|e| fail(format!("{oracle}: {e}")));
    let mut lexer = Lexer::new(&src).unwrap_or_else(|e| fail(format!("{e:?}")));
    let (mut n, mut sum, mut units) = (0u64, 0xcbf29ce484222325u64, 0u64);
    let mut buf = Vec::new();
    for line in oracle.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        let (class, goal) = (f[0], f[1]);
        let (start, end, nl): (u32, u32, bool) = (f[2].parse().unwrap(), f[3].parse().unwrap(), f[4] == "1");
        let goal = match goal {
            "R" => Goal::RegExp,
            "T" => Goal::TemplateTail,
            _ => Goal::Div,
        };
        let t = lexer.next(goal).unwrap_or_else(|e| fail(format!("token {n}: {:?} at {:?}, oracle {line}", e.kind, e.location(&src))));
        let got = match t.kind {
            Kind::Ident | Kind::Keyword(_) => "i",
            Kind::PrivateName => "p",
            Kind::Number(_) | Kind::BigInt(_) => "n",
            Kind::String { .. } => "s",
            Kind::RegExp => "r",
            Kind::Template { .. } => "t",
            Kind::Punct(_) => "x",
            Kind::Eof => "eof",
        };
        if got != class || t.start != start || t.end != end || t.newline_before != nl {
            fail(format!("token {n}: got {got} {}..{} nl={}, oracle {line}", t.start, t.end, t.newline_before));
        }
        buf.clear();
        let ok = match t.kind {
            Kind::String { .. } => decode_string(&src, &t, &mut buf).is_ok(),
            Kind::Template { bad_escape, .. } => decode_template(&src, &t, true, &mut buf).is_ok() && (bad_escape || decode_template(&src, &t, false, &mut buf).is_ok()),
            Kind::Ident | Kind::PrivateName | Kind::Keyword(_) => ident_name(&src, &t).is_ok(),
            Kind::Number(_) => number_value(&src, &t).is_some(),
            Kind::RegExp => regexp_parts(&src, &t).is_some(),
            _ => true,
        };
        if !ok {
            fail(format!("token {n}: decode failed for {line}"));
        }
        units += buf.len() as u64;
        for x in [class.as_bytes()[0] as u64, start as u64, end as u64] {
            sum = (sum ^ x).wrapping_mul(0x100000001b3);
        }
        n += 1;
    }
    let t = lexer.next(Goal::Div).unwrap_or_else(|e| fail(format!("after last token: {:?}", e.kind)));
    if t.kind != Kind::Eof {
        fail(format!("extra token {t:?}"));
    }
    println!("file={file} bytes={} tokens={n} utf16_units={units} fnv={sum:016x} ok=true", src.len());
}

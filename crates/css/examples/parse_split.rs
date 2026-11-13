//! Cost of the parts of reading a style sheet, so that a run under `perf stat -e instructions:u` can tell them apart.
//!
//!   parse_split MODE FILE.css REPS
//!
//! MODE: tok (tokenizer only), parse (`parse_stylesheet`), lazy (`parse_stylesheet_lazy`), scan (a byte-table scan for the rule boundaries, no tokens), scansel (scan + the selector list of every style rule: the floor of lazy bodies),
//! sel (parse + the selector list of every style rule), decl (parse + `parse_declaration` of every declaration),
//! add (`RuleSet::add`: all of it).
//! Run REPS=1 and REPS=11 and subtract: the difference over 10 is one pass without the start of the process and the read of the file.

use bare_css::cascade::{Origin, RuleSet};
use bare_css::parser::{AtBody, Rule, parse_stylesheet, parse_stylesheet_lazy};
use bare_css::selector::{Mode, parse_selector_list_in};
use bare_css::tokenizer::tokenize;
use bare_css::values::{Side, parse_declaration};

fn walk(rules: &[Rule<'_>], f: &mut impl FnMut(&bare_css::parser::StyleRule<'_>)) {
    for rule in rules {
        match rule {
            Rule::Style(r) => f(r),
            Rule::At(a) => {
                if let AtBody::Rules(inner) = &a.body {
                    walk(inner, f);
                }
            }
        }
    }
}

/// Find the rule boundaries with a byte table. Gives (prelude start, prelude end, body end) for each top-level rule or at-rule.
fn scan(b: &[u8], out: &mut Vec<[u32; 3]>) {
    scan_in(b, 0, false, out);
}

/// The rules of one list. A `@media`, `@supports` or `@layer` block is read as a list of its own, like the real parser does.
fn scan_in(b: &[u8], mut i: usize, nested: bool, out: &mut Vec<[u32; 3]>) -> usize {
    let mut t = [0u8; 256];
    for &c in b"{}()[]\"'/\\;" {
        t[c as usize] = 1;
    }
    let n = b.len();
    while i < n {
        while i < n && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        if nested && b[i] == b'}' {
            return i + 1;
        }
        let ps = i;
        let mut paren = 0i32;
        while i < n {
            let c = b[i];
            if t[c as usize] == 0 {
                i += 1;
                continue;
            }
            match c {
                b'{' if paren == 0 => break,
                b'(' | b'[' => {
                    paren += 1;
                    i += 1;
                }
                b')' | b']' => {
                    paren -= 1;
                    i += 1;
                }
                b'"' | b'\'' => {
                    i += 1;
                    while i < n && b[i] != c {
                        i += 1 + (b[i] == b'\\') as usize;
                    }
                    i += 1;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                }
                b';' if paren == 0 => {
                    i += 1;
                    break;
                }
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        if i >= n || b[i] != b'{' {
            continue;
        }
        let pe = i;
        i += 1;
        if b[ps] == b'@' && (b[ps..pe].starts_with(b"@media") || b[ps..pe].starts_with(b"@supports") || b[ps..pe].starts_with(b"@layer")) {
            i = scan_in(b, i, true, out);
            continue;
        }
        let mut depth = 1i32;
        while i < n && depth > 0 {
            let c = b[i];
            if t[c as usize] == 0 {
                i += 1;
                continue;
            }
            match c {
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b'}' => {
                    depth -= 1;
                    i += 1;
                }
                b'"' | b'\'' => {
                    i += 1;
                    while i < n && b[i] != c {
                        i += 1 + (b[i] == b'\\') as usize;
                    }
                    i += 1;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                }
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        out.push([ps as u32, pe as u32, i.min(n) as u32]);
    }
    n
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&a[2]).unwrap();
    let reps: usize = a[3].parse().unwrap();
    let mut sink = 0usize;
    for _ in 0..reps {
        match a[1].as_str() {
            "tok" => {
                let mut t = tokenize(&text);
                while let Some((_, s, e)) = t.next_spanned() {
                    sink += e - s;
                }
            }
            "parse" => sink += parse_stylesheet(&text).len(),
            "lazy" => {
                // Parse without reading the blocks; report how many style rules the scanner gave to the tokenizer.
                let rules = parse_stylesheet_lazy(&text);
                let (mut all, mut eager) = (0usize, 0usize);
                walk(&rules, &mut |r| {
                    all += 1;
                    eager += usize::from(r.lazy_block().is_none());
                });
                sink += all;
                if reps == 1 {
                    eprintln!("style rules {all}, read by the tokenizer {eager}");
                }
            }
            "scan" => {
                let mut v = Vec::new();
                scan(text.as_bytes(), &mut v);
                sink += v.len();
            }
            "scansel" => {
                let mut doc = bare_html::tree::parse("");
                let mut v = Vec::new();
                scan(text.as_bytes(), &mut v);
                for [ps, pe, _] in &v {
                    let prelude = text[*ps as usize..*pe as usize].trim();
                    if !prelude.starts_with('@') {
                        sink += parse_selector_list_in(prelude, &mut doc, Mode::Document).map_or(0, |l| l.len());
                    }
                }
            }
            "sel" => {
                let mut doc = bare_html::tree::parse("");
                let rules = parse_stylesheet(&text);
                walk(&rules, &mut |r| sink += parse_selector_list_in(r.prelude, &mut doc, Mode::Document).map_or(0, |l| l.len()));
            }
            "decl" => {
                let rules = parse_stylesheet(&text);
                let mut side = Side::default();
                let mut out = Vec::new();
                walk(&rules, &mut |r| {
                    for d in r.declarations().iter() {
                        if !d.name.starts_with("--") {
                            parse_declaration(&d.name, d.value, d.important, &mut side, &mut out);
                        }
                    }
                    sink += out.len();
                    out.clear();
                });
            }
            "add" => {
                let mut doc = bare_html::tree::parse("");
                let mut r = RuleSet::new((1280.0, 800.0));
                r.add(&mut doc, &text, Origin::Author);
                sink += r.len();
            }
            m => panic!("unknown mode {m}"),
        }
    }
    eprintln!("{} {} bytes x{reps} (sink {sink})", a[1], text.len());
}

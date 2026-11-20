//! Run a JavaScript file through js2 (lexer, parser, compiler, interpreter).
//!
//!   script_run FILE [NAME...]        print each named global (default `result`)
//!   script_run --eval FILE EXPECTED  an `eval_load` program: the last expression after the last `;` is its result; compare it with
//!                                    EXPECTED. Prints `ok` or the reason, exit status 1 on a failure.
//!   DIS=1 prints the bytecode and the root maps first.
use bare_js2::script;

fn main() {
    let mut a = std::env::args().skip(1).peekable();
    let eval = a.next_if(|s| s == "--eval").is_some();
    let file = a.next().expect("usage: script_run [--eval] FILE [NAME... | EXPECTED]");
    let mut src = std::fs::read_to_string(&file).expect("read the file");
    let mut names: Vec<String> = a.collect();
    let expected = if eval { Some(names.remove(0)) } else { None };
    if eval {
        let t = src.trim_end().to_string();
        // The last statement boundary at brace depth 0 (a `;` or a `}`); quotes are skipped.
        let (mut depth, mut quote, mut i) = (0i32, None, 0);
        for (k, c) in t.char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '\'' | '"') => quote = Some(c),
                (None, '{' | '(' | '[') => depth += 1,
                (None, '}' | ')' | ']') => {
                    depth -= 1;
                    if c == '}' && depth == 0 {
                        i = k + 1;
                    }
                }
                (None, ';') if depth == 0 => i = k + 1,
                _ => {}
            }
        }
        src = format!("{}\nvar __result = ({});", &t[..i], t[i..].trim());
        names = vec!["__result".into()];
    } else if names.is_empty() {
        names.push("result".into());
    }
    if std::env::var_os("DIS").is_some() {
        match script::compile(&src, &bare_js2::atoms::SharedAtoms::default(), Default::default()) {
            Ok(p) => p.codes.iter().for_each(|c| {
                println!("{}", c.disassemble());
                for &pc in c.roots.pcs() {
                    println!("  map {pc}: {:?}", c.roots.find(pc).unwrap().slots().collect::<Vec<_>>());
                }
            }),
            Err(e) => println!("FAIL {e}"),
        }
    }
    match script::run(&src) {
        Ok(mut vm) => {
            for n in names {
                let got = vm.global_text(&n);
                match &expected {
                    Some(want) if *want == got => println!("ok"),
                    Some(want) => {
                        println!("WRONG got {got:?} want {want:?}");
                        std::process::exit(1);
                    }
                    None => println!("{n} = {got}"),
                }
            }
        }
        Err(e) => {
            println!("FAIL {e}");
            std::process::exit(1);
        }
    }
}

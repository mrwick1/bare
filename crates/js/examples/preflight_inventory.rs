//! Parse and preflight (`Interp::new`, never `run`) script files and print the first remaining `Unsupported` of
//! each, with its span. Files are only read as text. Nothing runs, and no file goes to Node.
//!
//!   preflight_inventory FILE...

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};

fn line_col(src: &str, at: u32) -> (usize, usize) {
    let head = &src.as_bytes()[..(at as usize).min(src.len())];
    let line = head.iter().filter(|&&b| b == b'\n').count() + 1;
    (line, head.len() - head.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1) + 1)
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let t = std::thread::Builder::new().stack_size(256 << 20).spawn(move || {
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                println!("{f}\tunreadable");
                continue;
            };
            let name = f.rsplit('/').next().unwrap_or(&f);
            let prog = match parse_script(&src, Limits::default()) {
                Ok(p) => p,
                Err(e) if e.kind == bare_js::parse::ErrorKind::Unsupported => {
                    let (l, c) = line_col(&src, e.start);
                    println!("{name}\tparse-unsupported\t{}\t{l}:{c}\t{}", e.msg, snippet(&src, e.start, e.end));
                    continue;
                }
                Err(e) => {
                    println!("{name}\tparse-error\t{}", e.msg);
                    continue;
                }
            };
            match Interp::new(&prog, Heap::default(), EvalLimits::default()) {
                Ok(_) => println!("{name}\tpreflight-ok\t{} bytes", src.len()),
                Err(Error::Unsupported(u)) => {
                    let (l, c) = line_col(&src, u.span.start);
                    println!("{name}\tpreflight-unsupported\t{}\t{l}:{c}\t{}", u.what, snippet(&src, u.span.start, u.span.end));
                }
                Err(e) => println!("{name}\tpreflight-error\t{e:?}"),
            }
        }
    });
    t.unwrap().join().unwrap();
}

fn snippet(src: &str, a: u32, b: u32) -> String {
    let (a, b) = (a as usize, (b as usize).min(src.len()));
    let mut e = b.min(a + 60);
    while !src.is_char_boundary(e) {
        e -= 1;
    }
    src.get(a..e).unwrap_or("").replace('\n', " ")
}

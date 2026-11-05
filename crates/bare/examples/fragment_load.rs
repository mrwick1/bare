//! Load test for `Element.innerHTML`. One case at one scale per process.
//!
//!   fragment_load CASE SCALE      CASE: parse | replace | deep      SCALE: 1, 10, 100
//!
//! parse: SCALE*50 rounds. Each round sets `innerHTML` of a detached `div` to canonical markup that Rust generates from the round number alone
//! (20 paragraphs with attributes, entities, comments, void tags, a script and an SVG), then reads it back. Canonical markup serializes to
//! itself, so JS checks `got === markup`; Rust predicts the sum of the lengths and the child count from the generating rule.
//! replace: SCALE*1000 replacements of the children of 20 attached elements with `<i>k</i>` (2 nodes). A Rust model of the 20 contents
//! predicts the final `innerHTML` of each and the number of children; the native document node count must match the arena growth.
//! deep: SCALE*10 rounds. Each builds a fragment 400 levels deep, sets it, reads its serialization length and clears it.
//!
//! Prints time, CPU and peak RSS. `tools/js/fragment_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use std::time::Instant;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {src}")),
    }
}

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// Canonical markup of round `r`: it serializes to itself.
fn markup(r: usize) -> String {
    let mut s = String::new();
    for i in 0..20 {
        s += &format!("<p class=\"c{} d\" data-i=\"{i}\">t{r} &amp; {i}&nbsp;&lt;<b>x</b><br><img src=\"u{i}.png\"><!--c{i}--></p>", (r + i) % 7);
    }
    s += &format!("<script>if (a{r} < b && c) {{ x(\"&amp;\") }}</script><svg viewBox=\"0 0 {r} 1\"><circle r=\"{r}\"></circle></svg>");
    s
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    run(&mut b, "document.body.appendChild(document.createElement('hr')).remove(); 0");
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let (cpu0, nodes0) = (cpu_ms(), b.stats().nodes);
    let t = Instant::now();
    let mut ops = 0usize;
    match name {
        "parse" => {
            let rounds = 50 * scale;
            run(&mut b, "var sum = 0, kids = 0; function round(r, m) { var d = document.createElement('div'); d.innerHTML = m; var got = d.innerHTML; if (got !== m) throw new Error('round trip ' + r); sum += got.length; kids += d.childNodes.length; d = null; }");
            let (mut sum, mut kids) = (0usize, 0usize);
            for r in 0..rounds {
                let m = markup(r);
                sum += m.len();
                kids += 22;
                run(&mut b, &format!("round({r}, {m:?})"));
                ops += 1;
                if r % 100 == 99 {
                    run(&mut b, "0");
                    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
                }
            }
            expect("parse sum", run(&mut b, "sum"), sum.to_string());
            expect("parse kids", run(&mut b, "kids"), kids.to_string());
        }
        "replace" => {
            let n = 1000 * scale;
            run(&mut b, "var els = []; for (var i = 0; i < 20; i++) { var e = document.createElement('div'); e.id = 'e' + i; document.body.appendChild(e); els.push(e) } var e = null; var held = els[0].appendChild(document.createElement('s'));
                function chunk(s, c) { for (var k = s; k < s + c; k++) els[k % 20].innerHTML = '<i>' + k + '</i>'; }");
            let mut model = vec![String::new(); 20];
            for s in (0..n).step_by(100) {
                let c = 100.min(n - s);
                run(&mut b, &format!("chunk({s}, {c})"));
                for k in s..s + c {
                    model[k % 20] = format!("<i>{k}</i>");
                    ops += 1;
                }
            }
            expect("replace contents", run(&mut b, "els.map(function (e) { return e.innerHTML + '#' + e.childNodes.length }).join('|')"), model.iter().map(|m| format!("{m}#1")).collect::<Vec<_>>().join("|"));
            expect("replace held", run(&mut b, "held.tagName + held.parentNode"), "Snull".into());
            // Each replacement makes 2 nodes (the arena never frees); plus the 20 divs, `held` and the `hr`.
            expect("replace nodes", (b.stats().nodes - nodes0).to_string(), (2 * n + 20 + 1).to_string());
        }
        "deep" => {
            let rounds = 10 * scale;
            run(&mut b, "var sum = 0; var host = document.body.appendChild(document.createElement('div')); function round(r) { var m = ''; for (var i = 0; i < 400; i++) m += '<div>'; m += 'x' + r; for (var i = 0; i < 400; i++) m += '</div>'; host.innerHTML = m; var s = host.innerHTML; if (s !== m) throw new Error('deep ' + r); sum += s.length; host.innerHTML = ''; }");
            let mut sum = 0usize;
            for r in 0..rounds {
                sum += 400 * 5 + 400 * 6 + 1 + r.to_string().len();
                run(&mut b, &format!("round({r})"));
                ops += 1;
                if r % 20 == 19 {
                    run(&mut b, "0");
                    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
                }
            }
            expect("deep sum", run(&mut b, "sum"), sum.to_string());
            expect("deep nodes", (b.stats().nodes - nodes0).to_string(), (rounds * 401 + 1).to_string());
        }
        _ => die(format!("unknown case {name}")),
    }
    let el = t.elapsed();
    println!("case={name} scale={scale} ops={ops} wall={:.1}ms cpu={}ms peak_rss={}MiB nodes={} wrappers={} checksum=ok", el.as_secs_f64() * 1e3, cpu_ms() - cpu0, status_kib("VmHWM:") / 1024, b.stats().nodes, b.stats().wrappers);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: fragment_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("bad scale".into()));
    let case = a[1].clone();
    // Debug builds need a large stack for the interpreter, release builds 8 MiB.
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let h = std::thread::Builder::new().stack_size(stack).spawn(move || exec(&case, scale)).unwrap();
    if h.join().is_err() {
        std::process::exit(1);
    }
}

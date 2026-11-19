//! Load test for the CSSOM objects (`browser_cssom.rs`, `browser_computed.rs`): rule lists, sheets, media lists, adopted
//! sheets and `getComputedStyle`. One case at one scale per process. It makes the cost rows that `design-host-cssom.md` asks for.
//!
//!   cssom_load CASE SCALE      CASE: rules | sheets | adopted | computed      SCALE: 1, 10, 100
//!
//! rules: SCALE*100 rounds on one `<style>` sheet: insert 10 rules at index 0 (`insertRule`), read `cssRules.length`, the
//! `selectorText` and the `cssText` of each rule, then `deleteRule` all of them. sheets: SCALE*1000 rounds that add a `<style>`
//! element, read `document.styleSheets` (length, `ownerNode`, `cssRules.length`) and remove the element. adopted: SCALE*1000
//! rounds that make a `CSSStyleSheet` with `replaceSync` (2 style rules and one `@media print` rule), adopt it, read the rule
//! list and the media list, and drop it. computed: SCALE*10000 `getComputedStyle` calls over 50 elements, one read of
//! `margin-top` and `display` of each object. Each case runs in scripts of 100 steps with a VM collect between them. The
//! checksum is a sum that the script and a model in Rust compute separately (a model written from the CSSOM rules, not a copy
//! of the engine). Prints time, CPU, peak RSS and what the realm holds.

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

/// CPU time of this process in milliseconds (user + system), from /proc/self/stat (100 ticks per second).
fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
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

const CHUNK: usize = 100;

/// 50 elements with 3 classes. The sheet gives `margin-top` 1px, 2px or 3px by class.
fn page() -> String {
    let els: String = (0..50).map(|i| format!("<i id=e{i} class=c{}></i>", i % 3)).collect();
    format!("<!doctype html><head><style>.c0{{margin-top:1px}}.c1{{margin-top:2px}}.c2{{margin-top:3px}}</style></head><body>{els}</body>")
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(&page()), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    let (mut gc_ms, mut freed) = (0.0, 0);
    let mut collect = |b: &mut Browser| {
        run(b, "0"); // the interpreter keeps the last script value until the next script
        let t = Instant::now();
        freed += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    run(&mut b, "var sum = 0; 0");
    collect(&mut b);
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let (rounds, want): (usize, usize) = match name {
        "rules" => {
            // The sheet is a `<style>` with one rule that stays. Each round: 10 rules `.r0` .. `.r9`, each inserted at 0.
            run(&mut b, "var st = document.createElement('style'); st.textContent = '.keep { top: 0px }'; document.head.appendChild(st); var sh = st.sheet; 0");
            run(&mut b, "function round() { for (var i = 0; i < 10; i++) sh.insertRule('.r' + i + ' { top: 1px }', 0);
                var l = sh.cssRules; sum += l.length; for (var j = 0; j < 10; j++) sum += sh.cssRules[j].selectorText.length + sh.cssRules[j].cssText.length;
                for (var k = 0; k < 10; k++) sh.deleteRule(0); sum += sh.cssRules.length }");
            let rounds = 100 * scale;
            for _ in 0..rounds / CHUNK {
                run(&mut b, &format!("for (var n = 0; n < {CHUNK}; n++) round()"));
                collect(&mut b);
            }
            // length 11, then per rule `.rN` (3) and `.rN { top: 1px; }` (17), then the one rule that is left.
            (rounds, rounds * (11 + 10 * (3 + 17) + 1))
        }
        "sheets" => {
            // The page has one sheet. Each round adds a second one with 2 rules, reads the list and removes it.
            run(&mut b, "function round() { var el = document.createElement('style'); el.textContent = '.a { top: 1px } .b { top: 2px }'; document.head.appendChild(el);
                var ss = document.styleSheets; sum += ss.length; var s = ss[ss.length - 1]; sum += s.cssRules.length + (s.ownerNode === el ? 1 : 0) + (ss[0].cssRules.length);
                el.remove(); sum += document.styleSheets.length }");
            let rounds = 1000 * scale;
            for _ in 0..rounds / CHUNK {
                run(&mut b, &format!("for (var n = 0; n < {CHUNK}; n++) round()"));
                collect(&mut b);
            }
            // 2 sheets, then 2 rules + owner + 3 rules of the first sheet, then 1 sheet.
            (rounds, rounds * (2 + 2 + 1 + 3 + 1))
        }
        "adopted" => {
            run(&mut b, "function round() { var s = new CSSStyleSheet(); s.replaceSync('.a { top: 1px } .b { top: 2px } @media print { .c { top: 3px } }');
                document.adoptedStyleSheets = [s]; sum += document.adoptedStyleSheets.length + s.cssRules.length + s.cssRules[2].media.mediaText.length + (document.adoptedStyleSheets[0] === s ? 1 : 0);
                document.adoptedStyleSheets = []; sum += document.adoptedStyleSheets.length }");
            let rounds = 1000 * scale;
            for _ in 0..rounds / CHUNK {
                run(&mut b, &format!("for (var n = 0; n < {CHUNK}; n++) round()"));
                collect(&mut b);
            }
            // 1 adopted, 3 rules, `print` (5), same object (1), then none.
            (rounds, rounds * (1 + 3 + 5 + 1))
        }
        "computed" => {
            run(&mut b, "var els = []; for (var i = 0; i < 50; i++) els.push(document.getElementById('e' + i));
                function block(s, c) { for (var i = s; i < s + c; i++) { var cs = getComputedStyle(els[i % 50]); sum += parseInt(cs.marginTop) + (cs.display === 'inline' ? 1000 : 0) } }");
            let n = 10_000 * scale;
            for start in (0..n).step_by(CHUNK * 10) {
                run(&mut b, &format!("block({start}, {})", CHUNK * 10));
                collect(&mut b);
            }
            // The element `i % 50` has class `c{(i % 50) % 3}`, so its margin is `(i % 50) % 3 + 1` px, and it is inline.
            (n, (0..n).map(|i| (i % 50) % 3 + 1 + 1000).sum())
        }
        _ => die(format!("unknown case {name}")),
    };
    expect(name, run(&mut b, "sum"), want.to_string());
    let (run_ms, cpu) = (t.elapsed().as_secs_f64() * 1e3, cpu_ms() - cpu0);
    let st = b.stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok rounds={rounds} run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB | wrappers={} nodes={} | gc={gc_ms:.1}ms freed_objs={freed}",
        status_kib("VmHWM:") as f64 / 1024.0,
        st.wrappers,
        st.nodes,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: cssom_load rules|sheets|adopted|computed 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // The evaluator recurses: 8 MiB is the release VM stack of this project, 64 MiB the debug one.
    let mib = if cfg!(debug_assertions) { 64 } else { 8 };
    std::thread::Builder::new().stack_size(mib << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}

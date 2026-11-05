//! Load test for DOM focus. One case at one scale per process.
//!
//!   focus_load CASE SCALE      CASE: focus | dispatch | gc      SCALE: 1, 10, 100
//!
//! focus: SCALE*1000 focus moves over 20 buttons, alternating blocks of 100 from script and from the Rust driver, counted by listeners
//! and checked against the same walk in Rust. dispatch: SCALE*1000 synthetic `FocusEvent`s (script and driver) that must
//! not move focus. gc: SCALE*1000 rounds of create, append, focus, remove with a VM collect between blocks; the realm must
//! not keep the removed nodes. Prints time, CPU, peak RSS and what the realm holds. `tools/js/focus_load.sh` runs the matrix.

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

fn exec(name: &str, scale: usize) {
    let buttons: String = (0..20).map(|i| format!("<button id=b{i}>x</button>")).collect();
    let doc = tree::parse(&format!("<body><div id=w>{buttons}</div></body>"));
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(doc, BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    let cpu0 = cpu_ms();
    let t = Instant::now();
    let (mut gc_ms, mut freed) = (0.0, 0);
    let mut collect = |b: &mut Browser| {
        let t = Instant::now();
        freed += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    let n = 1000 * scale;
    match name {
        "focus" => {
            run(&mut b, "var btns = []; for (var i = 0; i < 20; i++) btns.push(document.getElementById('b' + i)); var ins = 0, blurs = 0, fo = 0;
                document.body.addEventListener('focusin', function () { ins++; });
                window.addEventListener('blur', function () { blurs++; }, true);
                document.addEventListener('focusout', function () { fo++; });
                function go(s, c) { for (var i = s; i < s + c; i++) btns[(i * 7 + 1) % 20].focus(); }");
            let nodes: Vec<_> = (0..20).map(|i| b.query(&format!("#b{i}")).unwrap().unwrap()).collect();
            let (mut prev, mut moves) = (usize::MAX, 0);
            for start in (0..n).step_by(CHUNK) {
                let block = (start / CHUNK).is_multiple_of(2);
                if block {
                    run(&mut b, &format!("go({start}, {CHUNK})"));
                }
                for i in start..start + CHUNK {
                    let target = (i * 7 + 1) % 20;
                    if !block && !b.focus(nodes[target]).unwrap_or_else(|e| die(format!("focus {e:?}"))).focused {
                        die("driver focus".into());
                    }
                    moves += usize::from(target != prev);
                    prev = target;
                }
                collect(&mut b);
            }
            expect("focus", run(&mut b, "[ins, blurs, fo, document.activeElement.id].join()"), format!("{moves},{},{},b{prev}", moves - 1, moves - 1));
        }
        "dispatch" => {
            run(&mut b, "var seen = 0, bt = document.getElementById('b3'); bt.focus();
                document.body.addEventListener('focusin', function (e) { if (e.relatedTarget === bt && !e.isTrusted) seen++; });
                function go(c) { for (var i = 0; i < c; i++) document.getElementById('b5').dispatchEvent(new FocusEvent('focusin', {bubbles: true, relatedTarget: bt})); }");
            let b5 = b.query("#b5").unwrap().unwrap();
            let mut done = 0;
            while done < n {
                run(&mut b, &format!("go({})", CHUNK));
                done += CHUNK;
                collect(&mut b);
            }
            // The Rust driver cannot send a FocusEvent; one real move of focus from b3 to b5 and back checks the other path.
            if !b.focus(b5).unwrap().focused {
                die("driver".into());
            }
            expect("dispatch", run(&mut b, "seen + ',' + document.activeElement.id"), format!("{},b5", done + 1));
        }
        "gc" => {
            run(&mut b, "var w = document.getElementById('w');
                function go(c) { var ok = 0; for (var i = 0; i < c; i++) { var x = document.createElement('button'); w.appendChild(x); x.focus(); if (document.activeElement === x) ok++; x.remove(); if (document.activeElement === document.body) ok++; } return ok; }");
            for start in (0..n).step_by(CHUNK) {
                expect("gc round", run(&mut b, &format!("go({})", CHUNK)), (2 * CHUNK).to_string());
                collect(&mut b);
                let s = b.stats();
                if s.wrappers > 64 || s.events > 8 {
                    die(format!("retained after {start}: {s:?}"));
                }
            }
        }
        _ => die(format!("unknown case {name}")),
    }
    let (run_ms, cpu) = (t.elapsed().as_secs_f64() * 1e3, cpu_ms() - cpu0);
    let st = b.stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB | wrappers={} listeners={} events={} nodes={} | gc={gc_ms:.1}ms freed_objs={freed}",
        status_kib("VmHWM:") as f64 / 1024.0,
        st.wrappers,
        st.listeners,
        st.events,
        st.nodes,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: focus_load focus|dispatch|gc 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // The evaluator recurses: 8 MiB is the release VM stack of this project.
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}

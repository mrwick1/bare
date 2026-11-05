//! Load test for form controls and the driver's click activation. One case at one scale per process.
//!
//!   form_load CASE SCALE      CASE: submit | gc | gates      SCALE: 1, 10, 100
//!
//! The stack of the VM thread is 8 MiB (release) or `FORM_LOAD_STACK_MIB` (64 for a debug build).
//! submit: SCALE*1000 driver clicks over 20 forms with a small repeating pattern of state (enabled, disabled, `type=button`,
//! owner by `form` attribute, a click listener that cancels), changed between blocks by script. A Rust model of the same rules
//! gives the expected number of submit events per form and per submitter; script counters must match exactly.
//! gc: SCALE*1000 rounds of build a form and a button in script, click it from the driver, remove it; a VM collect between
//! blocks. The realm must not keep the removed forms, buttons or events.
//! gates: SCALE*1000 clicks at a form that needs validation (no submit event, `Unsupported`), at one whose submit is not
//! canceled (one event, `Unsupported`) and at one whose submit is canceled (ok). Counts are checked against the model, and
//! the realm stays usable. Prints time, CPU, peak RSS and what the realm holds. `tools/js/form_load.sh` runs the matrix.

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
const FORMS: usize = 20;

/// What control `k` of a form is, by the rules, for a pattern `state` (0..4) of the block.
#[derive(Clone, Copy)]
enum Kind {
    Enabled,
    Disabled,
    Plain,
    Elsewhere,
}

/// The pattern of block `blk` for button `k`: a small, fixed function the script applies with the same arithmetic.
fn kind(blk: usize, k: usize) -> Kind {
    match (blk + k) % 4 {
        0 => Kind::Enabled,
        1 => Kind::Disabled,
        2 => Kind::Plain,
        _ => Kind::Elsewhere,
    }
}

fn exec(name: &str, scale: usize) {
    let forms: String = (0..FORMS).map(|i| format!("<form id=f{i} novalidate><button id=b{i}>x</button></form>")).collect();
    let doc = tree::parse(&format!("<body>{forms}<div id=w></div></body>"));
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
        "submit" => {
            // Every form counts submits (and the submitter id it saw); a click listener on 3 of 20 buttons cancels the click.
            run(&mut b, "var subs = [], who = [], clicks = 0; for (var i = 0; i < 20; i++) { subs.push(0); who.push('') }
                var btns = [], forms = [];
                for (var i = 0; i < 20; i++) { btns.push(document.getElementById('b' + i)); forms.push(document.getElementById('f' + i)) }
                forms.forEach(function (f, i) { f.addEventListener('submit', function (e) { e.preventDefault(); subs[i]++; who[i] = e.submitter.id }) });
                document.addEventListener('click', function () { clicks++ }, true);
                [2, 9, 16].forEach(function (i) { btns[i].addEventListener('click', function (e) { e.preventDefault() }) });
                function setup(blk) { for (var k = 0; k < 20; k++) { var m = (blk + k) % 4; var b = btns[k];
                    b.removeAttribute('form'); b.removeAttribute('disabled'); b.type = 'submit';
                    if (m === 1) b.disabled = true; else if (m === 2) b.type = 'button'; else if (m === 3) b.setAttribute('form', 'f' + ((k + 1) % 20)) } }");
            let nodes: Vec<_> = (0..FORMS).map(|i| b.query(&format!("#b{i}")).unwrap().unwrap()).collect();
            let (mut want_subs, mut want_who, mut want_clicks) = (vec![0usize; FORMS], vec![String::new(); FORMS], 0usize);
            let canceler = |k: usize| k == 2 || k == 9 || k == 16;
            for start in (0..n).step_by(CHUNK) {
                let blk = start / CHUNK;
                run(&mut b, &format!("setup({blk})"));
                for i in start..start + CHUNK {
                    let k = (i * 7 + 3) % FORMS;
                    let r = b.click(nodes[k]).unwrap_or_else(|e| die(format!("click {k}: {e:?}")));
                    let kd = kind(blk, k);
                    // A disabled control gets no click; the others do. The click is canceled when its own listener says so.
                    if !matches!(kd, Kind::Disabled) {
                        want_clicks += 1;
                    }
                    let cancels = !matches!(kd, Kind::Disabled) && canceler(k);
                    if r.not_canceled == cancels {
                        die(format!("click {i} k={k}: not_canceled={} want {}", r.not_canceled, !cancels));
                    }
                    if !cancels {
                        match kd {
                            Kind::Enabled => (want_subs[k], want_who[k]) = (want_subs[k] + 1, format!("b{k}")),
                            Kind::Elsewhere => {
                                let o = (k + 1) % FORMS;
                                (want_subs[o], want_who[o]) = (want_subs[o] + 1, format!("b{k}"));
                            }
                            Kind::Disabled | Kind::Plain => {}
                        }
                    }
                }
                collect(&mut b);
            }
            expect("submits", run(&mut b, "subs.join()"), want_subs.iter().map(usize::to_string).collect::<Vec<_>>().join(","));
            expect("submitters", run(&mut b, "who.join()"), want_who.join(","));
            expect("clicks", run(&mut b, "clicks"), want_clicks.to_string());
            let s = b.stats();
            if s.events > 8 {
                die(format!("events retained: {s:?}"));
            }
        }
        "gc" => {
            run(&mut b, "var w = document.getElementById('w'), ok = 0; 'ready'");
            // The click comes from the driver, so each block is: build CHUNK forms (script), click each button (driver), remove them.
            run(&mut b, "function build(s, c) { for (var i = s; i < s + c; i++) { var f = document.createElement('form'); f.noValidate = true; var x = document.createElement('button'); x.id = 'gx' + i; x.name = 'n' + i; f.appendChild(x); w.appendChild(f);
                (function (f, x, i) { f.addEventListener('submit', function (e) { e.preventDefault(); ok += (e.submitter === x && x.form === f && x.name === 'n' + i) ? 1 : 0 }) })(f, x, i) } }
                function drop() { while (w.firstChild) w.removeChild(w.firstChild) }");
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("build({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    let x = b.query(&format!("#gx{i}")).unwrap().unwrap_or_else(|| die("no button".into()));
                    let r = b.click(x).unwrap_or_else(|e| die(format!("click {e:?}")));
                    if !r.not_canceled || !r.reports.is_empty() {
                        die(format!("click {i}: {r:?}"));
                    }
                }
                run(&mut b, "drop()");
                collect(&mut b);
                let s = b.stats();
                if s.wrappers > 64 || s.events > 8 || s.listeners > 32 {
                    die(format!("retained after {start}: {s:?}"));
                }
            }
            expect("gc ok", run(&mut b, "ok"), n.to_string());
        }
        "gates" => {
            run(&mut b, "var w = document.getElementById('w'); w.innerHTML = '<form id=v><button id=gv>v</button></form><form id=u><button id=gu formnovalidate>u</button></form><form id=c><button id=gc formnovalidate>c</button></form>';
                var seen = { v: 0, u: 0, c: 0 };
                ['v', 'u', 'c'].forEach(function (id) { document.getElementById(id).addEventListener('submit', function (e) { seen[id]++; if (id === 'c') e.preventDefault() }) });");
            let (gv, gu, gc) = (b.query("#gv").unwrap().unwrap(), b.query("#gu").unwrap().unwrap(), b.query("#gc").unwrap().unwrap());
            for start in (0..n).step_by(CHUNK) {
                for _ in start..start + CHUNK {
                    match b.click(gv) {
                        Err(Error::Unsupported(u)) if u.what.contains("validation") => {}
                        o => die(format!("validation gate: {o:?}")),
                    }
                    match b.click(gu) {
                        Err(Error::Unsupported(u)) if u.what.contains("navigation") => {}
                        o => die(format!("navigation gate: {o:?}")),
                    }
                    if !b.click(gc).unwrap_or_else(|e| die(format!("canceled submit: {e:?}"))).not_canceled {
                        die("click of a canceled submit".into());
                    }
                }
                collect(&mut b);
            }
            expect("gates", run(&mut b, "[seen.v, seen.u, seen.c].join()"), format!("0,{n},{n}"));
            // The realm is still usable after thousands of gate errors.
            expect("alive", run(&mut b, "1 + 1"), "2".into());
            if b.stats().events > 8 {
                die(format!("events retained: {:?}", b.stats()));
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
        die("usage: form_load submit|gc|gates 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    let mib: usize = std::env::var("FORM_LOAD_STACK_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    // The evaluator recurses: 8 MiB is the release VM stack of this project, 64 MiB the debug one.
    std::thread::Builder::new().stack_size(mib << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}

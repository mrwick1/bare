//! Load test for `select`, `option` and the option lists. One case at one size per process.
//!
//!   select_load CASE N      CASE: value | react | churn      N: options in the select (10, 1000, 10000)
//!
//! value: a select with N options (`value="vK"`). `select.value = ...` is set 1,000 times (the targets cycle over the options, every
//! 7th target has no option), and each set is checked with `selectedIndex` and `value`. react: React DOM's `updateOptions` loop
//! (one write of `selected` and `defaultSelected` on the match), 100 times, on a single select and on a `multiple` one.
//! churn: 20 rounds of build a select with N options by `new Option` and `add`, shrink it with `options.length`, drop it; a collect
//! after each round. The realm must keep no `Opt` state of dropped selects in its wrappers. Prints time, CPU and peak RSS.
//! `tools/js/select_load.sh` runs the matrix.

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

fn expect(what: &str, got: String, want: &str) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

fn exec(name: &str, n: usize) {
    let opts: String = (0..n).map(|i| format!("<option value=v{i}>Option {i}</option>")).collect();
    let doc = tree::parse(&format!("<body><select id=s>{opts}</select><select id=m multiple>{opts}</select></body>"));
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let limits = BrowserLimits { max_results: 1 << 20, max_nodes: 1 << 22, ..Default::default() };
    let mut b = Browser::new(doc, limits, eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let (mut gc_ms, mut freed) = (0.0, 0);
    match name {
        "value" => {
            run(&mut b, &format!("var s = document.getElementById('s'), n = {n}, bad = 0, miss = 0;
                for (var k = 0; k < 1000; k++) {{ var i = (k * 7919) % n, none = k % 7 === 6;
                    s.value = none ? 'zz' + k : 'v' + i;
                    if (none) {{ miss++; if (s.selectedIndex !== -1 || s.value !== '') bad++ }}
                    else if (s.selectedIndex !== i || s.value !== 'v' + i) bad++ }}"));
            expect("bad", run(&mut b, "bad"), "0");
            expect("miss", run(&mut b, "miss"), "142");
        }
        "react" => {
            run(&mut b, &format!("function updateOptions(node, multiple, propValue, setDefaultSelected) {{
                var options = node.options;
                if (multiple) {{
                    var selectedValue = {{}};
                    for (var i = 0; i < propValue.length; i++) selectedValue['$' + propValue[i]] = true;
                    for (var j = 0; j < options.length; j++) {{
                        var selected = selectedValue.hasOwnProperty('$' + options[j].value);
                        if (options[j].selected !== selected) options[j].selected = selected;
                        if (selected && setDefaultSelected) options[j].defaultSelected = true;
                    }}
                }} else {{
                    var sv = '' + propValue, def = null;
                    for (var k = 0; k < options.length; k++) {{
                        if (options[k].value === sv) {{ options[k].selected = true; if (setDefaultSelected) options[k].defaultSelected = true; return }}
                        if (def === null && !options[k].disabled) def = options[k];
                    }}
                    if (def !== null) def.selected = true;
                }}
            }}
            var s = document.getElementById('s'), m = document.getElementById('m'), n = {n}, bad = 0;
            for (var k = 0; k < 100; k++) {{ var i = (k * 7919) % n;
                updateOptions(s, false, 'v' + i, false); if (s.selectedIndex !== i) bad++;
                updateOptions(m, true, ['v' + i, 'v' + ((i + 1) % n)], false);
                if (m.selectedOptions.length !== (n > 1 ? 2 : 1)) bad++ }}"));
            expect("bad", run(&mut b, "bad"), "0");
        }
        "churn" => {
            for round in 0..20 {
                let got = run(&mut b, &format!("var s = document.createElement('select'); for (var i = 0; i < {n}; i++) s.add(new Option('o' + i, 'v' + i));
                    s.value = 'v' + {}; var r = s.selectedIndex; s.options.length = Math.max(1, {n} >> 1); r + ',' + s.options.length + ',' + s.selectedOptions.length", round % n));
                let keep = if round % n < (n >> 1).max(1) { round % n } else { 0 };
                expect("churn", got, &format!("{},{},1", round % n, (n >> 1).max(1)));
                let _ = keep;
                run(&mut b, "s = null");
                let t = Instant::now();
                freed += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
                gc_ms += t.elapsed().as_secs_f64() * 1e3;
            }
        }
        _ => die(format!("unknown case {name}")),
    }
    let (run_ms, cpu) = (t.elapsed().as_secs_f64() * 1e3, cpu_ms() - cpu0);
    let st = b.stats();
    println!(
        "{name:6} n={n:<6} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB | wrappers={} nodes={} | gc={gc_ms:.1}ms freed_objs={freed}",
        status_kib("VmHWM:") as f64 / 1024.0,
        st.wrappers,
        st.nodes,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: select_load value|react|churn N".into());
    }
    let n: usize = a[2].parse().unwrap_or_else(|_| die("n".into()));
    let name = a[1].clone();
    let mib: usize = std::env::var("SELECT_LOAD_STACK_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    std::thread::Builder::new().stack_size(mib << 20).spawn(move || exec(&name, n)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}

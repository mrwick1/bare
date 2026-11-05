//! Load test for `<input>` selection. One case at one scale per process, on a virtual clock: nothing sleeps.
//!
//!   selection_load CASE SCALE      CASE: short | long      SCALE: 1, 10, 100
//!
//! SCALE*1000 operations over 64 inputs: `setSelectionRange`, the `selectionStart`, `selectionEnd` and
//! `selectionDirection` setters, `select()` and `value =`. Arguments mix fractions, negatives, NaN, Infinity, 2**32+3,
//! strings and indexes near the ends. `short` values are 0 to 9 UTF-16 units with surrogate pairs. `long` values are
//! about 4000 units. The checksum is a rolling hash of the selection that JS reads after every operation, and of the
//! target of every `select` event in order. A separate Rust model of the HTML algorithm (it shares no code with the
//! host) gives the expected values, the expected number of `select` tasks, and the native state of each input,
//! which is compared with `Browser::input_selection` after every batch (no JS read). Batches of 500 run through
//! a pump of at most 256 tasks and a `collect`. At the end no task, event or wrapper of a batch is left.

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
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {}", &src[..src.len().min(200)])),
    }
}

const BATCH: usize = 500;
const PUMP: usize = 256;
const INPUTS: usize = 64;
const P: u64 = 1000003;

/// The model: the selection as `(start, end, dir)`, dir 0 none, 1 forward, 2 backward.
#[derive(Clone, Copy)]
struct M {
    val: usize,
    len: u32,
    s: u32,
    e: u32,
    d: u8,
}

impl M {
    /// "Set the selection range". True when the range or the direction changed (a `select` task).
    fn set(&mut self, start: u32, end: u32, d: u8) -> bool {
        let end = end.min(self.len);
        let start = start.min(end);
        let changed = (start, end, d) != (self.s, self.e, self.d);
        (self.s, self.e, self.d) = (start, end, d);
        changed
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        die("usage: selection_load CASE SCALE".into());
    }
    let (name, scale) = (args[1].clone(), args[2].parse::<usize>().unwrap_or_else(|_| die("bad scale".into())));
    let t = std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(&name, scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}

fn exec(name: &str, scale: usize) {
    // value sources: (JS literal, UTF-16 length)
    let vals: Vec<(String, u32)> = match name {
        "short" => ["", "a", "abcdef", "\\u{1F600}x\\u{1F600}", "\\u00e9\\u20ac", "\\u{1F600}", "abcdefghi", "x"].iter().map(|s| (s.to_string(), 0)).collect(),
        "long" => (0..6)
            .map(|k| {
                let unit = ["abcd", "\\u{1F600}b", "\\u00e9\\u20acc", "wxyz1", "\\u{1F600}\\u{1F601}", "q"][k];
                (unit.to_string(), 0)
            })
            .collect(),
        _ => die(format!("unknown case {name}")),
    };
    // JS text of each value and its length by an independent count (decode the escapes here)
    let units = |lit: &str| -> (String, u32) {
        let (mut out, mut chars) = (String::new(), lit.chars().peekable());
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek() == Some(&'u') {
                chars.next();
                let hex: String = if chars.peek() == Some(&'{') {
                    chars.next();
                    chars.by_ref().take_while(|c| *c != '}').collect()
                } else {
                    chars.by_ref().take(4).collect()
                };
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
            } else {
                out.push(c);
            }
        }
        let n = out.encode_utf16().count() as u32;
        (out, n)
    };
    let mut js_vals = Vec::new();
    let mut lens = Vec::new();
    for (lit, _) in &vals {
        let reps = if name == "long" { 1000 } else { 1 };
        let (_, n1) = units(lit);
        js_vals.push(format!("'{}'", lit.repeat(reps)));
        lens.push(n1 * reps as u32);
    }
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    run(
        &mut b,
        &format!(
            "var vals = [{}], D = {{none: 0, forward: 1, backward: 2}}, h = 0, h2 = 0, ev = 0, inp = [];
            for (var i = 0; i < {INPUTS}; i++) {{ var e = document.createElement('input'); e.i = i; document.body.appendChild(e); inp.push(e) }}
            document.addEventListener('select', function (x) {{ ev++; h2 = (h2 * 31 + x.target.i) % {P} }});
            function op(i, k, a, b, d, v) {{
              var e = inp[i];
              if (k === 0) e.setSelectionRange(a, b, d); else if (k === 1) e.selectionStart = a; else if (k === 2) e.selectionEnd = b;
              else if (k === 3) e.select(); else if (k === 4) e.value = vals[v]; else e.selectionDirection = d;
              h = (h * 31 + e.selectionStart * 7 + e.selectionEnd * 3 + D[e.selectionDirection]) % {P};
            }}",
            js_vals.join(", ")
        ),
    );
    let nodes: Vec<_> = (0..INPUTS).map(|i| b.query(&format!("body > input:nth-child({})", i + 1)).unwrap_or_else(|e| die(format!("query {e:?}"))).unwrap_or_else(|| die("no input".into()))).collect();
    // every input starts with the empty value: that is `vals[0]` in `short`, and none of the `long` values
    let mut model = vec![M { val: if name == "short" { 0 } else { usize::MAX }, len: 0, s: 0, e: 0, d: 0 }; INPUTS];
    let fixed: [(&str, u32); 10] = [("0", 0), ("1", 1), ("3", 3), ("7", 7), ("1.9", 1), ("-1", u32::MAX), ("NaN", 0), ("Infinity", 0), ("4294967299", 3), ("'2'", 2)];
    let dirs: [(&str, u8); 5] = [("'forward'", 1), ("'backward'", 2), ("'none'", 0), ("'x'", 0), ("undefined", 0)];
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 33) as usize
    };
    let n = 1000 * scale;
    let (mut exp_h, mut exp_h2, mut exp_ev) = (0u64, 0u64, 0u64);
    let (mut pumps, mut tasks, mut max_queue) = (0usize, 0usize, 0usize);
    let cpu0 = cpu_ms();
    let t = Instant::now();
    for base in (0..n).step_by(BATCH) {
        let mut script = String::new();
        let mut order = Vec::new();
        for _ in 0..BATCH {
            let i = next() % INPUTS;
            let k = next() % 6;
            let pick = |r: usize, len: u32, nx: usize| -> (String, u32) {
                if r.is_multiple_of(3) {
                    let (s, v) = fixed[nx % fixed.len()];
                    (s.to_string(), v)
                } else {
                    let v = (nx % (len as usize + 3)) as u32;
                    (v.to_string(), v)
                }
            };
            let (r1, r2, r3, r4) = (next(), next(), next(), next());
            let m = &mut model[i];
            let (a, av) = pick(r1, m.len, r3);
            let (bb, bv) = pick(r2, m.len, r4);
            let (ds, dv) = dirs[next() % dirs.len()];
            let v = next() % vals.len();
            let changed = match k {
                0 => m.set(av, bv, dv),
                1 => {
                    let e = m.e.max(av);
                    let d = m.d;
                    m.set(av, e, d)
                }
                2 => {
                    let (s, d) = (m.s, m.d);
                    m.set(s, bv, d)
                }
                3 => m.set(0, u32::MAX, 0),
                4 => {
                    // the value setter: a different value moves the caret to the end and clears the direction
                    if v != m.val {
                        (m.val, m.len) = (v, lens[v]);
                        (m.s, m.e, m.d) = (m.len, m.len, 0);
                    }
                    false
                }
                _ => {
                    let (s, e) = (m.s, m.e);
                    m.set(s, e, dv)
                }
            };
            exp_h = (exp_h * 31 + m.s as u64 * 7 + m.e as u64 * 3 + m.d as u64) % P;
            if changed {
                order.push(i as u64);
            }
            script.push_str(&format!("op({i}, {k}, {a}, {bb}, {ds}, {v});\n"));
        }
        run(&mut b, &script);
        // the selection that JS read matches the model, and so does the native state of each input (no JS)
        let got = run(&mut b, "h");
        if got != exp_h.to_string() {
            die(format!("batch {base}: selection hash {got} != {exp_h}"));
        }
        for (i, m) in model.iter().enumerate() {
            let want = Some((m.s, m.e, ["none", "forward", "backward"][m.d as usize]));
            if b.input_selection(nodes[i]) != want {
                die(format!("batch {base}: native state of input {i}: {:?} != {want:?}", b.input_selection(nodes[i])));
            }
        }
        exp_ev += order.len() as u64;
        for i in &order {
            exp_h2 = (exp_h2 * 31 + i) % P;
        }
        max_queue = max_queue.max(b.sched_stats().tasks);
        loop {
            let p = b.advance_time(0.0, PUMP).unwrap_or_else(|e| die(format!("pump: {e:?}")));
            pumps += 1;
            tasks += p.ran;
            b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
            if !p.capped {
                break;
            }
        }
    }
    let got = run(&mut b, "[ev, h2].join()");
    if got != format!("{exp_ev},{exp_h2}") {
        die(format!("events: {got} != {exp_ev},{exp_h2}"));
    }
    if tasks as u64 != exp_ev {
        die(format!("tasks run {tasks} != expected select tasks {exp_ev}"));
    }
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let (s, st) = (b.sched_stats(), b.stats());
    if (s.tasks, s.timers, st.events) != (0, 0, 0) {
        die(format!("not empty at the end: {s:?} events={}", st.events));
    }
    println!(
        "{name:6} x{scale:<3} checksum=ok run={:.1}ms cpu={}ms peak_rss={:.1}MiB | ops={n} select_tasks={exp_ev} pumps={pumps} max_queue={max_queue} after: tasks={} events={} wrappers={}",
        t.elapsed().as_secs_f64() * 1e3,
        cpu_ms() - cpu0,
        status_kib("VmHWM:") as f64 / 1024.0,
        s.tasks,
        st.events,
        st.wrappers,
    );
}

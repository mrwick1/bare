//! Load test for `oninput` / `onchange`. One scale per process.
//!
//!   handler_load SCALE      SCALE: 1, 10, 100
//!
//! SCALE*1000 operations over 64 inputs, `document` and `window`: set a handler (function, non-callable object, null, a
//! primitive), `addEventListener` and `removeEventListener` of the same 16 functions, and driver `type_text` and
//! `commit_change` events that bubble. A Rust model of the HTML and DOM algorithms (it shares no code with the host)
//! keeps each target's ordered slot list. Events are `dispatchEvent` calls, except the last operation of each batch, which is a driver `type_text` or `commit_change`. It gives the expected call order, as a rolling hash of (function, receiver), and
//! the expected value of every property. Both are compared after every batch of 500 operations, then the heap is collected
//! and the number of live listener slots must equal the model's. No report may appear.

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

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {}", &src[..src.len().min(200)])),
    }
}

const BATCH: usize = 500;
const INPUTS: usize = 64;
const FNS: u64 = 16;
const P: u64 = 1000003;

/// What a property holds: a function number, the shared non-callable object.
#[derive(Clone, Copy, PartialEq)]
enum Val {
    F(u64),
    Obj,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Listener(u64),
    Handler(Val),
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 11) % n
    }
}

struct Model {
    ck: u64,
    /// `slots[input][type]`, in registration order.
    slots: Vec<[Vec<Slot>; 2]>,
    /// `doc[type]` and `win[type]`.
    doc: [Option<Val>; 2],
    win: [Option<Val>; 2],
}

impl Model {
    fn call(&mut self, f: u64, receiver: u64) {
        self.ck = (self.ck * 31 + f * 7 + receiver) % P;
    }
    fn set_slot(list: &mut Vec<Slot>, v: Option<Val>) {
        let at = list.iter().position(|s| matches!(s, Slot::Handler(_)));
        match (at, v) {
            (Some(i), Some(v)) => list[i] = Slot::Handler(v),
            (Some(i), None) => {
                list.remove(i);
            }
            (None, Some(v)) => list.push(Slot::Handler(v)),
            (None, None) => {}
        }
    }
    fn event(&mut self, k: usize, ty: usize) {
        for s in self.slots[k][ty].clone() {
            match s {
                Slot::Listener(f) | Slot::Handler(Val::F(f)) => self.call(f, 3),
                Slot::Handler(Val::Obj) => {}
            }
        }
        for (v, r) in [(self.doc[ty], 2), (self.win[ty], 1)] {
            if let Some(Val::F(f)) = v {
                self.call(f, r);
            }
        }
    }
    fn count(&self) -> usize {
        self.slots.iter().flatten().map(Vec::len).sum::<usize>() + self.doc.iter().chain(&self.win).flatten().count()
    }
    fn describe(v: Option<Val>) -> String {
        match v {
            None => "null".into(),
            Some(Val::Obj) => "O".into(),
            Some(Val::F(f)) => format!("F{f}"),
        }
    }
    fn state(&self) -> String {
        let mut out = String::new();
        for k in 0..INPUTS {
            for ty in 0..2 {
                let h = self.slots[k][ty].iter().find_map(|s| if let Slot::Handler(v) = s { Some(*v) } else { None });
                out += &Self::describe(h);
                out.push(',');
            }
        }
        for v in self.doc.iter().chain(&self.win) {
            out += &Self::describe(*v);
            out.push(',');
        }
        out
    }
}

fn flush(b: &mut Browser, pending: &mut String) {
    if !pending.is_empty() {
        run(b, pending);
        pending.clear();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        die("usage: handler_load SCALE".into());
    }
    let scale = args[1].parse::<usize>().unwrap_or_else(|_| die("bad scale".into()));
    let t = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || exec(scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}

fn exec(scale: usize) {
    let mut page = String::from("<!doctype html><body>");
    for i in 0..INPUTS {
        page += &format!("<input id=i{i} type=text>");
    }
    let mut b = Browser::new(tree::parse(&page), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    run(&mut b, &format!(
        "var ck = 0, I = [], F = [], OBJ = {{}}; for (var n = 0; n < {INPUTS}; n++) I.push(document.getElementById('i' + n));
         function mk(n) {{ return function (e) {{ ck = (ck * 31 + n * 7 + (this === window ? 1 : this === document ? 2 : 3)) % {P} }} }}
         for (var n = 0; n < {FNS}; n++) F.push(mk(n));
         var T = ['oninput', 'onchange'], N = ['input', 'change'];
         function desc(v) {{ if (v === null) return 'null'; if (v === OBJ) return 'O'; return 'F' + F.indexOf(v) }}
         function state() {{ var s = ''; for (var k = 0; k < {INPUTS}; k++) {{ s += desc(I[k].oninput) + ',' + desc(I[k].onchange) + ',' }} return s + desc(document.oninput) + ',' + desc(document.onchange) + ',' + desc(window.oninput) + ',' + desc(window.onchange) + ',' }}"));
    let mut m = Model { ck: 0, slots: (0..INPUTS).map(|_| [Vec::new(), Vec::new()]).collect(), doc: [None; 2], win: [None; 2] };
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let total = scale * 1000;
    let (start, mut driver_events) = (Instant::now(), 0u64);
    let mut pending = String::new();
    for op in 0..total {
        let (r, k, ty, f) = (rng.below(100), rng.below(INPUTS as u64) as usize, rng.below(2) as usize, rng.below(FNS));
        let v = rng.below(10);
        let (val, js_val) = match v {
            0..=6 => (Some(Val::F(f)), format!("F[{f}]")),
            7 => (Some(Val::Obj), "OBJ".to_string()),
            8 => (None, "null".to_string()),
            _ => (None, "1".to_string()),
        };
        match r {
            0..=29 => {
                pending += &format!("I[{k}].{} = {js_val};", ["oninput", "onchange"][ty]);
                Model::set_slot(&mut m.slots[k][ty], val);
            }
            30..=34 => {
                pending += &format!("document.{} = {js_val};", ["oninput", "onchange"][ty]);
                m.doc[ty] = val;
            }
            35..=39 => {
                pending += &format!("window.{} = {js_val};", ["oninput", "onchange"][ty]);
                m.win[ty] = val;
            }
            40..=54 => {
                pending += &format!("I[{k}].addEventListener(N[{ty}], F[{f}]);");
                if !m.slots[k][ty].contains(&Slot::Listener(f)) {
                    m.slots[k][ty].push(Slot::Listener(f));
                }
            }
            55..=59 => {
                pending += &format!("I[{k}].removeEventListener(N[{ty}], F[{f}]);");
                if let Some(i) = m.slots[k][ty].iter().position(|s| *s == Slot::Listener(f)) {
                    m.slots[k][ty].remove(i);
                }
            }
            _ => {
                // The last operation of a batch goes through the driver; the others are `dispatchEvent` in the batch script
                // (a script is kept by the page, so one script per operation would reach the script cap).
                if (op + 1) % BATCH == 0 || op + 1 == total {
                    flush(&mut b, &mut pending);
                    let node = b.query(&format!("#i{k}")).unwrap().unwrap_or_else(|| die("no input".into()));
                    let d = if ty == 0 { b.type_text(node, "x") } else { b.commit_change(node) };
                    match d {
                        Ok(d) if d.reports.is_empty() => {}
                        other => die(format!("driver event failed at op {op}: {:?}", other.map(|d| d.reports.len()))),
                    }
                    driver_events += 1;
                } else {
                    pending += &format!("I[{k}].dispatchEvent(new Event(N[{ty}], {{ bubbles: true }}));");
                }
                m.event(k, ty);
            }
        }
        if (op + 1) % BATCH == 0 || op + 1 == total {
            flush(&mut b, &mut pending);
            let ck = run(&mut b, "ck");
            if ck != m.ck.to_string() {
                die(format!("call order differs at op {op}: host {ck}, model {}", m.ck));
            }
            let st = run(&mut b, "state()");
            if st != m.state() {
                die(format!("property values differ at op {op}"));
            }
            b.collect(&[]).unwrap_or_else(|e| die(format!("collect {e:?}")));
            let live = b.stats().listeners;
            if live != m.count() {
                die(format!("live listener slots {live}, model {} at op {op}", m.count()));
            }
        }
    }
    if !b.take_reports().is_empty() {
        die("unexpected listener reports".into());
    }
    println!("ok scale={scale} ops={total} driver_events={driver_events} slots={} wall={}ms rss={}MiB hwm={}MiB", m.count(), start.elapsed().as_millis(), status_kib("VmRSS:") >> 10, status_kib("VmHWM:") >> 10);
}

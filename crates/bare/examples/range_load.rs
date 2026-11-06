//! Load test for live ranges and `NodeIterator`. One case at one scale per process.
//!
//!   range_load CASE SCALE      CASE: live | iter      SCALE: 1, 10, 100
//!
//! live: a list of 1000 items and 16 ranges on it (child offsets), then SCALE*1000 random inserts and removals of an item, and
//! text edits (`insertData`, `deleteData`, `splitText` on a text with 4 ranges). A model in the script, with plain numbers and
//! the offsets rule of the standard, gives the expected points; every 500 operations all points are compared.
//! iter: SCALE*1000 items (at most 10,000 for the backward walk: the wrapper of a node that is in the tree lives until it leaves); a `NodeIterator` visits them in order and removes each item that it returned (the reference node
//! moves back each time); a second one walks the list backwards, then `textContent` clears the list under it. The counts are checked.
//! Prints time, CPU, peak RSS and what the realm holds. `tools/js/range_load.sh` runs the matrix under caps.

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

const BATCH: usize = 500;

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {}", &src[..src.len().min(200)])),
    }
}

const LIVE: &str = "
var u = document.getElementById('u'), seed = 12345, K = 16, M = [], R = [], L = [], bad = 0;
function rnd(n) { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n }
for (var i = 0; i < 1000; i++) { var e = document.createElement('li'); u.appendChild(e); L.push(e) }
for (var i = 0; i < K; i++) { var a = rnd(1001), b = a + rnd(1001 - a); R.push(document.createRange()); R[i].setStart(u, a); R[i].setEnd(u, b); M.push([a, b]) }
var t = document.createTextNode('abcdefghijklmnopqrstuvwxyz'), T = [], TM = [], tn = 26;
document.getElementById('x').appendChild(t);
for (var i = 0; i < 4; i++) { var a = rnd(27), b = a + rnd(27 - a); var r = document.createRange(); r.setStart(t, a); r.setEnd(t, b); T.push(r); TM.push([a, b]) }
function ins(i) { var e = document.createElement('li'); u.insertBefore(e, L[i] || null); L.splice(i, 0, e); for (var k = 0; k < K; k++) for (var j = 0; j < 2; j++) if (M[k][j] > i) M[k][j]++ }
function rem(i) { u.removeChild(L[i]); L.splice(i, 1); for (var k = 0; k < K; k++) for (var j = 0; j < 2; j++) if (M[k][j] > i) M[k][j]-- }
function rep(o, c, n) { for (var k = 0; k < 4; k++) for (var j = 0; j < 2; j++) { var p = TM[k][j]; if (p > o) TM[k][j] = p <= o + c ? o : p - c + n } }
function edit() {
  var o = rnd(tn + 1), c = rnd(Math.min(4, tn - o) + 1);
  if (c > 0 && rnd(2)) { t.deleteData(o, c); rep(o, c, 0); tn -= c } else { t.insertData(o, 'xy'); rep(o, 0, 2); tn += 2 }
  if (tn > 200) { t.deleteData(0, 150); rep(0, 150, 0); tn -= 150 }
}
function check() {
  for (var k = 0; k < K; k++) if (R[k].startOffset !== M[k][0] || R[k].endOffset !== M[k][1]) bad++;
  for (var k = 0; k < 4; k++) if (T[k].startOffset !== TM[k][0] || T[k].endOffset !== TM[k][1]) bad++;
}
function step(n) {
  for (var q = 0; q < n; q++) {
    var c = L.length, d = rnd(10);
    if (d < 4 && c < 2000) ins(rnd(c + 1)); else if (d < 8 && c > 0) rem(rnd(c)); else edit();
  }
  check();
  return bad;
}";

const ITER: &str = "
var u = document.getElementById('u'), it;
function fill(k) { u.innerHTML = '<li></li>'.repeat(k) }
function fstart() { it = document.createNodeIterator(u, NodeFilter.SHOW_ELEMENT); it.nextNode() }
function fwd(k) { var n, seen = 0; while (seen < k && (n = it.nextNode())) { seen++; n.remove() } return seen }
function bstart() { it = document.createNodeIterator(u, NodeFilter.SHOW_ELEMENT); it.nextNode() }
function nx(k) { var seen = 0; while (seen < k && it.nextNode()) seen++; return seen }
function back(k) { var n, seen = 0; while (seen < k && (n = it.previousNode()) !== u) seen++; return seen }
";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        die("usage: range_load CASE SCALE".into());
    }
    let (name, scale) = (args[1].clone(), args[2].parse::<usize>().unwrap_or_else(|_| die("bad scale".into())));
    let t = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || exec(&name, scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body><ul id=u></ul><p id=x></p></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    let (t, cpu0) = (Instant::now(), cpu_ms());
    let n = scale * 1000;
    let detail = match name {
        "live" => {
            run(&mut b, LIVE);
            for base in (0..n).step_by(500) {
                let bad = run(&mut b, &format!("step({})", 500.min(n - base)));
                if bad != "0" {
                    die(format!("batch {base}: {bad} points differ from the model"));
                }
                b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
            }
            format!("ops={n} points=40")
        }
        "iter" => {
            run(&mut b, ITER);
            // Each phase runs in batches with a collect between them: a script cannot hold more wrappers than the cap.
            let phase = |b: &mut Browser, call: &str| -> usize {
                let mut total = 0;
                loop {
                    let got: usize = run(b, &format!("{call}({BATCH})")).parse().unwrap_or_else(|_| die(format!("{call}: bad count")));
                    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
                    total += got;
                    if got < BATCH {
                        return total;
                    }
                }
            };
            run(&mut b, &format!("fill({n})"));
            run(&mut b, "fstart()");
            let (f, left) = (phase(&mut b, "fwd"), run(&mut b, "u.childNodes.length"));
            if f != n || left != "0" {
                die(format!("forward: visited {f} (want {n}), left {left} (want 0)"));
            }
            let m = n.min(10_000);
            run(&mut b, &format!("fill({m})"));
            run(&mut b, "bstart()");
            if phase(&mut b, "nx") != m {
                die("nx".into());
            }
            let k = phase(&mut b, "back");
            run(&mut b, "u.textContent = ''");
            let left = run(&mut b, "u.childNodes.length");
            if k != m || left != "0" {
                die(format!("backward: visited {k} (want {m}), left {left} (want 0)"));
            }
            format!("items={n} back={m}")
        }
        _ => die(format!("unknown case {name}")),
    };
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let st = b.stats();
    println!(
        "{name:5} x{scale:<3} checksum=ok run={:.1}ms cpu={}ms peak_rss={:.1}MiB | {detail} after: wrappers={}",
        t.elapsed().as_secs_f64() * 1e3,
        cpu_ms() - cpu0,
        status_kib("VmHWM:") as f64 / 1024.0,
        st.wrappers,
    );
}

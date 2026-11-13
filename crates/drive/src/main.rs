//! `bare-drive [--out DIR] [--secrets FILE] [--var NAME=VALUE]... [--size WxH] [--timeout MS] [--repeat N] [--window] [--quiet] STEPS`
//!
//! Runs the steps of the file STEPS (`steps.rs`) and prints the result as JSON on stdout (also `DIR/result.json`). A line for each step
//! goes to stderr unless `--quiet`. Exit code: 0 all the steps passed, 1 a step failed, 2 a bad argument or step file.
//!
//! Ticks: each step, and the whole run, give `ticks`, `tick_max_ms` (the longest), `ticks_over_16` and `ticks_over_50`. A tick is one
//! stretch of work of the page thread: headless, one call of the driver on the page; in a window, one round of the window loop.
//!
//! `--repeat N`: run the steps N times in this process, until one run fails. The output is the result of the last run, and `runs` holds
//! the time, the RSS, the peak RSS and the live heap (glibc only) after each run, to find memory that a journey leaves behind.
//!
//! `--suite DIR [--workers K|auto] [--test-timeout MS] [--login-each] [--only TEXT] [--perf] [--list] [--cold OUTDIR]`: run a suite of
//! related tests that share a login in K warm worker processes (`suite.rs`) and print the summary as JSON (also `OUT/suite.json`).
//! `--perf` counts the instructions of each worker with `perf stat`. `--login-each` gives every test its own login. `--list` shows
//! each test as shared or own. `--cold OUTDIR` writes each test as a step file with its own login and runs nothing.
//!
//! `--secrets FILE`: `NAME=value` lines. A step uses a value as `${NAME}`; the value never appears in the output. `--var` gives a value
//! that is not secret. `--window` shows the page in a Wayland window; the default is headless.

use bare_drive::run::{Options, Redactor, run};
use bare_drive::suite;
use std::collections::HashMap;
use std::time::Duration;

fn main() {
    bare_net::parallel::limit_malloc_arenas(std::env::args().any(|a| a == "--window"));
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let code = std::thread::Builder::new().stack_size(stack).spawn(drive).expect("a thread").join().unwrap_or(1);
    std::process::exit(code);
}

fn usage(e: &str) -> i32 {
    eprintln!("bare-drive: {e}\nusage: bare-drive [--out DIR] [--secrets FILE] [--var NAME=VALUE]... [--size WxH] [--timeout MS] [--repeat N] [--window] [--quiet] STEPS\n       bare-drive --suite DIR [--workers K|auto] [--test-timeout MS] [--login-each] [--only TEXT] [--perf] [--list] [--cold OUTDIR] [same options]");
    2
}

fn drive() -> i32 {
    let mut opt = Options { size: (1280, 800), timeout: Duration::from_secs(10), out: None, window: false, progress: true };
    let (mut vars, mut red, mut file, mut repeat) = (HashMap::new(), Redactor::default(), None, 1u32);
    // The suite: its directory, the worker role, and the options that a worker takes as they came.
    let (mut suite_dir, mut worker, mut workers, mut limit, mut only, mut cold) = (None::<String>, false, None::<usize>, Duration::from_secs(60), None::<String>, None::<String>);
    let (mut perf, mut login_each, mut list) = (false, false, false);
    let mut pass: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or(format!("{a} needs a value"));
        let r: Result<(), String> = (|| {
            match a.as_str() {
                "--suite" | "--suite-worker" => {
                    worker = a == "--suite-worker";
                    suite_dir = Some(val()?);
                }
                "--workers" => {
                    let v = val()?;
                    workers = if v == "auto" { None } else { Some(v.parse().ok().filter(|&n| n > 0).ok_or("--workers K|auto, K > 0")?) };
                }
                "--test-timeout" => limit = Duration::from_millis(val()?.parse().map_err(|_| "--test-timeout MS")?),
                "--only" => only = Some(val()?),
                "--cold" => cold = Some(val()?),
                "--perf" => perf = true,
                "--login-each" => login_each = true,
                "--list" => list = true,
                "--out" => opt.out = Some(val()?.into()),
                "--window" => opt.window = true,
                "--quiet" => opt.progress = false,
                "--repeat" => repeat = val()?.parse().ok().filter(|&n| n > 0).ok_or("--repeat N, N > 0")?,
                "--timeout" => {
                    let v = val()?;
                    opt.timeout = Duration::from_millis(v.parse().map_err(|_| "--timeout MS")?);
                    pass.extend([a.clone(), v]);
                }
                "--size" => {
                    let v = val()?;
                    pass.extend([a.clone(), v.clone()]);
                    let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                    opt.size = (w.parse().map_err(|_| "--size WxH")?, h.parse().map_err(|_| "--size WxH")?);
                }
                "--var" => {
                    let v = val()?;
                    pass.extend([a.clone(), v.clone()]);
                    let (k, v) = v.split_once('=').ok_or("--var NAME=VALUE")?;
                    vars.insert(k.to_string(), v.to_string());
                }
                "--secrets" => {
                    let path = val()?;
                    pass.extend([a.clone(), path.clone()]);
                    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                    for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
                        let (k, v) = line.split_once('=').ok_or(format!("{path}: a line is not NAME=value"))?;
                        red.add(v);
                        vars.insert(k.trim().to_string(), v.to_string());
                    }
                }
                _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
                _ if file.is_none() => file = Some(a.clone()),
                _ => return Err("one step file only".into()),
            }
            Ok(())
        })();
        if let Err(e) = r {
            return usage(&e);
        }
    }
    if let Some(dir) = suite_dir {
        if file.is_some() {
            return usage("a suite takes no step file");
        }
        let dir = std::path::PathBuf::from(dir);
        if worker {
            return suite::worker(&dir, &opt, &vars, &red, login_each, || status_kib("VmHWM:"));
        }
        if let Some(into) = cold {
            return match suite::emit_cold(&dir, std::path::Path::new(&into)) {
                Ok(n) => {
                    eprintln!("{n} step files in {into}");
                    0
                }
                Err(e) => usage(&e),
            };
        }
        if list {
            return match suite::load(&dir) {
                Ok(s) => {
                    println!("setup\t{}", if s.setup.is_some() { "present" } else { "missing" });
                    s.tests.iter().for_each(|t| println!("{}\t{}\t{}", t.name, if t.class == suite::Class::Own { "own" } else { "shared" }, t.why));
                    0
                }
                Err(e) => usage(&e),
            };
        }
        let out = opt.out.clone().unwrap_or_else(|| std::env::temp_dir().join(format!("bare-suite-{}", std::process::id())));
        pass.extend(["--out".into(), out.display().to_string(), "--var".into(), format!("STATE={}", out.join("state.json").display())]);
        let cfg = suite::Config {
            dir,
            out,
            workers,
            test_limit: limit,
            perf,
            login_each,
            only,
            args: pass,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
            avail_kib: mem_available_kib(),
            quiet: !opt.progress,
            exe: None,
        };
        return suite::run(&cfg);
    }
    let Some(file) = file else { return usage("no step file") };
    let steps = match std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}")).and_then(|s| bare_drive::steps::parse(&s, &vars)) {
        Ok(s) => s,
        Err(e) => return usage(&red.clean(&e)),
    };
    if let Some(dir) = &opt.out
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return usage(&format!("{}: {e}", dir.display()));
    }
    // One fixed record for each run, allocated before the first: the list itself does not grow the heap between runs.
    let mut runs = Vec::with_capacity(repeat as usize);
    let report = loop {
        let report = run(&steps, &opt, &mut red);
        let (heap, rss, peak) = (heap_kib(), status_kib("VmRSS:"), status_kib("VmHWM:"));
        runs.push((report.ok, report.ms, rss, peak, heap));
        if opt.progress && repeat > 1 {
            eprintln!("run {}: {} {:.0} ms, RSS {rss} KiB, peak {peak} KiB, heap {heap:?} KiB", runs.len(), if report.ok { "ok" } else { "FAIL" }, report.ms);
        }
        if !report.ok || runs.len() == repeat as usize {
            break report;
        }
    };
    let runs: Vec<String> = runs
        .iter()
        .map(|(ok, ms, rss, peak, heap)| {
            let heap = heap.map_or("null".into(), |k| k.to_string());
            format!("{{\"ok\":{ok},\"ms\":{ms:.1},\"rss_kib\":{rss},\"peak_rss_kib\":{peak},\"heap_kib\":{heap}}}")
        })
        .collect();
    let mut json = report.json(&red);
    json.insert_str(json.len() - 1, &format!(",\"runs\":[{}]", runs.join(",")));
    println!("{json}");
    if let Some(dir) = &opt.out
        && let Err(e) = std::fs::write(dir.join("result.json"), format!("{json}\n"))
    {
        eprintln!("bare-drive: {}: {e}", dir.display());
    }
    if report.ok { 0 } else { 1 }
}

/// A `/proc/self/status` field in KiB (`VmRSS:`, `VmHWM:`), or 0.
fn status_kib(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(field)?.trim().strip_suffix("kB")?.trim().parse().ok()).unwrap_or(0)
}

/// `MemAvailable` in KiB (the memory that a program can take without swapping), or 0.
fn mem_available_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("MemAvailable:")?.trim().strip_suffix("kB")?.trim().parse().ok()).unwrap_or(0)
}

/// The bytes in use in the glibc heap (all arenas and the mmap blocks), in KiB.
#[cfg(target_env = "gnu")]
fn heap_kib() -> Option<usize> {
    #[repr(C)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> MallInfo2;
    }
    // SAFETY: glibc 2.33 and later; it only reads the heap state.
    let m = unsafe { mallinfo2() };
    Some((m.uordblks + m.hblkhd) >> 10)
}

#[cfg(not(target_env = "gnu"))]
fn heap_kib() -> Option<usize> {
    None
}

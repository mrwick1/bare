//! `bare-drive [--out DIR] [--secrets FILE] [--var NAME=VALUE]... [--size WxH] [--timeout MS] [--window] [--quiet] STEPS`
//!
//! Runs the steps of the file STEPS (`steps.rs`) and prints the result as JSON on stdout (also `DIR/result.json`). A line for each step
//! goes to stderr unless `--quiet`. Exit code: 0 all the steps passed, 1 a step failed, 2 a bad argument or step file.
//!
//! `--secrets FILE`: `NAME=value` lines. A step uses a value as `${NAME}`; the value never appears in the output. `--var` gives a value
//! that is not secret. `--window` shows the page in a Wayland window; the default is headless.

use bare_drive::run::{Options, Redactor, run};
use std::collections::HashMap;
use std::time::Duration;

fn main() {
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let code = std::thread::Builder::new().stack_size(stack).spawn(drive).expect("a thread").join().unwrap_or(1);
    std::process::exit(code);
}

fn usage(e: &str) -> i32 {
    eprintln!("bare-drive: {e}\nusage: bare-drive [--out DIR] [--secrets FILE] [--var NAME=VALUE]... [--size WxH] [--timeout MS] [--window] [--quiet] STEPS");
    2
}

fn drive() -> i32 {
    let mut opt = Options { size: (1280, 800), timeout: Duration::from_secs(10), out: None, window: false, progress: true };
    let (mut vars, mut red, mut file) = (HashMap::new(), Redactor::default(), None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or(format!("{a} needs a value"));
        let r: Result<(), String> = (|| {
            match a.as_str() {
                "--out" => opt.out = Some(val()?.into()),
                "--window" => opt.window = true,
                "--quiet" => opt.progress = false,
                "--timeout" => opt.timeout = Duration::from_millis(val()?.parse().map_err(|_| "--timeout MS")?),
                "--size" => {
                    let v = val()?;
                    let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                    opt.size = (w.parse().map_err(|_| "--size WxH")?, h.parse().map_err(|_| "--size WxH")?);
                }
                "--var" => {
                    let v = val()?;
                    let (k, v) = v.split_once('=').ok_or("--var NAME=VALUE")?;
                    vars.insert(k.to_string(), v.to_string());
                }
                "--secrets" => {
                    let path = val()?;
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
    let report = run(&steps, &opt, &mut red);
    let json = report.json(&red);
    println!("{json}");
    if let Some(dir) = &opt.out
        && let Err(e) = std::fs::write(dir.join("result.json"), format!("{json}\n"))
    {
        eprintln!("bare-drive: {}: {e}", dir.display());
    }
    if report.ok { 0 } else { 1 }
}

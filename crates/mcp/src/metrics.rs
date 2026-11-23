//! One JSON line per tool call, appended to a file: `ts` (unix seconds), `tool`, `ms`, `out_chars`, `ok` and, on a failure, `code`.
//! A line holds no URL, page text, typed text or credential. `BARE_MCP_METRICS` is `off` (or empty) to switch it off, or a file path.
//! Unset, the file is `mcp-metrics.jsonl` in the browser cache root (`bare_os::cache_dir`). Past `LIMIT` bytes the file is renamed to
//! `<file>.1` (one old file is kept) and a new one starts. A logging error never fails a call.
use serde_json::Value;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LIMIT: u64 = 5 << 20;

struct Log { file: File, path: PathBuf, size: u64 }

fn path() -> Option<PathBuf> {
    match std::env::var_os("BARE_MCP_METRICS") {
        Some(v) if v.is_empty() || v == "off" => None,
        Some(v) => Some(v.into()),
        None => bare_os::cache_dir().map(|d| d.join("mcp-metrics.jsonl")),
    }
}

fn open(path: &PathBuf) -> Option<Log> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let file = o.open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(Log { file, path: path.clone(), size })
}

/// A name that is plain ASCII is kept; anything else (an unknown tool name from a client) becomes `unknown`.
fn plain(s: &str) -> &str {
    if !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') { s } else { "unknown" }
}

/// Append the line of one finished call. The file opens on the first call.
pub fn record(params: &Value, took: Duration, result: &Value) {
    static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();
    let Some(log) = LOG.get_or_init(|| path().and_then(|p| open(&p)).map(Mutex::new)) else { return };
    let tool = plain(params.get("name").and_then(Value::as_str).unwrap_or(""));
    let chars: usize = result["content"].as_array().map_or(0, |c| c.iter().filter_map(|c| c["text"].as_str()).map(|t| t.chars().count()).sum());
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let ms = took.as_secs_f64() * 1000.0;
    let mut line = format!("{{\"ts\":{ts},\"tool\":\"{tool}\",\"ms\":{ms:.3},\"out_chars\":{chars}");
    if result["isError"] == true {
        let code = plain(result["structuredContent"]["failure"]["code"].as_str().unwrap_or(""));
        line.push_str(&format!(",\"ok\":false,\"code\":\"{code}\"}}\n"));
    } else {
        line.push_str(",\"ok\":true}\n");
    }
    let mut l = log.lock().unwrap_or_else(|e| e.into_inner());
    if l.size >= LIMIT {
        let mut old = l.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::remove_file(&old);
        if std::fs::rename(&l.path, &old).is_ok()
            && let Some(n) = open(&l.path) {
                *l = n;
            }
    }
    if l.file.write_all(line.as_bytes()).is_ok() {
        l.size += line.len() as u64;
    }
}

/// `bare-mcp --stats [FILE]`: per tool, the calls, the p50, p95 and max of the time in ms, and the share of failed calls.
pub fn stats(file: Option<String>) {
    let Some(p) = file.map(PathBuf::from).or_else(path) else { return eprintln!("no metrics file: pass one, or unset BARE_MCP_METRICS") };
    let Ok(text) = std::fs::read_to_string(&p) else { return eprintln!("cannot read {}", p.display()) };
    let mut tools: std::collections::BTreeMap<String, (Vec<f64>, usize)> = Default::default();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (Some(t), Some(ms)) = (v["tool"].as_str(), v["ms"].as_f64()) else { continue };
        let e = tools.entry(t.to_string()).or_default();
        e.0.push(ms);
        e.1 += (v["ok"] == false) as usize;
    }
    println!("{:<20} {:>7} {:>9} {:>9} {:>9} {:>7}", "tool", "calls", "p50 ms", "p95 ms", "max ms", "errors");
    for (t, (mut ms, bad)) in tools {
        ms.sort_by(f64::total_cmp);
        // Nearest rank: the value at ceil(p * n), 1-based.
        let at = |p: f64| ms[((p * ms.len() as f64).ceil() as usize).clamp(1, ms.len()) - 1];
        println!("{t:<20} {:>7} {:>9.2} {:>9.2} {:>9.2} {:>6.1}%", ms.len(), at(0.5), at(0.95), ms[ms.len() - 1], 100.0 * bad as f64 / ms.len() as f64);
    }
}

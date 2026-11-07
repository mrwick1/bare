//! The driver against the fixture app (`tools/fixtures/drive`, served by `common`): a journey of every action, each failure kind, the
//! waits, the diagnostics, and the binary (exit codes, redaction of secrets in the output and the captures).

mod common;

use bare_drive::run::{Options, Redactor, Report, run};
use bare_drive::steps::parse;
use common::{EMAIL, PASSWORD};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Run the steps headless with the fixture secrets. Gives the report and its JSON. One at a time: the network records of the process
/// are one list (`bare_net::trace`).
fn drive(origin: &str, src: &str, out: Option<PathBuf>) -> (Report, String) {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (origin, src) = (origin.to_string(), src.to_string());
    big(move || {
        let mut red = Redactor::default();
        let mut vars = HashMap::from([("ORIGIN".to_string(), origin)]);
        for (k, v) in [("EMAIL", EMAIL), ("PASSWORD", PASSWORD)] {
            red.add(v);
            vars.insert(k.into(), v.into());
        }
        let steps = parse(&src, &vars).unwrap();
        let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out, window: false, progress: false };
        let rep = run(&steps, &opt, &mut red);
        let json = rep.json(&red);
        (rep, json)
    })
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bare-drive-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn no_secret(text: &str) {
    for s in [EMAIL, PASSWORD, "fixture%20pass", "fixture+pass"] {
        assert!(!text.to_lowercase().contains(&s.to_lowercase()), "a secret leaked: {s}");
    }
}

const JOURNEY: &str = r##"
open ${ORIGIN}/login
expect title~"sign in"
expect role=heading name="Sign in" visible
type label="Email" "${EMAIL}"
type label="Password" "${PASSWORD}"
expect label="Password" value="${PASSWORD}"
shot login-filled
click role=button name="Sign in"
expect url~"/overview"
expect text="Loaded" visible
expect css="#stats li" count 3
expect role=button name="Account, Fixture User" enabled
shot overview
click role=link name="Agents"
expect role=heading name="Agents" visible
type placeholder~"search" "beta\n"
expect url="${ORIGIN}/agents?q=beta"
expect text="1 results" visible
expect text="Alpha agent" hidden
expect placeholder~"search" focused
goto ${ORIGIN}/overview
expect text="Loaded" visible
click role=button name~"account"
expect role=menu visible
click role=menuitem name~"sign out"
expect url~"/login"
expect js "sessionStorage.getItem('token') === null"
scroll page 200
press Tab
expect label="Email" focused
reload
expect label="Email" value=""
shot login-empty
"##;

#[test]
fn a_journey_of_every_action_passes() {
    let origin = common::serve();
    let out = tmp("journey");
    let (rep, json) = drive(&origin, JOURNEY, Some(out.clone()));
    assert!(rep.ok, "{json}");
    no_secret(&json);
    let note = |line: &str| rep.steps.iter().find(|s| s.text.starts_with(line)).and_then(|s| s.note.clone()).unwrap_or_default();
    // A field holds the email, and the overview shows it: no capture. The empty login page is captured.
    assert_eq!(note("shot login-filled"), "withheld: a secret is on the page");
    assert_eq!(note("shot overview"), "withheld: a secret is on the page");
    assert!(note("shot login-empty").ends_with("login-empty.png") && out.join("login-empty.png").exists());
    assert_eq!(note("scroll page"), "nothing scrolled");
    // The data of the overview came after a 300 ms timer and a fetch: the step waited for it, and did not fail at once.
    let loaded = rep.steps.iter().find(|s| s.text == "expect text=\"Loaded\" visible").unwrap();
    assert!(loaded.tries > 1 && loaded.waited_ms >= 200.0, "{json}");
    let urls: Vec<(String, u16)> = rep.net.iter().map(|(_, r)| (r.url.rsplit_once(':').unwrap().1.split_once('/').unwrap().1.to_string(), r.status)).collect();
    for want in [("api/login", 200), ("overview", 200), ("api/stats", 200), ("agents", 200), ("login", 200)] {
        assert!(urls.iter().any(|(u, s)| u == want.0 && *s == want.1), "{want:?} not in {urls:?}");
    }
    std::fs::remove_dir_all(out).unwrap();
}

/// Run `steps` on the kinds page; gives the failure kind and message, and the report.
fn kind(origin: &str, steps: &str) -> (String, String, Report) {
    let (rep, json) = drive(origin, &format!("open ${{ORIGIN}}/kinds\ntimeout 1500\n{steps}"), None);
    let (_, f, _) = rep.failure.clone().unwrap_or_else(|| panic!("no failure: {json}"));
    (f.kind.to_string(), f.message, rep)
}

#[test]
fn each_failure_kind_is_named() {
    let origin = common::serve();
    let cases = [
        ("click text=\"Save\"", "duplicate", "matches 2 elements: button.dup, button.dup"),
        ("click role=button name=\"Ghost\"", "hidden", "none is rendered"),
        ("click css=\"#ghost\"", "hidden", "it has no box"),
        ("click css=\"#ghost2\"", "hidden", "visibility: hidden"),
        ("click css=\"#off\"", "disabled", "button#off is disabled"),
        ("click role=button name=\"Under\"", "covered", "button#under is covered by div#veil"),
        ("click text=\"Nothing here\"", "not_found", "no element matches text=\"Nothing here\""),
        ("type css=\"#plain\" \"x\"", "not_editable", "div#plain is not a text field"),
        ("type label=\"Guarded\" \"x\"", "no_focus", "input#guarded did not take the focus"),
        ("expect css=\"#vanish\" visible\nclick role=button name=\"Remove\"\nclick css=\"#vanish\"", "detached", "matched button#vanish before; it left the document"),
        ("expect css=\"#log\" text=\"nope\"", "expect", "its text is \"missing 404\""),
        ("click css=\"p[\"", "not_found", "the selector is not valid"),
        // `goto` returns at once; the next step fails when the load fails.
        ("goto http://127.0.0.1:1/x\nexpect css=\"#log\" visible", "page", "the document did not load"),
    ];
    for (steps, want, msg) in cases {
        let (k, m, rep) = kind(&origin, steps);
        assert_eq!(k, want, "{steps}: {m}");
        assert!(m.contains(msg), "{steps}: {m}");
        // Once the timers of the page ran, nothing can change it: a failure that waits ends before its time limit.
        assert!(rep.steps.last().unwrap().ms < 1400.0, "{steps}: {} ms", rep.steps.last().unwrap().ms);
    }
}

#[test]
fn steps_wait_for_timers_and_scroll_into_view() {
    let origin = common::serve();
    let steps = r##"
open ${ORIGIN}/kinds
click role=button name="Soon"
click role=button name="Later"
expect css="#log" text="later clicked"
click role=button name="Bottom"
expect css="#log" text="bottom 1"
expect role=button name="Under" hidden
"##;
    let (rep, json) = drive(&origin, steps, None);
    // `Under` is covered, not hidden: the check of `hidden` fails.
    let (line, f, _) = rep.failure.clone().expect("the hidden check fails");
    assert_eq!((line, f.kind), (8, "expect"), "{json}");
    assert!(rep.steps[1].waited_ms > 100.0 && rep.steps[2].waited_ms > 0.0, "{json}");
    // The console and the network of the page are in the result. `open` returns at once: the scripts run in the ticks of the next step.
    assert!(json.contains(r#"{"line":3,"level":"error","text":"fixture console error"}"#) && json.contains(r#"{"line":3,"level":"log","text":"fixture log 42"}"#), "{json}");
    assert!(rep.net.iter().any(|(_, r)| r.url.ends_with("/missing.json") && r.status == 404), "{json}");
}

fn bin(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[test]
fn the_binary_exits_by_the_result_and_hides_secrets() {
    let origin = common::serve();
    let dir = tmp("bin");
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, format!("EMAIL={EMAIL}\nPASSWORD={PASSWORD}\n")).unwrap();
    let steps = dir.join("journey.steps");
    std::fs::write(&steps, JOURNEY).unwrap();
    let (s, o) = (secrets.to_str().unwrap(), dir.join("out"));
    let args = ["--secrets", s, "--var", &format!("ORIGIN={origin}"), "--out", o.to_str().unwrap(), "--size", "800x600", steps.to_str().unwrap()];
    let (code, stdout, stderr) = bin(&args);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.starts_with("{\"ok\":true"), "{stdout}");
    no_secret(&stdout);
    no_secret(&stderr);
    no_secret(&std::fs::read_to_string(o.join("result.json")).unwrap());

    // A failure: exit 1, the capture of the failure, the message without the typed password.
    std::fs::write(&steps, "open ${ORIGIN}/login\ntype label=\"Password\" \"${PASSWORD}\"\nexpect label=\"Password\" value=\"nope\"\n").unwrap();
    let (code, stdout, stderr) = bin(&args);
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(stdout.contains("\"kind\":\"expect\"") && stdout.contains("its value is \\\"[secret]\\\""), "{stdout}");
    assert!(o.join("fail-L3.png").exists(), "{stdout}");
    no_secret(&stdout);
    no_secret(&stderr);

    // A bad step file, an unknown variable: exit 2, and the message names no secret.
    std::fs::write(&steps, format!("open ${{ORIGIN}}/login\nclick role=button name=\"{PASSWORD}\" extra\n")).unwrap();
    let (code, _, stderr) = bin(&args);
    assert_eq!(code, 2, "{stderr}");
    no_secret(&stderr);
    std::fs::write(&steps, "open ${NOPE}\n").unwrap();
    assert_eq!(bin(&args).0, 2);
    std::fs::remove_dir_all(dir).unwrap();
}

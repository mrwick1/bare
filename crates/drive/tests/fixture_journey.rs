//! The login journey of the staging app against a local copy of its login model (`tools/fixtures/drive/app.html`, served by `common`):
//! the staging step file passes unchanged, and so do the fail paths (a wrong password, an expired session, sign out). A deliberate
//! failure gives exit 1, a capture and the JSON evidence. The binary `bare-drive` runs each path, headless, as the live job does.

mod common;

use common::{BRIEF_EMAIL, EMAIL, PASSWORD};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run `steps` (a path from the repository root) as `bare-drive --secrets --var BASE --var QUERY --out` against the app. Gives the exit
/// code, the JSON on stdout and the output directory.
fn drive(name: &str, steps: &str, email: &str, query: &str) -> (i32, String, PathBuf) {
    drive_with(name, steps, email, query, &[])
}

fn drive_with(name: &str, steps: &str, email: &str, query: &str, more: &[&str]) -> (i32, String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("bare-drive-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, format!("EMAIL={email}\nPASSWORD={PASSWORD}\n")).unwrap();
    let out = dir.join("out");
    let base = format!("BASE={}", common::serve_app());
    let args = ["--quiet", "--secrets", secrets.to_str().unwrap(), "--var", &base, "--var", &format!("QUERY={query}"), "--out", out.to_str().unwrap()];
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(args).args(more).arg(root().join(steps)).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    for text in [&stdout, &String::from_utf8_lossy(&run.stderr).to_string(), &std::fs::read_to_string(out.join("result.json")).unwrap()] {
        for s in [EMAIL, BRIEF_EMAIL, PASSWORD, "app-token-"] {
            assert!(!text.contains(s), "{name}: a secret or a token leaked: {s}");
        }
    }
    (run.status.code().unwrap_or(-1), stdout, dir)
}

fn passes(name: &str, steps: &str, email: &str) -> String {
    let (code, json, dir) = drive(name, steps, email, "Beta");
    assert_eq!(code, 0, "{name}: {json}");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    std::fs::remove_dir_all(dir).unwrap();
    json
}

/// The status of each request to `path` (any query) in the JSON, in order.
fn statuses(json: &str, path: &str) -> Vec<u16> {
    json.split("\"url\":").filter(|r| r.split('"').nth(1).is_some_and(|u| u.split('?').next().unwrap().ends_with(path))).map(|r| r.split("\"status\":").nth(1).unwrap()[..3].parse().unwrap()).collect()
}

#[test]
fn the_staging_journey_passes_on_the_fixture_app() {
    let json = passes("journey", "tools/drive/staging-journey.steps", EMAIL);
    assert_eq!(statuses(&json, "/api/login"), [200]);
    // The list after the click on Agents, after the search and after the reload: each with the token.
    assert_eq!(statuses(&json, "/api/agents"), [200, 200, 200], "{json}");
}

#[test]
fn a_wrong_password_shows_an_error_and_stores_no_token() {
    let json = passes("invalid", "tools/fixtures/drive/invalid-login.steps", EMAIL);
    assert_eq!(statuses(&json, "/api/login"), [401]);
    assert!(statuses(&json, "/api/agents").is_empty(), "{json}");
}

#[test]
fn an_expired_session_goes_to_the_login_page() {
    let json = passes("expiry", "tools/fixtures/drive/expiry.steps", BRIEF_EMAIL);
    assert_eq!(statuses(&json, "/api/agents"), [200, 401], "{json}");
}

#[test]
fn sign_out_ends_the_session() {
    let json = passes("logout", "tools/fixtures/drive/logout.steps", EMAIL);
    assert_eq!(statuses(&json, "/api/agents"), [200], "{json}");
}

#[test]
fn a_failed_step_gives_exit_1_a_capture_and_the_evidence() {
    // No agent has this name: the check after the search fails once the list is drawn, without its full time limit.
    let (code, json, dir) = drive("fail", "tools/drive/staging-journey.steps", EMAIL, "zeta");
    assert_eq!(code, 1, "{json}");
    let out = |f: &str| dir.join("out").join(f);
    assert_eq!(std::fs::read_to_string(out("result.json")).unwrap().trim_end(), json.trim_end());
    let failure = json.split("\"failure\":").nth(1).unwrap();
    let line = std::fs::read_to_string(root().join("tools/drive/staging-journey.steps")).unwrap().lines().position(|l| l.starts_with("expect js \"Array")).unwrap() + 1;
    assert!(failure.starts_with(&format!("{{\"line\":{line},\"kind\":\"expect\",\"message\":\"the script gave false")), "{failure}");
    assert!(failure.contains("the page is idle"), "{failure}");
    let shot = out(&format!("fail-L{line}.png"));
    assert!(failure.contains(&format!("\"capture\":\"{}\"", shot.display())) && png(&shot), "{failure}");
    assert_eq!(statuses(&json, "/api/agents"), [200, 200], "{json}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn png(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.starts_with(b"\x89PNG") && b.len() > 1000)
}

/// `--repeat N` runs the journey N times in one process (`BARE_DRIVE_REPEAT`, default 3). Each run gives its time and memory.
#[test]
fn the_journey_repeats_in_one_process() {
    let n = std::env::var("BARE_DRIVE_REPEAT").unwrap_or("3".into());
    let (code, json, dir) = drive_with("repeat", "tools/drive/staging-journey.steps", EMAIL, "Beta", &["--repeat", &n]);
    assert_eq!(code, 0, "{json}");
    let runs = json.rsplit_once(",\"runs\":[").unwrap().1;
    assert_eq!(runs.matches("{\"ok\":true,").count(), n.parse::<usize>().unwrap(), "{runs}");
    assert!(runs.contains("\"peak_rss_kib\":") && !runs.contains("\"rss_kib\":0,"), "{runs}");
    println!("runs: {runs}");
    std::fs::remove_dir_all(dir).unwrap();
}

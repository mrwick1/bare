//! `shot` on a page whose scripts never stop: an animation frame loop runs a task on every look, so the page never has two calm
//! looks in a row. The shot must still end at the step limit and take its picture with a `not ready` note (OFS-63).

mod common;

use std::time::{Duration, Instant};

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let body: &[u8] = match target {
        "/svg" => b"<!DOCTYPE html><title>t</title><p>x</p><svg><rect id=r width=9 height=9></rect></svg><script>var i=0;function f(){document.getElementById('r').setAttribute('x',i++%50);requestAnimationFrame(f)}requestAnimationFrame(f)</script>",
        _ => b"<!DOCTYPE html><title>t</title><p>x</p><script>function f(){requestAnimationFrame(f)}requestAnimationFrame(f)</script>",
    };
    (200, "text/html", body.to_vec())
}

/// Run `open`, `timeout 1500` and `shot x` against the page. Gives the JSON and the time. Kills the driver after 20 s.
fn shot(path: &str) -> (String, Duration) {
    let dir = std::env::temp_dir().join(format!("bare-drive-shot-anim-{}-{}", std::process::id(), path.len()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, format!("open {}{path}\ntimeout 1500\nshot x\n", common::serve_route(route))).unwrap();
    let out = dir.join("out");
    let start = Instant::now();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive"))
        .args(["--quiet", "--size", "300x120", "--out", out.to_str().unwrap()])
        .arg(&file)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            child.kill().unwrap();
            panic!("shot of {path} did not end in 20 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let took = start.elapsed();
    let run = child.wait_with_output().unwrap();
    let json = String::from_utf8_lossy(&run.stdout).to_string();
    assert_eq!(run.status.code(), Some(0), "{json}");
    assert!(out.join("x.png").exists(), "{json}");
    std::fs::remove_dir_all(dir).unwrap();
    (json, took)
}

#[test]
fn shot_of_an_endless_animation_frame_loop_ends_at_the_limit() {
    let (json, took) = shot("/");
    assert!(json.contains("not ready: "), "{json}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

#[test]
fn shot_of_an_endless_svg_animation_ends_at_the_limit() {
    let (json, took) = shot("/svg");
    assert!(json.contains("not ready: "), "{json}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

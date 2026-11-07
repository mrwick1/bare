//! The cost of the driver (S5): the time of a click step against the time of the same window events sent straight to `Page::handle`,
//! on pages of 1, 100 and 1,000 buttons, and the CPU that the driver uses while a step waits for a timer 2 s away.
//! `cargo run --release -p bare-drive --example drive_cost [window]`; `window` also opens a real Wayland window and reads the CPU of 5 s
//! of idle (run it inside a compositor, such as a hyprcage screen).

#[path = "../tests/common/mod.rs"]
mod common;

use bare::page::Page;
use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use bare_window::{Event, Mods};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// User and system CPU of this process, in ms.
fn cpu_ms() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = stat.rsplit_once(')').unwrap().1.split_whitespace().collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) * 1000.0 / 100.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let window = std::env::args().nth(1).as_deref() == Some("window");
    let origin = common::serve();
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            const N: usize = 200;
            println!("buttons\tdriver_click_us\tdirect_click_us\toverhead_us\trole_click_us");
            for n in [1, 100, 1000] {
                let url = format!("{origin}/buttons?n={n}");
                let target = n / 2;
                // The driver: a strict CSS locator, the checks, the hit test and the events, as a step.
                let mut src = format!("open {url}\n");
                for _ in 0..N {
                    src += &format!("click css=\"#b{target}\"\n");
                }
                for _ in 0..N {
                    src += &format!("click role=button name=\"Button {target}\"\n");
                }
                let steps = parse(&src, &HashMap::new()).unwrap();
                let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
                let rep = run(&steps, &opt, &mut Redactor::default());
                assert!(rep.ok);
                let css = median(rep.steps[1..=N].iter().map(|s| s.ms * 1000.0).collect());
                let role = median(rep.steps[N + 1..].iter().map(|s| s.ms * 1000.0).collect());
                // Direct: the same three events at the same point, straight to the page, as the Wayland loop gives them.
                let mut p = Page::open(&url, (800, 600)).unwrap();
                p.quiet = true;
                let h = p.query_all(&format!("#b{target}")).unwrap()[0];
                let (x, y) = p.actionable(h).unwrap();
                let mods = Mods::default();
                let mut t = Vec::new();
                for _ in 0..N {
                    let t0 = Instant::now();
                    p.handle(Event::PointerMove { x, y, buttons: 0, mods });
                    p.handle(Event::PointerButton { x, y, button: 0, buttons: 1, pressed: true, mods });
                    p.handle(Event::PointerButton { x, y, button: 0, buttons: 0, pressed: false, mods });
                    p.frame_now();
                    t.push(t0.elapsed().as_secs_f64() * 1e6);
                }
                let direct = median(t);
                println!("{n}\t{css:.0}\t{direct:.0}\t{:.0}\t{role:.0}", css - direct);
            }
            // A step that waits 2 s for a timer: the driver sleeps until the timer is due.
            let steps = parse(&format!("open {origin}/buttons?n=10\nexpect text=\"late\" visible\n"), &HashMap::new()).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
            let c0 = cpu_ms();
            let rep = run(&steps, &opt, &mut Redactor::default());
            assert!(rep.ok);
            println!("wait for a 2 s timer: {:.0} ms, {} tries, CPU {:.0} ms (the page load included)", rep.steps[1].ms, rep.steps[1].tries, cpu_ms() - c0);
            if window {
                let r = bare::control::Remote::open(&format!("{origin}/buttons?n=100"), (800, 600), &bare_net::http::Client::new()).unwrap();
                std::thread::sleep(Duration::from_secs(3));
                let c0 = cpu_ms();
                std::thread::sleep(Duration::from_secs(5));
                println!("window idle 5 s: CPU {:.0} ms", cpu_ms() - c0);
                let t0 = Instant::now();
                let n = 100;
                for _ in 0..n {
                    r.call(|p| p.generation()).unwrap();
                }
                println!("window call round trip: {:.0} us", t0.elapsed().as_secs_f64() * 1e6 / n as f64);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

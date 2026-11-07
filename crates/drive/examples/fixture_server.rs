//! Serve the fixture app of the driver tests (`tools/fixtures/drive`) on a free loopback port; prints the origin and runs for SECS
//! seconds (default 300). `cargo run -p bare-drive --example fixture_server [SECS]`.

#[path = "../tests/common/mod.rs"]
mod common;

fn main() {
    let secs = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    println!("{}", common::serve());
    std::thread::sleep(std::time::Duration::from_secs(secs));
}

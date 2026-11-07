//! Load one page through the session, as `bare --run` does, but trusting the root in a PEM file instead of the OS roots.
//! For a page on a loopback test server with a private CA (tools/net/h2_node_server.mjs).
//!
//!   page_load_roots URL ROOT.pem      with BARE_NET_TRACE=1, prints the wall time and the request table on stderr

use bare::browser_session::{Session, SessionConfig};
use bare_net::http::Client;
use bare_tls::trust_store;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, root] = &args[..] else { panic!("usage: page_load_roots URL ROOT.pem") };
    let pem = std::fs::read(root).unwrap();
    let url = url.clone();
    let worker = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let mut cfg = SessionConfig::new(&url);
            cfg.client = Box::new(move || Client::with_roots(trust_store::decode_pem(&pem).unwrap()));
            let mut s = Session::new(cfg);
            let wall = std::time::Instant::now();
            let r = s.run();
            if bare_net::trace::on() {
                eprintln!("wall_us\t{}", wall.elapsed().as_micros());
                bare_net::trace::dump();
            }
            println!("stop: {:?}", r.stop);
        })
        .unwrap();
    worker.join().unwrap();
}

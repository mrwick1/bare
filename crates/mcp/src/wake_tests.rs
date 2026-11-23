//! The wake of a session worker (`Server::wake_on_done`): a page of the worker puts one wake job in its queue, and no more until the
//! worker takes it. So an idle page with an open WebSocket does not make the worker tick every 4 ms, and wakes never fill the queue.

use super::dispatch_tests::test_shared;
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

fn serve() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let _ = s.read(&mut [0u8; 4096]);
            let body = "<!doctype html><title>t</title><p>x</p>";
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        }
    });
    format!("http://127.0.0.1:{port}/")
}

#[test]
fn a_page_queues_one_wake_until_the_worker_takes_it() {
    let url = serve();
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let tx = Arc::new(tx);
        let mut server = Server { shared: test_shared(), live: HashMap::new(), queue: Arc::downgrade(&tx) };
        let (_, ok) = server.start(&json!({}), &url, None).unwrap();
        assert!(ok, "the page did not open");
        // The worker takes what the load queued.
        for job in rx.try_iter() {
            job.wake.expect("only wakes are queued").store(false, Ordering::SeqCst);
        }
        for _ in 0..5 {
            bare_net::parallel::wake();
        }
        let jobs: Vec<Job> = rx.try_iter().collect();
        assert_eq!(jobs.len(), 1, "one wake waits in the queue");
        jobs[0].wake.as_ref().unwrap().store(false, Ordering::SeqCst);
        bare_net::parallel::wake();
        assert_eq!(rx.try_iter().count(), 1, "a wake after the worker took the first one");
        // A closed session ends its hook.
        server.live.clear();
        bare_net::parallel::wake();
        assert_eq!(rx.try_iter().count(), 0, "a page that is gone does not wake the worker");
    }).unwrap().join().unwrap();
}

//! Requests on worker threads. A page asks for many files that do not depend on each other. One at a time, each costs a round
//! trip (about 40 ms to the staging server), so 17 files cost 17 round trips. `Client::spawn` runs a request on a worker thread
//! and gives a `Task` to wait on, so the caller can start many and take the results in the order that it wants.
//!
//! - At most `MAX_WORKERS` run at once for a client and all its forks (`Client::fork`). Chrome opens 6 connections for each
//!   server with HTTP/1.1, and so do we. More jobs wait in a queue, which is not bounded here: the caller bounds what it starts.
//! - A worker thread ends when the queue is empty. Nothing runs, and no thread is kept, when the page asks for nothing.
//! - The workers use the connection pool of the client: a connection that a worker is done with serves the next request.
//! - A waiting caller sleeps on a condition variable. Nothing polls.

use crate::http::Client;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

/// Worker threads at once for one client and its forks.
pub const MAX_WORKERS: usize = 6;

type Job = Box<dyn FnOnce(&mut Client) + Send>;

#[derive(Default)]
pub struct Workers {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    running: usize,
}

enum Slot<R> {
    Waiting,
    Done(R),
    /// The job panicked, or was dropped before it ran.
    Lost,
}

/// The result of a job that runs on a worker thread.
pub struct Task<R>(Arc<(Mutex<Slot<R>>, Condvar)>);

impl<R> Task<R> {
    /// Wait for the result. `None` when the job was lost (it panicked, or no thread could start).
    pub fn wait(self) -> Option<R> {
        let (slot, ready) = &*self.0;
        let mut slot = slot.lock().unwrap();
        loop {
            match std::mem::replace(&mut *slot, Slot::Lost) {
                Slot::Done(r) => return Some(r),
                Slot::Lost => return None,
                Slot::Waiting => *slot = Slot::Waiting,
            }
            slot = ready.wait(slot).unwrap();
        }
    }
}

/// Marks a task as lost when its job ends without a result (a panic, or the job is dropped unrun).
struct Finish<R>(Arc<(Mutex<Slot<R>>, Condvar)>);

impl<R> Finish<R> {
    fn set(&self, to: Slot<R>) {
        *self.0.0.lock().unwrap() = to;
        self.0.1.notify_all();
    }
}

impl<R> Drop for Finish<R> {
    fn drop(&mut self) {
        let mut slot = self.0.0.lock().unwrap();
        if matches!(*slot, Slot::Waiting) {
            *slot = Slot::Lost;
            self.0.1.notify_all();
        }
    }
}

impl Client {
    /// Run `job` on a worker thread, with a client that shares the pool of this one. Jobs start in the order given.
    pub fn spawn<R: Send + 'static>(&self, job: impl FnOnce(&mut Client) -> R + Send + 'static) -> Task<R> {
        let task = Arc::new((Mutex::new(Slot::Waiting), Condvar::new()));
        let finish = Finish(task.clone());
        let job: Job = Box::new(move |client| {
            let r = job(client);
            finish.set(Slot::Done(r));
        });
        let mut st = self.workers.state.lock().unwrap();
        st.queue.push_back(job);
        if st.running < MAX_WORKERS {
            st.running += 1;
            let mut worker = self.fork();
            // 8 MiB of address space, as the thread of the page: the TLS code needs room. The pages are not used until they are needed.
            let started = std::thread::Builder::new().stack_size(8 << 20).spawn(move || worker.work());
            if started.is_err() {
                st.running -= 1;
                if st.running == 0 {
                    st.queue.clear();
                    // Nobody can run the jobs: drop them, so that their tasks end as lost.
                }
            }
        }
        Task(task)
    }

    fn work(&mut self) {
        loop {
            let job = {
                let mut st = self.workers.state.lock().unwrap();
                match st.queue.pop_front() {
                    Some(job) => job,
                    None => {
                        st.running -= 1;
                        return;
                    }
                }
            };
            job(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const LATENCY: Duration = Duration::from_millis(100);

    /// A server that waits `LATENCY` before each reply. Gives its port and the number of connections it accepted.
    fn slow_server() -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut seen = Vec::new();
                    let mut piece = [0u8; 1024];
                    loop {
                        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                            match stream.read(&mut piece) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => seen.extend_from_slice(&piece[..n]),
                            }
                        }
                        let at = seen.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let path = String::from_utf8_lossy(seen[4..].split(|&b| b == b' ').next().unwrap()).to_string();
                        seen.drain(..at);
                        std::thread::sleep(LATENCY);
                        let body = format!("body of {path}");
                        if stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (port, accepted)
    }

    fn url(port: u16, path: &str) -> bare_url::Url {
        bare_url::Url::parse(&format!("http://127.0.0.1:{port}{path}")).unwrap()
    }

    fn get(client: &mut Client, port: u16, path: &str) -> String {
        String::from_utf8(client.send_once(&crate::http::Request::get(&url(port, path)), 1 << 20).unwrap().body.clone()).unwrap()
    }

    #[test]
    fn many_requests_take_the_latency_times_the_waves_not_the_count() {
        let (port, accepted) = slow_server();
        let client = Client::new();
        let start = Instant::now();
        let tasks: Vec<_> = (0..12).map(|i| client.spawn(move |c| get(c, port, &format!("/{i}")))).collect();
        let bodies: Vec<String> = tasks.into_iter().map(|t| t.wait().unwrap()).collect();
        let took = start.elapsed();
        // 12 requests, 6 at a time, 100 ms each: two waves.
        assert!(took >= LATENCY * 2 && took < LATENCY * 5, "took {took:?}");
        assert_eq!(bodies, (0..12).map(|i| format!("body of /{i}")).collect::<Vec<_>>());
        // 6 connections, each used twice.
        assert_eq!(accepted.load(Ordering::SeqCst), MAX_WORKERS);
    }

    #[test]
    fn the_same_requests_one_by_one_take_the_count_times_the_latency() {
        let (port, _) = slow_server();
        let mut client = Client::new();
        let start = Instant::now();
        for i in 0..4 {
            get(&mut client, port, &format!("/{i}"));
        }
        assert!(start.elapsed() >= LATENCY * 4);
    }

    #[test]
    fn a_fork_reuses_the_connections_of_the_workers() {
        let (port, accepted) = slow_server();
        let client = Client::new();
        let tasks: Vec<_> = (0..6).map(|i| client.spawn(move |c| get(c, port, &format!("/{i}")))).collect();
        tasks.into_iter().for_each(|t| drop(t.wait()));
        assert_eq!(accepted.load(Ordering::SeqCst), 6);
        // A fork has its own cache and the same pool: the next request needs no new connection.
        let mut fork = client.fork();
        assert_eq!(get(&mut fork, port, "/x"), "body of /x");
        assert_eq!(accepted.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn a_job_that_panics_is_lost_and_does_not_stop_the_others() {
        let client = Client::new();
        let bad = client.spawn(|_| -> u8 { panic!("a job fails") });
        let good = client.spawn(|_| 7u8);
        assert_eq!(bad.wait(), None);
        assert_eq!(good.wait(), Some(7));
    }

    #[test]
    fn no_worker_thread_stays_when_the_queue_is_empty() {
        let client = Client::new();
        client.spawn(|_| 1).wait();
        let start = Instant::now();
        while client.workers.state.lock().unwrap().running != 0 {
            assert!(start.elapsed() < Duration::from_secs(2), "a worker did not end");
            std::thread::yield_now();
        }
    }
}

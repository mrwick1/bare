//! Requests on worker threads. A page asks for many files that do not depend on each other. One at a time, each costs a round
//! trip (about 40 ms to the staging server), so 17 files cost 17 round trips. `Client::spawn` runs a request on a worker thread
//! and gives a `Task` to wait on, so the caller can start many and take the results in the order that it wants.
//!
//! - At most `MAX_WORKERS` run at once for a client and all its forks (`Client::fork`). Chrome opens 6 connections for each
//!   server with HTTP/1.1, and so do we. More jobs wait in a queue, which is not bounded here: the caller bounds what it starts.
//! - A worker thread waits `PARK` for a job, then ends. A parked worker uses no CPU. A page that asks for files a few at a time
//!   starts at most `MAX_WORKERS` threads, not one for each file.
//! - The workers use the connection pool of the client: a connection that a worker is done with serves the next request.
//! - Callers use workers only after a reply was slow to start (`parallel_pays`): on a fast link the thread costs more than it saves.
//! - At most `MAX_CONNECTING` connections are set up at once (`Gate`). A burst of new connections to one server loses SYN packets
//!   on staging, and each lost one costs a retry after about 1 s. Measured there: 6 at once, 2 to 4 were late; 2 at once, none.
//! - A waiting caller sleeps on a condition variable. Nothing polls.

use crate::http::Client;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Worker threads at once for one client and its forks.
pub const MAX_WORKERS: usize = 6;

/// Give glibc two malloc arenas, not up to 8 for each core. Each worker thread of a page took an arena of its own, and the memory
/// that it freed there stayed in it: the page thread could not use it, and the next worker took a new arena. In 15 runs of one journey
/// in one process, RSS went from 96 to 121 MiB while the live heap stayed under 2 MiB. With two arenas (the page thread's and one shared
/// by the workers) it stays at about 105 MiB, and a run takes the same time. Call it first in `main`, before a thread starts. A set
/// `MALLOC_ARENA_MAX` wins, so a measurement can choose another count. Other targets: nothing.
pub fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        // SAFETY: glibc; `M_ARENA_MAX` is -8. It sets a limit and touches no memory.
        unsafe { mallopt(-8, 2) };
    }
}

/// How long a worker with no job waits for one before its thread ends.
const PARK: Duration = Duration::from_secs(1);

/// A reply that takes this long to start (request sent to the first byte) means that the server is far: files asked together gain by running together.
/// On a fast link (loopback, a LAN) a worker thread costs more than the round trip that it hides.
pub const SLOW_HEAD: Duration = Duration::from_millis(10);

/// Connections that may be set up (DNS, TCP and TLS) at once for one client and its forks.
pub const MAX_CONNECTING: usize = 2;

type Job = Box<dyn FnOnce(&mut Client) + Send>;

#[derive(Default)]
pub struct Workers {
    state: Mutex<State>,
    /// A parked worker waits on it for a job.
    wake: Condvar,
    /// A reply took `SLOW_HEAD` or more to start. Set once, for this client and its forks.
    slow: AtomicBool,
    pub(crate) connecting: Gate,
}

/// A counter with a limit. `enter` waits on a condition variable while the limit is reached.
#[derive(Default)]
pub(crate) struct Gate {
    inside: Mutex<usize>,
    free: Condvar,
}

pub(crate) struct Inside<'a>(&'a Gate);

impl Gate {
    pub(crate) fn enter(&self) -> Inside<'_> {
        let mut n = self.inside.lock().unwrap();
        while *n >= MAX_CONNECTING {
            n = self.free.wait(n).unwrap();
        }
        *n += 1;
        Inside(self)
    }
}

impl Drop for Inside<'_> {
    fn drop(&mut self) {
        *self.0.inside.lock().unwrap() -= 1;
        self.0.free.notify_one();
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    running: usize,
    /// Workers parked on `wake`.
    parked: usize,
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
    /// Has the job ended (with a result, or lost)? Then `wait` does not block.
    pub fn done(&self) -> bool {
        !matches!(*self.0.0.lock().unwrap(), Slot::Waiting)
    }

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
        ended();
    }
}

type Hook = Arc<dyn Fn() + Send + Sync>;

static ON_END: Mutex<Option<Hook>> = Mutex::new(None);

/// Call `f` from the worker thread each time a task ends (with a result, or lost). A window uses it to wake its loop, so that it
/// need not poll `Task::done`. It replaces the hook of an earlier call. `f` must not block.
pub fn on_end(f: impl Fn() + Send + Sync + 'static) {
    *ON_END.lock().unwrap() = Some(Arc::new(f));
}

fn ended() {
    let hook = ON_END.lock().unwrap().clone();
    if let Some(f) = hook {
        f();
    }
}

impl<R> Drop for Finish<R> {
    fn drop(&mut self) {
        let mut slot = self.0.0.lock().unwrap();
        if matches!(*slot, Slot::Waiting) {
            *slot = Slot::Lost;
            self.0.1.notify_all();
            drop(slot);
            ended();
        }
    }
}

impl Client {
    /// Has a reply been slow to start (`SLOW_HEAD`)? Then running requests together pays. Callers that choose between a worker and
    /// their own thread read it; `spawn` does not.
    pub fn parallel_pays(&self) -> bool {
        self.workers.slow.load(Ordering::Relaxed)
    }

    /// Note how long a reply took to start.
    pub(crate) fn note_head(&self, took: Duration) {
        if took >= SLOW_HEAD && !self.parallel_pays() {
            self.workers.slow.store(true, Ordering::Relaxed);
        }
    }

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
        if st.parked >= st.queue.len() {
            self.workers.wake.notify_one();
        } else if st.running < MAX_WORKERS {
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
        let workers = self.workers.clone();
        loop {
            let job = {
                let mut st = workers.state.lock().unwrap();
                loop {
                    if let Some(job) = st.queue.pop_front() {
                        break job;
                    }
                    // No job: wait a little for the next one, so that a page that asks for files a few at a time does not start a thread for each.
                    st.parked += 1;
                    let (guard, timeout) = workers.wake.wait_timeout(st, PARK).unwrap();
                    st = guard;
                    st.parked -= 1;
                    if timeout.timed_out() && st.queue.is_empty() {
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

    #[test]
    fn the_gate_lets_at_most_max_connecting_in_at_once() {
        let gate = Arc::new(Gate::default());
        let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (gate, now, most) = (gate.clone(), now.clone(), most.clone());
                std::thread::spawn(move || {
                    let _inside = gate.enter();
                    most.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    now.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(most.load(Ordering::SeqCst), MAX_CONNECTING);
    }

    /// A server that waits `LATENCY` before each reply. Gives its port and the number of connections it accepted.
    fn slow_server() -> (u16, Arc<AtomicUsize>) {
        server_after(LATENCY)
    }

    fn server_after(latency: Duration) -> (u16, Arc<AtomicUsize>) {
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
                        std::thread::sleep(latency);
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
        eprintln!("12 requests at 100 ms: {took:?}");
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
    fn parallel_pays_after_a_slow_reply_and_not_after_a_quick_one() {
        let mut quick = Client::new();
        quick.get(&url(server_after(Duration::ZERO).0, "/a")).unwrap();
        assert!(!quick.parallel_pays());
        let mut slow = Client::new();
        slow.get(&url(slow_server().0, "/a")).unwrap();
        assert!(slow.parallel_pays());
        assert!(slow.fork().parallel_pays(), "a fork shares it");
    }

    #[test]
    fn jobs_that_come_one_by_one_run_on_one_parked_thread() {
        let client = Client::new();
        // A job reports its result before its worker parks again. A job that comes in that gap starts a second thread, so wait for the park.
        let ids: std::collections::HashSet<_> = (0..5)
            .map(|_| {
                let id = client.spawn(|_| std::thread::current().id()).wait().unwrap();
                while client.workers.state.lock().unwrap().parked == 0 {
                    std::thread::yield_now();
                }
                id
            })
            .collect();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn no_worker_thread_stays_after_the_park_time_when_the_queue_is_empty() {
        let client = Client::new();
        client.spawn(|_| 1).wait();
        let start = Instant::now();
        while client.workers.state.lock().unwrap().running != 0 {
            assert!(start.elapsed() < Duration::from_secs(2), "a worker did not end");
            std::thread::yield_now();
        }
    }
}

# HTTP/2 client: wake only the stream that changed

## Measurement

- Outlook login journey, archMachine, one run under `perf record -e cycles:u` (build ab6a4ae1 with symbols). About 9.8 s of CPU was on 199 threads other than the main thread.
- The samples of a typical worker thread: kernel time, `Mutex::lock_contended`, `Condvar::wait_optional_timeout`, `clock_gettime` and `h2::Conn::request`. These threads wait for HTTP/2 replies.
- Local bench `https_load h2` (Node `http2` server, one connection, each worker GETs 40 replies of 256 KiB), base 04c36bdc, 3 runs each:
  - 6 workers: CPU 0.155–0.162 s, 18,037–19,080 voluntary context switches. The replies have about 3,840 DATA frames, so each frame costs about 4.8 thread wakes.
  - 16 workers: CPU 0.928–0.970 s, 170,385–180,360 switches. The cost grows with the square of the waiters.

## Current design

- One connection has one reader thread and one `Condvar` (`Shared::cv`) with the state mutex `st`.
- Each request thread waits on that `Condvar` in 4 places: for a stream slot, for send window, for the reply head, and for body data (`Body::read`).
- The reader calls `notify_all` after each frame. So each DATA frame wakes every waiter of the connection. Each woken thread takes `st`, finds no change for its stream, and sleeps again. The reader also wakes threads while they still contend for `st`.
- Cost: O(waiters) wakes, lock takes and context switches per frame. A `notify_all` is a `futex` system call even when no thread waits.

## Options

1. A `Condvar` per stream (chosen). Each `Stream` has an `Arc<Condvar>` that is used with the same mutex `st`, and a `waiting` flag. A stream frame (DATA, HEADERS, RST_STREAM, a stream WINDOW_UPDATE) wakes only that stream's thread, and only when `waiting` is set. The wake comes after the mutex is released. A change of the whole connection (SETTINGS, the connection WINDOW_UPDATE, GOAWAY, the end) wakes every waiting stream and `Shared::cv`. Only slot waiters use `Shared::cv`. They wake when a slot frees.
   - Wakes per frame: 0 or 1. Memory: one `Arc<Condvar>` (about 24 bytes) per open stream. Correctness: each wait still checks its condition under `st`. A `Condvar` cannot lose a wake that is sent after the state change under the mutex.
2. An event loop (as in Chrome's network service, or `hyper` on `tokio`). One thread owns all sockets, and requests are tasks that the loop polls. This removes the thread per request too. But the whole client is synchronous (`Read` bodies, worker pool, TLS stream), so this is a rewrite of `http.rs`, `h2.rs` and their callers.
3. A channel per stream. The reader sends the bytes to the stream's channel. This also moves the copy out of the lock. But flow control, reset and close need the shared state anyway, so it needs the same wake rules as option 1, plus a second queue.

Option 1 removes the wasted wakes with a local change and no new dependency. Option 2 stays open if the thread count itself becomes the cost.

## Expected gain

- On the bench: CPU should fall to about the cost of the useful wakes (one per frame), and the switches to about one per frame plus one per request.
- On the Outlook login: the off-main CPU of the h2 workers. The main thread does not change.

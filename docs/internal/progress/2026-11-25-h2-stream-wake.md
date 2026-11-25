# HTTP/2 client wakes only the stream that changed

Design: `docs/internal/design-h2-wake.md`.

## Change

- Each HTTP/2 stream has its own `Condvar` and a `waiting` flag. The reader wakes only the thread of the stream that a frame changed, and only when it waits. The wake comes after the state lock is released.
- SETTINGS, the connection WINDOW_UPDATE, GOAWAY and the end of the connection wake every waiting stream.
- `Shared::cv` is only for threads that wait for a stream slot. It wakes when a slot frees. A request thread that closes its own stream also wakes it now.
- New bench mode `https_load h2 <scale> [threads]`: workers GET 256 KiB replies at once on one Node `http2` connection. It prints the context switches of the process.

## Check

- `t` in `crates/net`: 7 test files pass. `h2_node.rs`: 4 pass. `h2_nginx.rs` skips: nginx is not installed on archMachine.
- Bench, archMachine, `https_load h2 2` (40 GETs of 256 KiB per worker), 3 runs each, base 04c36bdc:

| workers | tree | CPU s               | wall s              | voluntary switches        |
| ------- | ---- | ------------------- | ------------------- | ------------------------- |
| 6       | base | 0.160, 0.155, 0.162 | 0.165, 0.172, 0.161 | 18,322, 18,037, 19,080    |
| 6       | new  | 0.121, 0.118, 0.112 | 0.163, 0.168, 0.158 | 2,210, 2,965, 2,814       |
| 16      | base | 0.970, 0.928, 0.930 | 0.467, 0.466, 0.483 | 180,360, 180,080, 170,385 |
| 16      | new  | 0.327, 0.331, 0.348 | 0.413, 0.402, 0.437 | 12,087, 12,207, 12,267    |

CPU: -25% with 6 workers, -64% with 16. Switches: -85% and -93%. Wall time is bound by the server and changes little.

- Cost rows: no row of `cost_rows.txt` uses the HTTP/2 client. None ran.
- Live Outlook open to the login form (no login), archMachine, 3 runs each, base 7299e011, `perf stat -e task-clock`: base 490, 439, 429 ms CPU; new 423, 436, 427 ms. Wall: base 10.4, 5.8, 4.2 s; new 4.0, 5.1, 3.8 s. This page loads few scripts, so the CPU change is inside the spread. The wall time is bound by the network.
- Not tested: Windows; nginx; the logged-in Outlook and Teams journeys.

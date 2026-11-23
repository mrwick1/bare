# A wake for each page loop: an idle WebSocket stops waking bare-mcp

## Problem

A page with network work open asks its loop for a tick in 4 ms. A window installs a wake hook (`Page::wake_on_done`), and then asks for a tick each second only. The hook was one for the process: the last call won. The headless loops installed none. An idle open WebSocket made each `bare-mcp` session worker wake 250 times a second, for as long as the page lived.

## Change

- `bare_net::parallel::on_end` keeps a list of hooks. It gives an `EndHook`; dropping it removes the hook. Each loop has its own.
- `Page` keeps its `EndHook`. A reopen keeps it.
- `bare_drive::run::Session::wake_on_done` gives the wake to the page of the session, and to the page of each later `open`.
- `bare-mcp`: each session worker installs a wake for its session. The wake puts a wake job in the queue of the worker. The worker clears the flag and ticks its pages. At most one wake waits in the queue, so wakes never fill it. The hook holds the queue weakly, so a worker still ends when its slot goes.

## Results

Machine: archMachine. Release `bare-mcp`, main `a10b0898` and this branch. The chat fixture (`tools/fixtures/chat`), `plain.html`, socket connected, then 20 s idle. CPU of the process from `/proc/<pid>/stat` (10 ms ticks). 3 runs each.

| Build | Socket open | CPU in 20 s |
|---|---|---|
| main | yes | 0.050 s, 0.050 s, 0.050 s |
| branch | yes | 0.000 s, 0.000 s, 0.000 s |
| main | no | 0.000 s (1 run) |
| branch | no | 0.000 s (1 run) |

## Tests

- New: `parallel_wake_tests` (two hooks both wake; a dropped hook does not). It failed on main: only the last hook was called. `mcp/src/wake_tests.rs` (one wake in the queue until the worker takes it; none after the session closes).
- Pass: `bare-drive --test ws_wake --test idle`, `bare-mcp --bins` (6), `bare --lib page_tests` (9) and `browser_websocket` (9).

## Not tested

- A WebSocket message that comes while a `bare-mcp` session is idle, end to end. The drive test `ws_wake` covers the page side, and `wake_tests` the queue side.
- Windows.
- The standalone `bare-drive` loop: it runs steps back to back and sleeps without a tick in `wait`, so it had no idle poll to remove.

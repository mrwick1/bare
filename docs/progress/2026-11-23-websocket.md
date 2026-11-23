# WebSocket over the network

## Change

- `WebSocket` connects. Before, `shims/websocket.js` failed every connection with `error` and `close` 1006.
- `bare_net::ws` (new) is the RFC 6455 client. `connect` sends the opening handshake on a new HTTP/1.1 connection (`Client::upgrade` in `http.rs`, ALPN `http/1.1` only). The request has the cookies of the jar, the identity of the client, `Origin`, a random key and the subprotocols. The reply must be `101` with the right `Sec-WebSocket-Accept`. An extension or a subprotocol that was not offered fails the handshake. `Set-Cookie` of the reply goes in the jar.
- The connection splits into a `Reader` and a `Writer`, as in `h2.rs`. The reader waits on the socket (`peek`), then locks the stream for one read only. No thread polls. An idle socket wakes its reader once each read time limit (30 s).
- The reader puts fragments together, answers ping with pong and close with close. A masked server frame, a set RSV bit, an unknown opcode or a bad control frame closes with 1002. A text that is not UTF-8 closes with 1007. A message over 32 MiB closes with 1009. Each client frame is masked with OS random bytes, taken 64 at a time.
- `browser_websocket.rs` (new) gives the shim one native, `ws(op, ...)`. Each socket has a reader thread and a writer thread. The page sends through a channel, so a slow peer never blocks the page. `bufferedAmount` counts the bytes that the writer has not written yet.
- The events come through `Browser::run_streams`, as the chunks of an event stream do: one task each, with a microtask checkpoint. An open socket does not hold the page busy (`live_work_of(false)`). `Page::pending` lists it as `websocket`. When the page goes away, the channels drop and both threads end.
- The shim checks the subprotocols (token, no duplicate), the close reason (123 bytes at most) and mixed content (`ws:` from an `https:` page, except loopback), as Chrome does. `send` takes a string, an `ArrayBuffer`, a view or a `Blob`. A `Blob` keeps its place in the send order. `binaryType` `blob` or `arraybuffer` works. `close()` before the open fires `error` and then `close` 1006.

## Measurements

Machine: archMachine, with other work running. Base: origin/main at `ae150ab2`. New: this branch. `tools/js/cost_matrix.sh -j 2 -g 'fetch_load|xhr_load|streams_load|binary_load stream'`, 3 runs per row (minimum), glibc. The change in `streams_ready` runs on every turn of the loader.

| Row | Change |
|---|---|
| fetch_load bytes / iterate / objects / post, 1 and 10 | +0.000% to +0.050% |
| xhr_load 1 / 100 | +0.004% / +0.056% |
| streams_load churn / flow / many / pending, 1 and 10 | +0.000% to +0.009% |
| binary_load stream 1 / 10 | +0.000% / -0.000% |

No row is over +0.5%.

## Tests

- New: `ws_tests` (`bare-net`, 7 tests): an echo of text and of binary in all three length forms, ping, fragments, a close from each side, the subprotocol choice, a bad handshake, the bad frames with their codes, and an end with no close.
- New: `browser_websocket_net_tests` (`bare`, 5 tests): an echo of text, a view, a `Blob` and text in order, then a clean close. Also a `Blob` message, a close from the server, a protocol error (error, then close 1006), a close before the open, and bad arguments.
- `browser_websocket_tests`: the 3 old tests pass. A refused handshake (404) still gives error, then close 1006.
- `cargo test --locked -p bare-net`: 187 pass.
- E2E: release `bare-drive` opens a local page that talks to a public echo server. The page sends text, 3 bytes and 70,000 characters, then closes with 1000.
  - `wss://echo.websocket.org/`: 3 of 3 runs get all the echoes and a clean close 1000 (1.64 s to 1.75 s).
  - `wss://ws.postman-echo.com/raw`: the server drops the connection after a binary message (1006). It closes the 70,000-character text with 1009. Node 22's built-in `WebSocket` gets the same result with the same script. Without the binary message, bare gets a clean 1009 in 3 of 3 runs, as Node does.

## Not tested

- `permessage-deflate`: not offered.
- `ws:` through an HTTP proxy: refused with an error. `wss:` through a proxy uses the CONNECT tunnel, but it was not tested.
- A replay tape has no WebSocket: the connection fails.
- Windows: the code uses only `std` sockets (`peek`, `try_clone`, `shutdown`). Not run on Windows; the CI Windows job runs the tests.
- A real site that needs a WebSocket for a login: none was available.

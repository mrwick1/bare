# WebSocket: a chat app fixture, and two fixes it found

Follows `2026-11-23-websocket.md` (#982).

## Change

- `tools/fixtures/chat` (new): a chat app to run bare against a real WebSocket stack. `run.sh` installs `socket.io` 4.8.4 and `ws` 8.23.0 in a work directory and starts `server.mjs`.
  - `index.html`: Socket.IO. It starts with HTTP long-polling and upgrades to WebSocket. It has rooms, names, history, presence, typing, acks, images (binary) both ways, a bot and a reconnect.
  - `plain.html`: the native `WebSocket` on `/ws`. It has subprotocols, text and binary echo, a long message, server pings, a reconnect with backoff and a close from the server.
  - `/login` sets an HttpOnly session cookie. `/ws?auth=1` refuses a socket without it (401).
  - `chat.steps`, `plain.steps` and `auth.steps` are the `bare-drive` journeys. `GET /log` gives what the server saw.
- Fix: a socket message did not wake a window. The loop of a window (and of a driven window, `control.rs`) sleeps until a worker ends a job (`Page::wake_on_done`), and looks by itself only once a second. The reader thread of a socket was not a worker, so a message waited up to 1 s. The reader now calls `bare_net::parallel::wake` after each event.
- Fix: a page that went away dropped its socket with no close frame, so the server saw 1006. The writer now sends close 1001 ("going away"), as Chrome does.

## Results

Machine: archMachine. Release `bare-drive` of this branch, server on 127.0.0.1:8790, 3 runs of each journey: all pass.

- `chat.steps` (27 steps): the server log shows `connect polling`, then `upgrade websocket`. The image arrives as a binary `Buffer` of 6 bytes. After `engine.close()`, the client reconnects and upgrades again.
- `plain.steps`: the socket stays open across 2 pings of the server (11 s). A dropped connection gives close 1006 and a reconnect. `kick` gives close 4000 `kicked` (clean).
- `auth.steps`: before the login the server refuses the socket. After it, the handshake has `Cookie: sid=s3cret`, and `document.cookie` stays empty.
- A navigation away from an open socket: the server sees close 1001 (3 of 3 runs). Before the fix: 1006.
- Wake: `ws_wake::a_socket_message_wakes_the_loop_at_once` gave 1.001 s before the fix and passes after it (the message comes at about 600 ms). An idle open socket makes fewer than 5 ticks in 1 s.

Cost: an idle open socket in a headless `bare-drive` session used 0.123 s to 0.137 s CPU in 20 s (3 runs, about 0.65% of a core). The headless loops (`bare-drive`, the headless sessions of `bare-mcp`) have no wake hook: with a socket open, they tick every 4 ms for as long as the page lives. The window loop does not.

## Tests

- New: `crates/drive/tests/ws_wake.rs` (2 tests); `browser_websocket_net_tests::a_page_that_goes_away_closes_its_socket_with_1001`. Both failed before their fix.
- `cargo test -p bare --lib browser_websocket` (9), `-p bare-drive --test ws_wake --test idle` (6), `-p bare-net parallel` (8): pass.
- `bare-drive` gains a dev-dependency on `bare-tls` (SHA-1 for the test server's handshake).

## Not tested

- The window itself (`--window`): only its loop, in `ws_wake`.
- Two real users in two bare sessions: the bot is the second user.
- `permessage-deflate`: still not offered. Both servers enable it, so they send uncompressed frames to bare.
- Event streams (`queue_xhr_stream`) have the same missing wake in a window. Not changed here.
- Windows.

## Next (hypothesis)

- A wake for each page, so that a headless session with an open socket sleeps, as a window does.

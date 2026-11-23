# WebSocket: permessage-deflate

## Change

- `bare_net::ws::connect` offers `permessage-deflate; client_max_window_bits`, as Chrome does. It reads the reply of the server: only `permessage-deflate`, with the settings of RFC 7692, each once. Another extension or setting fails the handshake. `connect` gives the extensions that the server took.
- The reader inflates a message whose first frame has RSV1, with `flate2` (the `miniz_oxide` backend that `bare-net` builds already). It adds the 4 bytes `00 00 ff ff`, and stops at the cap of the caller (1009). The window stays from one message to the next, unless the server sent `server_no_context_takeover`. RSV1 without the extension, on a control frame or on a continuation frame is 1002. Data that does not inflate is 1002.
- The client sends its own messages without compression. RFC 7692 allows this. It keeps no compressor state and spends no CPU on it.
- `WebSocket.extensions` gives what the server took.
- `tools/fixtures/chat/plain.steps` expects `chat.v1 permessage-deflate` in `#proto`. The page shows the protocol and the extensions there.

## Bug found by the fixture

The first inflate loop stopped when the output had room left but input was not used up, and called that an error. A 300,000-character message that compresses to a few hundred bytes failed with 1002. The loop now goes on while the inflater makes progress. The test `a_long_message_that_compresses_well_inflates_whole` failed before the fix and passes after it.

## Results

Machine: archMachine. Release `bare-drive` of this branch, the chat fixture on 127.0.0.1:8790. Both servers there enable `permessage-deflate` (threshold 256 bytes).

- `chat.steps`, `plain.steps` and `auth.steps`: 3 of 3 runs pass each.
- The server log shows `extensions: permessage-deflate` for each plain socket. The 300K echo arrives whole.

## Tests

- New: `ws_deflate_tests` (5): a shared window over two messages and fragments, `server_no_context_takeover`, a 300K message, bad frames (cap 1009, RSV1 misuse 1002, bad data 1002), and replies that were not offered. `browser_websocket_deflate_tests` (1): the example "Hello" of RFC 7692, `extensions` on the page.
- Changed: `ws_tests::a_bad_handshake_fails` uses `x-webkit-deflate-frame` as the extension that was not offered.
- Pass: `bare-net ws_`, `bare` lib (1764). Clippy is clean on the touched files.

## Not tested

- Real public servers with compression.
- Windows.
- Cost rows: no cost row covers WebSocket. Inflate runs only when a server compresses a message.
- Memory: an inflater is about 43 KB for each socket that took the extension (hypothesis, from the `miniz_oxide` state and its 32 KB window). Not measured.

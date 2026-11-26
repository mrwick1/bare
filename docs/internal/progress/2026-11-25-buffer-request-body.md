# Buffer request bodies of fetch and XMLHttpRequest (2026-11-25)

## Change

`Ops::bytes_of` of the Vex engine (`eng_new.rs`) always returned `None`. So `extract_body` (`browser_fetch.rs`) did not see an
`ArrayBuffer`, a typed array or a `DataView`. It sent the body as the text `[object ArrayBuffer]` with
`Content-Type: text/plain;charset=UTF-8`. This occurred for `fetch` and `XMLHttpRequest`, on the page and in a worker.

Now `bytes_of` calls the new `Realm::value_bytes` (`crates/vex/src/bi_buffer.rs`). It gives the bytes of a buffer or a view with no
copy, as `Engine::bytes` does. A shared buffer gives `None`, as before.

## Why

WhatsApp Web could not send an attachment. Its media upload (`WAWebMmsClientMmsUpload`, through `WAWebHttpExtendedFetch`) sends
the encrypted media as an `ArrayBuffer` with `XMLHttpRequest`:

- `xhr.open("post", "https://media-<pop>.cdn.whatsapp.net/mms/<type>/<url-safe base64 SHA-256 of the body>?auth=..&token=..&media_id=..", true)`
- an upload `progress` listener, `withCredentials = false`, no request headers, `responseType = "arraybuffer"`
- `xhr.send(buffer)`. The buffer is the AES-CBC ciphertext without the IV, then 10 bytes of HMAC-SHA256.

The server answers 415 when the SHA-256 of the body is not the hash in the path. WhatsApp maps 415 to
`MediaInvalidError("mmsUpload: hash mismatch")`. bare sent the 20 bytes `[object ArrayBuffer]`, so the hash never matched.

## Tests

- New: `browser_worker_fetch_upload_tests.rs`. A worker gets a 21-byte Blob from the page and does the WhatsApp steps. The server
  must get the 42 bytes that the worker hashed, `Content-Length: 42`, no `Content-Type` and `Origin` of the page. One test sends to
  the same origin, one to another origin. Both fail before the fix (the body is `[object ArrayBuffer]`) and pass after it.
- These tests were ignored on Vex for this fault. They pass now and are not ignored any more:
  `browser_fetch_tests::bytes_and_string_bodies_get_the_right_content_type`, `bodies_are_the_bytes_that_chrome_sends`,
  `request_and_response_take_blob_form_and_params_bodies`, and `browser_xhr_tests::every_recorded_chrome_case_gives_the_same_result`.
- `t`: all pass. `browser_fetch_tests.rs`: 58 pass, 3 ignored. `browser_xhr_tests.rs`: 12 pass.

## Cost

Not run. The `cost_matrix.sh` rows need two release trees, and the disk of `/data` was at 97%. The Vex change adds one function that
the interpreter does not call. The host change replaces a constant `None` with a range lookup. It runs only for a request body
that is an object.

## test262

Not run. The Vex change adds one public function and changes no behavior of the interpreter.

## Not tested

- The live WhatsApp Web upload. A logged-in profile is necessary.
- A `SharedArrayBuffer` body. The Fetch and XHR Standards refuse it (`BufferSource` has no `[AllowShared]`). `value_bytes` gives `None` for it.

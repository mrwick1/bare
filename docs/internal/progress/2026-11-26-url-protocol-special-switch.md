# URL.protocol switches between special schemes

## Cause
T3 Code makes its socket address with `url.protocol = 'ws:'` on an `http:` URL (chunks `runtime-*.js` and `utils-*.js`). `Rec::set_protocol` in `crates/bare/src/browser_navigation_url.rs` returned `UErr::Scheme` for `ws`, `wss` and `ftp`. `uerr` in `browser_navigation_params.rs:121` turned it into "URL scheme other than http and https". The app mapped the failure to "HTTP 500" on its session request.

## Change
- `set_protocol` follows WHATWG: a switch between `http`, `https`, `ws`, `wss` and `ftp` works. Other switches change nothing. `file` stays gated, and so does user info on a `ws`, `wss` or `ftp` result.
- `Location` still refuses a `ws`, `wss` or `ftp` protocol (`loc_set` checks `is_alt`).

## Tests
- New: `browser_navigation_url_protocol_tests.rs` (2 tests). The first failed with `Scheme` before the fix.
- `t` and `cargo check -p bare --all-targets` pass.

## Result in bare
`bare-drive` on `http://localhost:3773`: no scheme error. The page shows the T3 pairing screen ("Pair with this environment", pairing token field). The console shows one WebSocket handshake 401, which is expected without a session.

## Not tested
- A paired session (no token used).
- `ws://` URL with user info.
- Windows.
- Cost rows.

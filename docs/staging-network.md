# Staging network report

Date of inspection: 2026-11-04. Target: `https://staging.app.8vox.ai` (156.67.110.109). No credentials were used. Only public pages and public static files were read. Machine: Arch Linux, OpenSSL 3.6.4, curl 8.21.0, Python 3.14.7.

Words: **observed** means a command showed it on this date. **inferred** means it follows from observed facts but no command proved it. **to verify** means nobody tested it.

## Tool and commands

`tools/staging_inspect.py` uses Python stdlib and `openssl` only. Fixtures are in `tools/fixtures/staging/`.

| Command | What it does | Requests |
| --- | --- | --- |
| `python3 -I tools/staging_inspect.py selftest` | Offline. Parses the saved login page and webpack runtime. Checks the manifest hash, the gzip/deflate decoders and a 100x parse. | 0 |
| `python3 -I tools/staging_inspect.py tls` | About 25 short `openssl s_client` handshakes with one offered option each. | 25 handshakes |
| `python3 -I tools/staging_inspect.py fetch --out FILE` | Reads `/` and `/login`, then each file they name, plus the lazy chunks that the webpack runtime names. Each file is read once as identity and once for each Accept-Encoding value. | 315 requests, 1 connection, 10 s |
| `sh tools/staging_https_check.sh` | The first `bare` HTTPS check (see the end). Fails today. | 1 |

Limits in the tool: one connection at a time, 400 requests, 8 MiB for a body, 300 s in total, and no other origin. Decoded bodies also have an 8 MiB cap. Socket operations have a timeout; the total time cap is checked between requests and attempts. It reads no page that the two pages do not name. This is not a load tool. The raw-byte probes below were plain `ssl` sockets in Python (one request each, `Connection: close`).

## TLS (observed)

- Server picks TLS 1.3 by default. TLS 1.2 also works (`ECDHE-ECDSA-AES256-GCM-SHA384`). Old TLS 1.0 and 1.1 probes failed, but the local OpenSSL may forbid them, so this is **not established**.
- Cipher suites that the server accepts in TLS 1.3: `TLS_AES_128_GCM_SHA256`, `TLS_AES_256_GCM_SHA384`, `TLS_CHACHA20_POLY1305_SHA256`. It rejects `TLS_AES_128_CCM_SHA256` (alert 40). When the client offers all, the server picks `TLS_AES_256_GCM_SHA384`.
- Key exchange: each of `X25519`, `secp256r1`, `secp384r1`, `secp521r1`, `X448` and `ffdhe2048` worked when it was the only offered group. `X25519MLKEM768` also works and the server picks it when the client offers it first.
- Server signature in `CertificateVerify`: `ecdsa_secp256r1_sha256` only. When the client offered only `ecdsa_secp384r1_sha384`, `rsa_pss_rsae_sha256` or `ed25519`, the handshake failed (alert 40).
- ALPN: `http/1.1` is chosen if offered. `h2` offered alone gets no ALPN (the connection still works). The server then speaks HTTP/1.1.
- SNI: the server sends the same leaf without SNI (`-noservername`). It does not require SNI. Send it anyway. To verify: whether the server has another default certificate on other names.
- No OCSP stapling (`OCSP responses: no responses sent`).
- A minimal hello works. Offer only `TLS_AES_128_GCM_SHA256`, group `X25519`, signature algorithm `ecdsa_secp256r1_sha256`, ALPN `http/1.1`. Result: handshake OK, certificate verified, `Peer signature type: ecdsa_secp256r1_sha256`.

## Certificate chain (observed)

The server sends four certificates.

| # | Subject | Issuer | Public key | Signature | Valid until | DER SHA-256 |
| --- | --- | --- | --- | --- | --- | --- |
| 0 leaf | CN=staging.app.8vox.ai | Let's Encrypt YE2 | EC P-256 | ecdsa-with-SHA384 | 2027-01-13 (from 2026-10-15) | `cdf85d23…70c35b` |
| 1 | Let's Encrypt YE2 | ISRG Root YE | EC P-384 | ecdsa-with-SHA384 | 2028-09-02 | `97658de8…cbab4` |
| 2 | ISRG Root YE | ISRG Root X2 | EC P-384 | ecdsa-with-SHA384 | 2032-09-02 | `0fc0901c…485f0` |
| 3 | ISRG Root X2 | ISRG Root X1 | EC P-384 | sha256WithRSAEncryption | 2032-09-02 | `ee5f7abd…fe2c6` |

- Leaf: SAN `staging.8vx.in` and `staging.app.8vox.ai`. Key usage `digitalSignature` (critical). EKU `serverAuth`. `CA:FALSE`. Has embedded SCTs, a CRL URL and a CA Issuers URL. No OCSP URL. Serial `06E00CBB30E8A1134AD45513205C70D78DF2`.
- Certificate 1 has `CA:TRUE, pathlen:0`. Certificate 2 has `CA:TRUE` and no path length. Both have key usage `keyCertSign`.
- Certificate 3 is a cross-sign. It has the same public key as the self-signed `ISRG Root X2` in the system store (SPKI SHA-256 `762195c2…8332`), but a different certificate (the store copy is from 2020, fingerprint `69729b8e…1470`).
- Trust anchor: OpenSSL stopped at the self-signed `ISRG Root X2` from the system store (`depth=3`). `ISRG Root YE` is **not** in the system store (`/etc/ssl/certs` has only `ISRG_Root_X1.pem` and `ISRG_Root_X2.pem`).

### Requirements for verification

Observed facts give these needs. This is a sufficient algorithm set for the observed chain and the tested minimal handshake. A certificate renewal can change it.

1. ECDSA P-256 with SHA-256 for `CertificateVerify`. **Required for the tested configuration** (the other individually tested algorithms failed).
2. ECDSA P-384 with SHA-384 for the certificate signatures of certificates 0, 1 and 2. **Required.** This also needs SHA-384.
3. RSA is **not required** if the trust store holds `ISRG Root X2` and the verifier ends the path there. Certificate 3 (RSA signed by X1) then is not used. It is only needed if the store holds `ISRG Root X1` alone (inferred; not tested).
4. The path builder must match by issuer name (and AKI/SKI), and must accept a trust anchor that is not the last certificate sent. The server sends a cross-signed X2 and the store has the self-signed X2. A verifier that insists that the last sent certificate is a root fails here (inferred).
5. Check: the hostname against SAN DNS names, validity dates, `CA:FALSE` for the leaf, `CA:TRUE` for issuers, `pathlen`, key usage, EKU `serverAuth`. Unknown critical extensions must fail.
6. Trust store: `bare` needs a trusted root source independent of the peer-supplied chain. This report names `ISRG Root X2` (SPKI SHA-256 above). The source of the store (a PEM file from the OS, or a file in the repo) is a decision for S1 topic 3. **No certificate bypass.**
7. The leaf expires 2027-01-13 (Let's Encrypt, about 90 days). A fixture with this leaf expires. Tests that need a valid chain must use a fixed "now" or a self-made chain.

### Requirements for the handshake

- Minimum that works (observed): `TLS_AES_128_GCM_SHA256`, `X25519`, signature algorithm `ecdsa_secp256r1_sha256`, SNI, ALPN `http/1.1` or none.
- AES-128-GCM with SHA-256 key schedule is enough for the first working handshake. Offering `TLS_AES_256_GCM_SHA384` would need the SHA-384 key schedule too (SHA-384 is needed for certificates anyway).
- The post-quantum hybrid group, ChaCha20-Poly1305, P-256 key exchange and TLS 1.2 are **not required**. Decision: do not offer them first.
- The group P-256 key exchange also works. X25519 is simpler to write (inferred).

## HTTP (observed)

- HTTP/1.1 works over TLS with `Host` only. nginx 1.31.6 serves it. A request with no `Host` gets `400 Bad Request` with `Connection: close`. A wrong `Host` still gets the site (`200`). An HTTP/1.0 request gets an `HTTP/1.1` response.
- `http://staging.app.8vox.ai/<path>?<query>` returns `301` with `Location: https://staging.app.8vox.ai/<path>?<query>`, an HTML body (169 bytes) and `Content-Length`. The redirect is on the same host.
- `/` and `/login` both return `200` for a request with no session. The "redirect to login" in the acceptance journey is client side (JavaScript), not an HTTP redirect. `bare` will see a `200` page, and JavaScript must do the redirect. (observed for `/` and `/login`; the `/agents` page was not read.)
- Persistent connections work. The tool sent 315 requests on 1 connection. The server may close it (`Connection: close` appears when the client asks).
- Pages: `Content-Length` with identity, `Transfer-Encoding: chunked` when compressed. A `404` page is chunked even without `Accept-Encoding`. The 404 page also carries a long `link:` header.
- `HEAD /login` gives the headers and `Content-Length: 8687` and no body.
- Conditional `GET` with `If-None-Match` gives `304` for a static file (weak ETag `W/"…"`) and for `/login` (strong ETag). A `304` has no body.
- `Range: bytes=0-9` gives `206` with `Content-Range`. Static files send `Accept-Ranges: bytes`. `bare` does not need ranges.
- Static files: `Cache-Control: public, max-age=31536000, immutable`, `Last-Modified`, weak `ETag`, `Vary: Accept-Encoding`. HTML: `Cache-Control: s-maxage=31536000, stale-while-revalidate` with `x-nextjs-cache: HIT` and `Vary: RSC, Next-Router-State-Tree, Next-Router-Prefetch, Accept-Encoding`. `/favicon.svg` has `max-age=0`.

### Content encodings (observed)

| Accept-Encoding | Result |
| --- | --- |
| none, `identity` | Plain body with `Content-Length`. |
| `gzip` | `Content-Encoding: gzip`, chunked. |
| `deflate` | `Content-Encoding: deflate`, chunked. The body decoded with the zlib wrapper (RFC 1950) in this tool. |
| `br`, `zstd` | Ignored. The body is plain. The server never sent `br` or `zstd`, for pages or files. |
| `gzip, deflate, br, zstd` (Chrome style) | `gzip`. |

- The server compresses text only (HTML, JS, CSS) and only above a size threshold. Files of 780 bytes or fewer were plain with every header. Files of 1,325 bytes or more were compressed. The exact threshold is **not measured**.
- Fonts (`woff2`) and the SVG are never compressed.
- For all 52 files, gzip and deflate decoded to the identity bytes (SHA-256 equal).
- Superseded on 2026-11-07: `bare` now sends `Accept-Encoding` and decodes gzip, deflate and br (see [resources.md](resources.md#compressed-bodies-and-the-connect-backup)). Original decision: `bare` needs **no decoder** if it sends no `Accept-Encoding`. It then gets identity bytes (observed with `Host` only; `bare` sends `Host` and `User-Agent` only, so equivalent behavior is expected for its request). A decoder (gzip with deflate) is needed only to cut the 1.86 MB of wire bytes to 0.62 MB. Brotli is not needed. A `Content-Encoding` header that `bare` cannot decode must be an error (not shown as text). This is a design choice, not a server rule.

## Resources (observed)

`/login` names 24 references (4 font preloads, 2 stylesheets, 1 script preload, 16 scripts, 1 icon) and 6 inline scripts. `/` names 45 references (4 font preloads, 3 stylesheets, 1 script preload, 36 scripts, 1 icon) and 7 inline scripts. The union with the lazy chunks is 52 files. Total identity size of the files that returned `200`: 1,856,571 bytes (scripts 1,633,246 in 42 files; CSS 122,335 in 4 files). Wire size with gzip: 622,491 bytes.

- Scripts have `async`. The order of execution is therefore **not** document order. The plan said "in document order where required"; Next.js does not need it. `webpack-*.js` and `main-app-*.js` boot and the others register chunks into `self.webpackChunk_N_E`. The polyfills script has `noModule` (a browser with modules skips it).
- Stylesheets have `data-precedence="next"` and no `media`. They come before the scripts in the head.
- Inline scripts push data to `self.__next_f` (React server component payload). The 6 inline scripts on `/login` carry that payload. They need JavaScript (S2).
- Lazy chunks: the webpack runtime (`webpack-4af263207fca128f.js`) has `b.p="/_next/"` and maps chunk ids 167, 3083, 3299, 6231 and 9041 to files, and one CSS file (`static/css/32df9a4d493f8b0f.css`). The tool read these names from the runtime.
- **Failure observed:** `static/chunks/3299.722ab16035874b78.js` returns `404` (text/html, 8,377 bytes, `no-store`). The runtime names it, but the server does not have it. A bare load that follows the runtime map will hit this `404`. Cause is not known (stale build, or the chunk is only for a path not deployed). A script loader must treat a `404` for a script as an error (do not run the HTML as JavaScript).
- A small file at 249 bytes (`6231…js`) and 471 bytes (`main-app-…js`) are plain with every Accept-Encoding.
- Content type: JavaScript is `application/javascript; charset=UTF-8`, CSS is `text/css; charset=UTF-8`. HTML is `text/html; charset=utf-8`. The page also has `<meta charSet="utf-8"/>`.
- The pages are served with a cache. The HTML hash can change at the next deploy. The file names of static chunks contain a content hash, so a changed file has a new name.

Full hashes and sizes: `tools/fixtures/staging/manifest-2026-11-04.json`. Page hashes: `/` `5cc48bdb446c2d56c405a22fcc99f25ac5efbe350be22cce8d4cfafbbb13f407` (13,315 bytes), `/login` `0bc6a3d32df8098c47b0de2d683ebcbeb5653d67717c39aa67c2bf2bbc12f773` (8,687 bytes). Short hashes (first 16 hex digits of the identity SHA-256) below. The `gzip` column is the wire size with `Accept-Encoding: gzip` (`-` if the server did not compress).

| Path (under `https://staging.app.8vox.ai`) | Kind | Status | Bytes | gzip | SHA-256 |
| --- | --- | --- | --- | --- | --- |
| `/_next/static/chunks/1129-2dd1580f64840d1c.js` | script | 200 | 8336 | 3142 | `ad39e1f057ef761c` |
| `/_next/static/chunks/1188-8e6e055b19d2d633.js` | script | 200 | 2894 | 1295 | `59af6c7e94c5d2ba` |
| `/_next/static/chunks/1194-52df7b0b1754deba.js` | script | 200 | 9724 | 3894 | `1381b22f29e5320a` |
| `/_next/static/chunks/1668-dde5a4e7189a32d1.js` | script | 200 | 13609 | 5158 | `b7de3513a71109d1` |
| `/_next/static/chunks/1672-d3e78ad3e3312578.js` | script | 200 | 28361 | 6743 | `d5ef5e71aeac6128` |
| `/_next/static/chunks/1749-b6930d55eceee9a4.js` | script | 200 | 141630 | 45111 | `a32e2fedc344bd8d` |
| `/_next/static/chunks/1a258343.9598bf0e00059502.js` | lazy-script | 200 | 81644 | 23554 | `eac0a54008b0dd8a` |
| `/_next/static/chunks/2117-562f130d3e4e3a40.js` | script | 200 | 124727 | 32001 | `93552ef42437efdd` |
| `/_next/static/chunks/2361-33efdfda5a1cbc78.js` | script | 200 | 16484 | 6172 | `98b3f5ea3d96f2fa` |
| `/_next/static/chunks/2465-a753c447a621e98d.js` | script | 200 | 23450 | 6866 | `df37777f4e18846f` |
| `/_next/static/chunks/2545-c6f2d01812f9161c.js` | script | 200 | 391134 | 100719 | `b604d99d57ee5376` |
| `/_next/static/chunks/2600-45d65efe29c560d9.js` | script | 200 | 8826 | 3905 | `7f49b0189ea27e4c` |
| `/_next/static/chunks/3083.2e933fe3e6b915d3.js` | lazy-script | 200 | 34893 | 7313 | `0469b3380ea06648` |
| `/_next/static/chunks/3299.722ab16035874b78.js` | lazy-script | 404 | 8377 | 3316 | `0f958631543fc5df` |
| `/_next/static/chunks/3362-41a7a45f11e731e6.js` | script | 200 | 11752 | 4470 | `b31bb027331b1ef2` |
| `/_next/static/chunks/3436-977a2cc38588153a.js` | script | 200 | 7477 | 2819 | `44296331a2d09d80` |
| `/_next/static/chunks/4114-95c721ce51e9b561.js` | script | 200 | 10578 | 3146 | `85b708e0bb46a95a` |
| `/_next/static/chunks/4438-1f32b8bceb8eba13.js` | script | 200 | 34029 | 9355 | `4b9eeef9173ca81c` |
| `/_next/static/chunks/4531-1179243f7d637b0f.js` | script | 200 | 6677 | 2784 | `d718e24e2f896f0a` |
| `/_next/static/chunks/469-29e950465fea3f63.js` | script | 200 | 24771 | 8030 | `e951ad1efcfc91c8` |
| `/_next/static/chunks/5716-d066932302afae24.js` | script | 200 | 10055 | 3813 | `787bfec760d843e5` |
| `/_next/static/chunks/6205-e87ee98c821e2b88.js` | script | 200 | 22756 | 8663 | `76fd3f77155c8407` |
| `/_next/static/chunks/6231.594438b7aa1a0af8.js` | lazy-script | 200 | 249 | - | `3594376b610ea3e8` |
| `/_next/static/chunks/6792-d0767c4435f70e2b.js` | script | 200 | 15769 | 5604 | `2fdd93bc6f312d3b` |
| `/_next/static/chunks/7580-1753610d4bbb10ea.js` | script | 200 | 9173 | 2986 | `28d20a61d771dcab` |
| `/_next/static/chunks/7648-2a3fd33abc885c95.js` | script | 200 | 26207 | 8767 | `2e7e1551f9c4956b` |
| `/_next/static/chunks/8265-32fbc40f455ad75f.js` | script | 200 | 21097 | 7094 | `7718d04bb0597073` |
| `/_next/static/chunks/8841-9622c9bd46bdec1c.js` | script | 200 | 9402 | 3440 | `a560f010fe048275` |
| `/_next/static/chunks/8994-4812622e09ebac50.js` | script | 200 | 34784 | 11725 | `1691271de0e03346` |
| `/_next/static/chunks/902-0928058a70232f07.js` | script | 200 | 20253 | 6833 | `f29f00ecf3678088` |
| `/_next/static/chunks/9041.fc6c8a1f0df66e20.js` | lazy-script | 200 | 94353 | 32505 | `fde2c9a38187d74b` |
| `/_next/static/chunks/9652-59de272f730e6a59.js` | script | 200 | 13439 | 4996 | `10b0c4308eae2581` |
| `/_next/static/chunks/9919-e1805abfea6458fb.js` | script | 200 | 16397 | 5529 | `f20b4d319587786a` |
| `/_next/static/chunks/app/(dashboard)/layout-ed1a4a194b15735a.js` | script | 200 | 30167 | 10395 | `915922a973a62f82` |
| `/_next/static/chunks/app/(dashboard)/page-c2002f1614229541.js` | script | 200 | 23195 | 7917 | `7ff711fbd3c5f914` |
| `/_next/static/chunks/app/global-error-5befa00322d7ba32.js` | script | 200 | 780 | - | `3015470d81994088` |
| `/_next/static/chunks/app/layout-9399aba19695dc17.js` | script | 200 | 3518 | 1551 | `fe175aed4bae5e58` |
| `/_next/static/chunks/app/login/page-e12b02b3b605f694.js` | script | 200 | 8316 | 3334 | `52c02990601a6eb3` |
| `/_next/static/chunks/app/not-found-0785627d2a85257a.js` | script | 200 | 1325 | 722 | `41e56d17f7863c94` |
| `/_next/static/chunks/fd9d1056-41ce205d8e0c6ad6.js` | script | 200 | 172837 | 53744 | `865cad64a44a0c49` |
| `/_next/static/chunks/main-app-5294d1645fe5526e.js` | script | 200 | 471 | - | `5fbb88ecc6a5304f` |
| `/_next/static/chunks/polyfills-42372ed130431b0a.js` | script | 200 | 112594 | 39627 | `0973c1d64c88adc8` |
| `/_next/static/chunks/webpack-4af263207fca128f.js` | preload-script | 200 | 5113 | 2310 | `f6cb7dd883949453` |
| `/_next/static/css/32df9a4d493f8b0f.css` | lazy-style | 200 | 15739 | 2648 | `36fa29d0d473a106` |
| `/_next/static/css/c9673e01f666feef.css` | style | 200 | 104630 | 18485 | `94c8b8386eb8781f` |
| `/_next/static/css/dddf4be8bcb68c22.css` | style | 200 | 367 | - | `2796b7756e393a2a` |
| `/_next/static/css/f550c1958bfdbf7f.css` | style | 200 | 1599 | 469 | `fb9ee4acdf22c81b` |
| `/_next/static/media/06ac09dcb8ec7d56-s.p.woff2` | preload-font | 200 | 14708 | - | `08949f728dc52d52` |
| `/_next/static/media/a7358d2e3dd919a9-s.p.woff2` | preload-font | 200 | 45712 | - | `e2291e842cf5af16` |
| `/_next/static/media/dece0821e8ed059f-s.p.woff2` | preload-font | 200 | 14888 | - | `01d285447409c8a5` |
| `/_next/static/media/fe2d7f0f4dd64702-s.p.woff2` | preload-font | 200 | 25384 | - | `531d2365a3dbd2da` |
| `/favicon.svg` | icon | 200 | 298 | - | `ab6dc45533df2198` |

## Local measurements (offline)

`parse_manifest` on the saved login page repeated N times (release is not relevant; Python):

| N | Bytes | References | Time |
| --- | --- | --- | --- |
| 1 | 8,687 | 24 | 0.001 s |
| 10 | 86,870 | 240 | 0.005 s |
| 100 | 868,700 | 2,400 | 0.044 s |
| 1,000 | 8,687,000 | 24,000 | 0.374 s |
| 5,000 | 43,435,000 | 120,000 | 2.006 s |

The time grows with the input (about 20 MB/s). The live fetch was 315 requests in 10 s on one connection. This is not a load test and the service was not loaded. The network cost is separate from engine cost.

## First failing bare HTTPS check

Raw bytes (before 2026-11-07; `bare` now also sends `Accept-Encoding`): `bare` sends `GET <path> HTTP/1.1`, `Host`, `User-Agent: bare/0.1` and no `Accept-Encoding`. The server answers this shape with identity bytes and `Content-Length` (observed with a Host-only request). So the first `https://` fetch needs TLS and the existing HTTP parser. It does not need a decoder.

Today `target/release/bare --source https://staging.app.8vox.ai/login` fails with `error: only http:// is supported`. `http://staging.app.8vox.ai/login` fails the same way after the `301` (the redirect goes to `https://`). Both are observed on 2026-11-04.

`sh tools/staging_https_check.sh` is the check. It builds `bare`, fetches `/login` and compares the SHA-256 of the output (without the final newline that `--source` adds) with the manifest value `0bc6a3d3…c2f773`. It exits 1 today. It is not in `cargo test` (it needs the network, and it fails until S1 topic 4). Closing condition for S1 topic 4: this script prints `ok`, with certificate checks on.

A fixed hash can break at the next staging deploy. If the hash differs, run `fetch` again, compare the new manifest, and update the manifest file. That is a data change, not a `bare` bug.

## Unknowns

- The exact compression size threshold. TLS 1.0 and 1.1 (local OpenSSL). Other server names.
- Whether `/agents` and API calls need more request headers (S1 topic 5). The API host (if not the same origin) was not inspected.
- Whether the `3299` chunk is loaded by any page path.
- Behavior with a very old or very large request. Not tested on purpose (no load, no abuse).

# Networking

The `net` crate turns a URL into the bytes of a page. The `tls` crate gives it an encrypted stream. Both are written from scratch.

## URL

The `url` crate parses URLs by hand, with IDNA for international host names. It follows the WHATWG URL standard.

## DNS

`net` does not ask the OS resolver. Its own client reads the hosts file, keeps a cache of answers for as long as each answer allows, and asks the name servers. `localhost` never leaves the machine.

## HTTP/1.1

- Persistent connections (keep-alive), with a pool for each origin.
- Redirects, up to 20.
- A reply cache that follows RFC 9111: freshness, validation with `304`, and invalidation by a successful unsafe request.
- `Content-Encoding`: `gzip`, `deflate` and Brotli (decode only).
- A cookie jar (RFC 6265 and its draft update), shared by the network and `document.cookie`.
- Tracker and analytics hosts on a block list are refused before DNS. `BARE_TRACKERS=allow` turns the list off.

## HTTP/2

An HTTP/2 client (RFC 9113) with HPACK. One connection for each origin carries many requests at once, each on its own stream. A reader thread waits on the socket, and a writer takes the stream only for one write. A request waits when the server's stream limit is full.

## TLS

- TLS 1.3 (RFC 8446) with `TLS_AES_128_GCM_SHA256` and X25519, and TLS 1.2 (RFC 5246) with ECDHE and AES-128-GCM, over ECDSA or RSA certificates.
- ALPN offers `h2` and `http/1.1`.
- The crypto is in the crate: SHA-1, SHA-256, SHA-384, SHA-512, HMAC, HKDF, AES-128-GCM, X25519, P-256, RSA, big integers, and the X.509 and DER code for chain validation. SHA-256 uses the x86-64 SHA instructions when the CPU has them.
- Trust roots come only from the OS certificate store or an explicit local file. Nothing comes from the peer.
- Known-answer and independent-oracle tests check the crypto, including the Wycheproof ECDSA vectors.

This TLS code has not had an outside audit and does not promise constant-time behavior. Do not use it to protect real secrets.

## WebSocket

A WebSocket client (RFC 6455) runs on its own HTTP/1.1 connection. It offers `permessage-deflate` (RFC 7692) and inflates compressed server messages up to a size cap. It answers ping with pong, closes on a protocol error with the right code, and checks text for UTF-8. Each socket has a reader thread and a writer thread, so a slow peer never blocks the page. An open socket does not hold the page busy.

## Downloads

A download streams to a file in 64 KiB pieces, so memory stays flat. It runs on its own threads with resume and retry (`Range` and `If-Range` from the last byte on disk), parallel byte ranges that split the largest remaining range when a connection is free or stalls, a queue with pause, resume and cancel, a running-job limit, a speed limit, and live progress. A download stops at 1 GiB.

## Disk cache

A shared disk tier stores public replies, one file for each reply, named by the SHA-256 of the request key. Each file has a magic number, a version, lengths and a CRC. The cache keeps the decoded body, so a hit skips decompression. The size cap is 256 MiB, and the oldest entries leave first. An I/O failure is a cache miss. Replies marked private or `no-store`, replies that set cookies, and requests with caller headers are never written. `BARE_CACHE_DIR` with an empty value turns the cache off.

## Record and replay

`BARE_NET_RECORD=DIR` records every reply. `BARE_NET_REPLAY=DIR` answers every request from the recording, with no DNS and no socket. A virtual clock makes a journey do the same work in each run. This is how [performance.md](performance.md) measures pages that do not depend on a live site.

## Sessions

A session exports and imports its cookies, `localStorage` and `sessionStorage` as a file (`save-state`, `open ... state=`). The file format is compatible with the `storageState` of Playwright.

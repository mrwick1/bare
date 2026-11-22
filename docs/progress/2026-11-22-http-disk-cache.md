# Design

Use memory first, then one disk file per public GET representation. The file name is the SHA-256 of the request key. The key includes the URL and the client-generated Accept-Encoding value. Keep the full key in the file to verify the lookup. Use a fixed header with magic, version, header and body lengths, and a CRC of the key, metadata and body. The metadata holds the wall-clock store time, freshness lifetime, and response headers. A stale file keeps its ETag and Last-Modified for validation. A clock rollback makes the file stale.

Store the decoded body to skip decompression on a hit. Keep the existing response headers for callers. Send the memory cache's response to a bounded writer queue by reference. Write a temporary file in the cache directory. Close it before rename. The writer finishes queued writes when the last client drops.

Use the file modification time for disk LRU. A small, locked file tracks the total size of committed entries for all writers. Clear the count before a mutation. Store the count with its inverse. Rebuild it after an interrupted mutation or an invalid count. Evict a batch of the oldest entries when a write would cross 256 MiB. Reads and ordinary writes take O(key + headers + body) time. A read allocates one body. A write needs O(headers) extra space. Eviction uses O(entries) space and O(entries log entries) time.

Chromium's Simple Cache uses individual entry files and a hash index for size and last use. It evicts from a low watermark and persists the index. A full index would also let bare check membership without an open, but each process would need to keep its index current. [Chromium Simple Cache](https://chromium.googlesource.com/chromium/src/net/+/HEAD/disk_cache/) and [Simple Index](https://chromium.googlesource.com/chromium/src/net/+/HEAD/disk_cache/simple/simple_index.cc).

A single append-only store would make writes sequential. An offset index could locate replies in O(1), with O(entries) memory. To reclaim old space it would need compaction or a free-space map, and recovery would have to validate a shared file. Independent entry files keep recovery local.

## Scope

The bare executable enables the disk tier for `--run` and HTTP windows. Set `BARE_CACHE_DIR` to an empty value to turn it off. The network library takes a directory from the caller. The driver and MCP create and pass their own clients. Their test sessions use memory only.

Write only replies marked public or immutable from a GET without caller headers or jar cookies. Exclude private, no-store, Set-Cookie, unknown encodings, and other Vary values. Record and replay and custom trust roots use memory only.

Opaque preloads still drop their bodies. Keeping them needs buffering and a budget for concurrent preloads. The image loader still keeps its eligible complete replies.

## Measurements

archMachine, AMD Ryzen 9 5900HX, Linux, release builds. Base is `14237fe1`. The comparison build is `c1c41286`. A local HTTP/1.1 server serves a 136-byte page with the staging webpack runtime (2,300 bytes gzip, 5,113 decoded) and the background CSS fixture (575 bytes). The page sends no-store. The two assets send public, immutable and max-age. Each round starts a new process with an empty application cache, then starts another process with that cache. Three runs per mode. Every run settled and ran one script without a load error or exception.

| Mode | Requests | Wire body bytes | User instructions, median (range) | Ready ms, median (range) | Peak RSS KiB, median (range) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Main | 3 | 3,011 | 7,331,511 (7,331,285–7,332,640) | 85.116 (83.910–95.609) | 16,652 (16,596–16,676) |
| Cold | 3 | 3,011 | 7,441,942 (7,441,771–7,450,514) | 84.666 (84.238–85.611) | 16,896 (16,564–16,956) |
| Warm | 1 | 136 | 7,169,062 (7,168,675–7,169,164) | 2.412 (2.403–2.680) | 16,820 (16,816–16,872) |

Warm uses 3.67% fewer instructions than cold. Cold uses 1.51% more than main. Peak RSS ranges overlap. `perf stat -e instructions:u` includes the small process wrapper that reports peak RSS from `wait4`. The ready time is the `bare --run` trace wall time through session settle. Host load was 18.42 at the start, so the wall times describe this fixture only.

Three separate trace runs recorded six cache writes. Each took 0.150–0.172 ms on the writer thread. A smoke run with `BARE_CACHE_DIR=` fetched all three replies and left the temporary XDG fallback directory absent. With the override unset, a temporary `XDG_CACHE_HOME` received two entries under `bare/http`.

## Tests

- Linux. 96 HTTP tests, 30 resource tests and 11 preload tests passed. `cargo check --locked -p bare -p bare-js -p bare-js-api` passed.
- Windows VM WIN-P140KL4QVMK. Seven narrow library tests passed, including concurrent writers, an existing destination, and LRU touches. The whole-package cross-build stopped in the existing `https_load` example at the undefined symbol `clock_gettime`. The library-only cross-build linked and ran.
- Not tested. Live public origins, a GUI launch, macOS, driver or MCP integration suites, the full cost matrix, or test262.

## Cost debt

The 28 resource, preload, stylesheet, fetch and XHR rows passed. Mean glibc change was +0.084%. The largest change was +0.366% for `resources_load large 1`. No row went over +0.5% in the glibc pass. The rule skipped jemalloc and single-codegen-unit runs.

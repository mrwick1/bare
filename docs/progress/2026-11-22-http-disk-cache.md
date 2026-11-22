# Design

Use memory first, then one disk file per public GET representation. The file name is the SHA-256 of the request key. The key includes the URL and the client-generated Accept-Encoding value. Keep the full key in the file to verify the lookup. Use a fixed header with magic, version, header and body lengths, and a CRC of the key, metadata and body. The metadata holds the wall-clock store time, freshness lifetime, and response headers. A stale file keeps its ETag and Last-Modified for validation. A clock rollback makes the file stale.

Store the decoded body to skip decompression on a hit. Keep the existing response headers for callers. Send the memory cache's response to a bounded writer queue by reference. Write a temporary file in the cache directory. Close it before rename. The writer finishes queued writes when the last client drops.

Use the file modification time for disk LRU. A small, locked file tracks the total size of committed entries for all writers. Clear the count before a mutation. Rebuild it after an interrupted mutation. Evict a batch of the oldest entries when a write would cross 256 MiB. Reads and ordinary writes take O(key + headers + body) time. A read allocates one body. A write needs O(headers) extra space. Eviction uses O(entries) space and O(entries log entries) time.

Chromium's Simple Cache uses individual entry files and a hash index for size and last use. It evicts from a low watermark and persists the index. A full index would also let bare check membership without an open, but each process would need to keep its index current. [Chromium Simple Cache](https://chromium.googlesource.com/chromium/src/net/+/HEAD/disk_cache/) and [Simple Index](https://chromium.googlesource.com/chromium/src/net/+/HEAD/disk_cache/simple/simple_index.cc).

A single append-only store would make writes sequential. An offset index could locate replies in O(1), with O(entries) memory. To reclaim old space it would need compaction or a free-space map, and recovery would have to validate a shared file. Independent entry files keep recovery local.

## Scope

The bare executable enables the disk tier for `--run` and HTTP windows. Set `BARE_CACHE_DIR` to an empty value to turn it off. The network library takes a directory from the caller. The driver and MCP create and pass their own clients. Their test sessions use memory only.

Write only replies marked public or immutable from a GET without caller headers or jar cookies. Exclude private, no-store, Set-Cookie, unknown encodings, and other Vary values. Record and replay and custom trust roots use memory only.

Opaque preloads still drop their bodies. Keeping them needs buffering and a budget for concurrent preloads. The image loader still keeps its eligible complete replies.

## Measurements

archMachine, AMD Ryzen 9 5900HX, Linux, release builds. Base is `65f23e2f`. The comparison build is `91dca387`. A local HTTP/1.1 server serves a 136-byte page with the staging webpack runtime (2,300 bytes gzip, 5,113 decoded) and the background CSS fixture (575 bytes). The page sends no-store. The two assets send public, immutable and max-age. Each round starts a new process with an empty application cache, then starts another process with that cache. Three runs per mode. Every run settled and ran one script without a load error or exception.

| Mode | Requests | Wire body bytes | User instructions, median (range) | Ready ms, median (range) | Peak RSS KiB, median (range) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Main | 3 | 3,011 | 7,331,464 (7,330,796–7,331,496) | 92.815 (92.186–114.069) | 16,844 (16,784–16,888) |
| Cold | 3 | 3,011 | 7,440,885 (7,439,725–7,451,633) | 93.317 (93.137–93.772) | 16,720 (16,568–17,088) |
| Warm | 1 | 136 | 7,169,241 (7,168,634–7,171,543) | 9.522 (8.626–11.582) | 16,884 (16,568–17,284) |

Warm uses 3.65% fewer instructions than cold. Cold uses 1.49% more than main. Peak RSS ranges overlap. `perf stat -e instructions:u` includes the small process wrapper that reports peak RSS from `wait4`. The ready time is the `bare --run` trace wall time through session settle. Host load was 24.66 at the start, so the wall times describe this fixture only.

Three separate trace runs recorded six cache writes. Each took 0.204–0.287 ms on the writer thread. A smoke run with `BARE_CACHE_DIR=` fetched all three replies and left the temporary XDG fallback directory absent.

## Tests

- Linux. 96 HTTP tests, 30 resource tests and 11 preload tests passed. `cargo check --locked -p bare -p bare-js -p bare-js-api` passed.
- Windows VM WIN-P140KL4QVMK. Seven narrow library tests passed, including concurrent writers, an existing destination, and LRU touches. The whole-package cross-build stopped in the existing `https_load` example at the undefined symbol `clock_gettime`. The library-only cross-build linked and ran.
- Not tested. Live public origins, a GUI launch, macOS, driver or MCP integration suites, the full cost matrix, or test262.

## Cost debt

The 28 resource, preload, stylesheet, fetch and XHR rows passed. Mean glibc change was +0.086%. One row needed jemalloc, `resources_load small 1`. It measured +0.547% under glibc and -0.193% under jemalloc. No row exceeded +0.5% under both allocators.

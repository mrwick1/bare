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

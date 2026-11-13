# Design: a download manager (resume, retry, parallel segments, queue)

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-13 against main `8eb90c9c`. Machine: archMachine (16 cores, Wi-Fi link, load average 16 to 23 during the runs).
Scope (owner choice, 2026-11-13): resume and retry, segmented parallel downloads, a queue with controls. Streaming uploads are out of scope.
This document does not change code.

## 0. Summary

Today a download is one HTTP request that streams to a file. A dropped or stalled connection loses the whole file. Nothing retries.

The measurements in section 2 show two facts:

- On this link, more connections do not give a steady speed gain. The last mile is the limit. The best case was 2.6 times (8 connections). The worst case was 3.5 times slower (8 connections).
- Single connections stall. 2 of 6 single-connection runs dropped below 0.7 MB/s (one ran for 622 s at 0.08 MB/s). In the same rounds, split runs finished.

So the main value of splitting here is not raw bandwidth. The main value is that a slow or dead connection stops holding the whole file. The design follows from this:

| Part | Idea | Verdict |
| --- | --- | --- |
| Resume and retry | `Range` + `If-Range` from the last byte on disk, with backoff | Build first. It fixes the stall and the drop for every server that supports ranges. |
| Dynamic segments | Start with one connection. Split the largest remaining range when a connection is free or a segment stalls (work stealing) | Build second. It adds no HEAD round trip and no cost for a small file. |
| Static split | HEAD, then N equal ranges | Do not build. It costs a round trip, and one slow part still holds the file. |
| Queue and controls | Pause, resume, cancel, a running limit, a speed limit, live progress | Build third. Pause and resume reuse the resume path. |

## 1. The current design (from the code)

- `browser_session.rs` `fetch_hop`: a 2xx response that is a download (`downloads::is_download`, or a `download` link) streams into `Downloads::save`.
- `downloads.rs` `Downloads::save`: reads 64 KiB pieces and writes them in order to a new file. At 1 GiB (`MAX_DOWNLOAD`) or on any error, it removes the file. `bytes` is set only at the end, so no live progress exists.
- No request sends `Range`. No code reads `206`, `Content-Range`, `Accept-Ranges` or `If-Range`. No retry.
- `start_downloads` runs a link download with `self.fork().spawn`, a worker of `parallel.rs`. `MAX_WORKERS` is 6 for a client and all its forks. A long download holds one of the 6 workers for its whole time, so it slows page loads of the same session.
- `http.rs` sends `Accept-Encoding: gzip, deflate, br` to a non-loopback host, and decodes the reply. A `Content-Length` of an encoded reply is the encoded size.
- `http.rs` shares one HTTP/2 connection for each server (`Sessions`). Several range requests to an HTTP/2 server go on one TCP connection.
- State: `Running`, `Done`, `Failed(String)`. No pause, no cancel.

Data structures: one `Vec<Download>` behind a `Mutex`. One request per download. Memory per download: one 64 KiB buffer.

## 2. Measurements

Tool: `curl --http1.1`, one process for each range, ranges of equal size, output to `/dev/null`. Script: `split.sh` in the session scratchpad (not in the repository). 3 rounds, in the order n = 1, 4, 8 in each round. The link is Wi-Fi to a home router. Wall time is not reliable on a busy machine, but the link is the bottleneck here, not the CPU.

kernel.org, `linux-6.12.tar.xz`, the first 100 MiB:

| n | round 1 | round 2 | round 3 |
| --- | --- | --- | --- |
| 1 | 11.39 MB/s | 0.65 MB/s (162 s) | 4.80 MB/s |
| 4 | 7.06 MB/s | 3.93 MB/s | 2.41 MB/s |
| 8 | 2.53 MB/s | 4.95 MB/s | 4.27 MB/s |

proof.ovh.net, `1Gb.dat`, the first 50 MiB:

| n | round 1 | round 2 | round 3 |
| --- | --- | --- | --- |
| 1 | 9.55 MB/s | 101.60 MB/s | 0.08 MB/s (622 s) |
| 4 | 10.22 MB/s | 73.45 MB/s | 7.28 MB/s |
| 8 | 24.98 MB/s | 73.96 MB/s | 29.82 MB/s |

Spread is very large. Hypothesis: the stalls are packet loss on the Wi-Fi link, where one TCP connection backs off for a long time. Hypothesis: the 101 MB/s run hit a warm cache near the server. I did not check either.

Not measured yet: the bare baseline. `bare-drive` has no download option, so a bare run needs `bare-mcp`. Step 1 of section 6 adds the fixture for it.

## 3. How others do it

- Firefox and Chrome resume a download with `Range` and `If-Range` (RFC 9110, section 14.2 and 13.1.5). To verify: the exact validator rules of each.
- Chrome has a "parallel downloading" feature behind a flag. To verify: its default slice count, minimum slice size and whether it is on by default.
- aria2: `--split` (default 5), `--max-connection-per-server` (default 1, at most 16), `--min-split-size` (default 20 MiB). It does not split a range smaller than 2 times `--min-split-size`. To verify against the current manual.
- IDM (vendor claim): up to 32 connections, "dynamic segmentation": a free connection takes half of the largest remaining part. It does not do a fresh HEAD first.
- RFC 9110: a server that does not support ranges sends `200` with the full body. A changed file with `If-Range` also gets `200`. `416` means the start is past the end.

## 4. Options

| | A. Resume only | B. Static split | C. Dynamic split (work stealing) |
| --- | --- | --- | --- |
| Data | one `pos` per file | N fixed `(start, end, pos)` | a list of `(start, end, pos)` that grows |
| Start | the first GET | HEAD, then N GETs | the first GET; extra GETs only when the size is known and large |
| Stall | retry from `pos` after a time-out | one slow part holds the file | split the slow part; a free connection takes its second half |
| Small file | 1 request | N + 1 requests | 1 request |
| Memory | 64 KiB | N × (64 KiB + connection state) | same as B, only while N runs |
| Writes | append | write at offset | write at offset |

C does the work of A on each segment. A is step 1 of C. B costs a round trip and does not fix a stall, so C replaces it.

## 5. The chosen design (C)

### 5.1 Data structures

```text
Job      { id, url (after redirects), name, path, total: Option<u64>, validator: Option<Validator>,
           segs: Vec<Seg>, state, done: AtomicU64, limit: Option<u64 bytes/s> }
Seg      { start, end (exclusive), pos, conn: Option<worker>, last_progress: Instant, tries: u8 }
Validator::{ ETag(strong only), LastModified }
State    ::{ Queued, Running, Paused, Done, Failed(why), Cancelled }
```

`Downloads` keeps its shared list. It holds `Job`s in place of `Download`. The public `Download` view stays for the MCP tool and `bare-drive`.

### 5.2 The first request

1. The first GET runs as today. Its body is segment 0, from 0 to `total` (or to the end of the stream when `total` is unknown).
2. The job may split only when all of these hold: status `200`, `Accept-Ranges: bytes`, a known `Content-Length` of at least 2 × `MIN_SPLIT`, no `Content-Encoding`, method `GET`, and a validator (a strong `ETag`, else `Last-Modified`).
3. Else the job is single-stream. It can still resume when the server sends `Accept-Ranges: bytes` and a validator.

The file is made at its full size first (`set_len`). Each segment writes at its own offset: `FileExt::write_at` on Unix, `seek_write` on Windows. The core stays OS-independent behind one small function.

### 5.3 Splitting

A free connection slot takes the segment with the most bytes left. When that is at least 2 × `MIN_SPLIT`, the segment ends at its middle, and a new segment takes the second half with `Range: bytes=mid-(end-1)` and `If-Range`. The old connection keeps reading. It stops when `pos` reaches the new `end` and gives its connection back. Start values: `MIN_SPLIT` = 4 MiB, at most 8 connections for each job and 16 in total. The benchmark in section 6 sets the final values.

A stall: a segment with no new byte for 10 s, or with less than 1/8 of the job's median segment speed for 10 s, is split the same way. A segment with no byte for 30 s closes its connection and retries.

Every range request:

- sends `Accept-Encoding: identity`, so the bytes match the offsets;
- uses a new TCP connection. An HTTP/2 server would otherwise put all segments on one connection, which gives no gain. `http.rs` gets one request option for this;
- checks the reply: `206` with `Content-Range` that starts at the asked byte and has the same total. Else the segment fails.

### 5.4 Retry and resume

- A segment that fails retries from its `pos` after 1, 2, 4, 8 and 16 s, with up to 5 tries. A `Retry-After` header sets a longer wait.
- `200` to a request with `If-Range`: the file changed. The job drops all segments and starts again from byte 0 on one connection.
- `401`, `403` or `410` to a range request: the URL may be signed for one use. The job stops splitting. It keeps the segments that run.
- `416`: the job checks `total` with the reply and fails when they differ.
- A job without a validator does not resume. A failure restarts it from 0, once.

### 5.5 Queue and controls

- At most 3 jobs run at once; more wait as `Queued`.
- Pause closes the connections and keeps the file and the segment list. Resume sends range requests from each `pos`.
- Cancel closes the connections and removes the file.
- A speed limit is a token bucket for each job, read before each 64 KiB write.
- Progress: `done` is an `AtomicU64` that each write adds to. Speed is a moving average over 2 s. No lock on the write path.
- `bare-mcp` `downloads` gets `op`: `list` (default), `pause`, `resume`, `cancel`, `limit`. `list` shows bytes, total, speed and connection count.

### 5.6 Threads

Download segments use their own threads, not the `parallel.rs` workers, so a download never takes a slot from a page load. A segment thread is blocking, the same model as today. Threads end with their segment.

### 5.7 What stays

- `MAX_DOWNLOAD` (1 GiB) stays. It protects the disk, not a slow algorithm. Open question: make it a setting.
- Names, `sanitize`, `name (1).ext`, `blob:` and `data:` downloads do not change.
- A file is `name.part` until it is done, then it is renamed. A failure or a cancel removes it.

## 6. Benchmark and checks

Fixture (Python, loopback): a server that serves a file with ranges and validators, a byte rate for each connection, and a stall after K bytes on chosen connections.

| Case | Measure | Pass |
| --- | --- | --- |
| 256 MiB, 2 MB/s for each connection, no stall | wall time, 3 runs | 8 connections: at least 6 times faster than 1 |
| 256 MiB, one connection stalls at 50% | wall time | finishes within 1.5 times the no-stall time |
| 256 MiB, server drops all connections at 30% | bytes on the wire | at most 1 MiB fetched twice |
| file changes during the download (new `ETag`) | content | file equals the new file |
| no `Accept-Ranges` | requests | 1 request, file correct |
| 16 KiB file | requests, instructions | 1 request; instructions within 2% of today |
| any case | peak RSS | at most (connections × 64 KiB) + connection state above today |
| login journeys | instructions | no change (downloads are off that path) |

Instructions with `perf stat -e instructions`, 3 runs, spread given. Wall time only on an idle machine. Real servers (kernel.org, OVH) as a second check, with the spread, since the link is noisy.

## 7. Build steps

1. Resume and retry on one stream. Positional writes, `.part`, live progress, own threads. The fixture and the bare baseline.
2. Dynamic split and stall split. The HTTP/2 new-connection option in `http.rs`.
3. Queue, pause, resume, cancel, speed limit, the MCP `downloads` ops.

Each step is one PR with its benchmark rows in `docs/progress.md`.

## 8. Open questions for the owner

1. Scope: the goal of 2026-11-10 is login and E2E only. No journey fails on downloads today. Build now, or keep this design until a journey needs it?
2. Resume after a restart of `bare-mcp`: write the segment list to a small file next to `name.part`? It adds a file format to keep. Without it, a pause lasts only while the process runs.
3. `MAX_DOWNLOAD`: keep 1 GiB, or make it a setting?
4. Start values: `MIN_SPLIT` 4 MiB, 8 connections for each job, 16 in total, 3 jobs at once. Step 2 measures them.

Not tested: every claim about Chrome, Firefox, aria2 and IDM in section 3; the causes in section 2; any bare run.

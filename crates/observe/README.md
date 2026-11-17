# bare-observe

This crate provides the phase 1A collector. It does not sample processes or instrument the engine.

Call `Config::from_env()` and `Collector::start(config)` once per process. Keep the owner until process shutdown. Clone `Collector::handle()` for producers. Call `Collector::shutdown()` to drain accepted records, write terminal status, flush, and join the worker. Drop also shuts down. Shutdown waits for active field closures and sink I/O to return.

```rust
use bare_observe::{Collector, Config, Field, Ids, Level, Scalar};
let collector = Collector::start(Config::from_env().unwrap()).unwrap();
let producer = collector.handle();
producer.emit(Level::Info, "layout", "layout_end", Ids::default(), || [
    Field("duration_ns", Scalar::U64(120)),
]);
let status = collector.shutdown();
assert!(status.write_error.is_none());
```

`Handle::default()` is disabled. Disabled emission returns before evaluating fields, allocating, reading a clock, or starting a worker. Category and level filters also skip field construction. `Emission` reports immediate acceptance or rejection. `Handle::status()` and `Collector::status()` report cumulative drops, stop reason, and writer error. `Queued` means accepted into the queue; it does not promise a successful disk write.

Enabled configuration is strict:

| Variable | Values or default |
| --- | --- |
| `BARE_OBSERVE` | `off` (default), `metrics`, `trace` |
| `BARE_OBSERVE_FILE` | Required nonempty path when enabled; create new only |
| `BARE_OBSERVE_INTERVAL_MS` | Flush interval, 1 through 60000; default 1000 |
| `BARE_OBSERVE_CATEGORIES` | Optional CSV of nonempty categories; trim whitespace; each at most 64 bytes |
| `BARE_OBSERVE_LEVEL` | `error`, `warn`, `info` (default), `debug`, `trace` |
| `BARE_OBSERVE_MAX_BYTES` | At least 1024 bytes; default 67108864 |

Off mode ignores the other controls. Legacy statistics switches do not enable this collector. Both enabled modes use the same transport. Integration uses `Config::mode()` to choose metrics or trace events and `Config::interval()` for sampling. This crate only schedules flushes. Files use mode 0600 on Unix, subject to a stricter process umask.

Each JSONL record has `version: 1`, monotonic `elapsed_ns`, `pid`, an opaque Rust `thread_id`, `level`, `category`, `event`, and typed `fields`. Optional `session_id`, `document_id`, `request_id`, and `step_id` are numeric. Event records identify the producing thread; terminal status identifies the worker. Fields accept unsigned integers, signed integers, finite floats, booleans, and explicit null. Unknown measurements must use null or omit the field. Use units in field names.

Names use static strings. Use fixed literals, never leaked page data. Fields cannot carry string payloads. The collector never captures bodies, headers, cookies, page source, console output, or user input automatically.

The queue holds at most 256 records. Each serialized record, including its newline, is at most 4096 bytes. Events allow at most 32 unique fields; keys are nonempty and at most 64 bytes. Category names are at most 64 bytes; event names are at most 128 bytes. Oversize, invalid, and queue-full events increment the drop count. Producers use nonblocking `try_send` and never perform sink I/O. Field closures run on the producer, so callers must keep them short.

The writer reserves 512 bytes for `observe/collector_status`. It stops at the first record that cannot fit before that reserve, drains pending records as drops, writes the terminal reason `file_cap`, and flushes. Normal shutdown uses reason `shutdown`. A failed writer sets `WriteFailed` and exposes the I/O error through status. `Write::write_all` handles partial writes and interrupted writes. An unrecoverable error or process crash can leave an incomplete final line; discard that line when reading. No terminal write is attempted after a record write fails. A flush does not imply `fsync` durability.

`Collector::with_writer` accepts an owned `Write + Send + 'static` sink for tests or embedding. This bypasses file creation only. It retains all queue, record, flush, and cap behavior.

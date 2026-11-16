# Engine monitoring (partial integration)

Phase 2 is incomplete. Do not treat this branch as accepted monitoring support.
The local journey test passes after adding retained and incremental style hooks. Remaining coverage and acceptance work is listed below.

## Invocation

Build with symbols: `cargo build --profile profiling --workspace`.
The profiling profile inherits release, enables debug symbols, and sets `strip = false`.
The normal release profile is unchanged.

Example: `BARE_OBSERVE=trace BARE_OBSERVE_FILE=/tmp/journey.jsonl cargo run -p bare-drive -- --quiet journey.steps`.
Use `metrics` for process samples and engine counters. Use `trace` to add durations.
The default is `off`. Off mode creates no capture or monitoring threads.
Each executable initializes an owner. Normal returns drain the collector.
The bare and bare-drive exit paths shut down the owner before `process::exit`.
Configuration and file-open errors print to stderr and exit with code 2.
Writer and flush errors print to stderr at shutdown. MCP stdout remains protocol output.
A crash, forced termination, or blocked sink can prevent completion of shutdown.

Suite workers append `.pid-PID` to `BARE_OBSERVE_FILE`.
The suite parent retains the exact configured path.
Each path uses create-new semantics. Existing files cause an error.
This rule also applies when a suite uses `perf` to launch workers.

See [the collector contract](../crates/observe/README.md) for all controls and the JSONL schema.
`BARE_OBSERVE_INTERVAL_MS` controls both process sampling and writer flush intervals.
Process samples have no session or document attribution.
Each sample includes optional RSS, peak RSS, allocator bytes, CPU microseconds, and thread count.
Linux supports procfs counters. Allocator accounting requires glibc.
Other platforms return null for unsupported process counters.
Allocator bytes, resident bytes, and JS accounted bytes measure different scopes.
No complete subsystem memory total is provided.

## Current hooks

Categories are `process`, `engine`, `html`, `js`, `gc`, `style`, `layout`, `paint`, `scheduler`, `network`, `loader`, and `driver`.
Duration fields use `duration_ns`. Trace durations emit at info level.
HTML feed and finish, JS parsing and script/function execution, GC, style entry points, layout, paint, scheduler pumps, and network headers have hooks.
Network connection hooks measure DNS, TCP, and TLS.
JS counters emit at script entry. Browser counters emit at the microtask drain entry.
Engine counters are owning-thread snapshots, not interval samples.
They use existing counters. They do not request collection or layout.
Loader milestones emit at their source. They do not consume legacy trace records.

Driver steps carry process-local numeric session and step IDs.
Network worker jobs copy the calling context into a scoped thread-local guard.
Window jobs copy the calling context across the control handoff.
Browser script, drain, collection, and scheduler entry points add document IDs.
Some asynchronous paths and phase records still lack document or request IDs.
A process sample never inherits these IDs.

Names are fixed literals. Field values are scalars.
The hooks do not emit URLs, headers, bodies, source, console messages, or input text.
Legacy switches keep their existing behavior. A new capture does not enable legacy tracing.
The existing driver still enables its own legacy network records.

## Limits and remaining work

The queue holds 256 records. Each record is limited to 4096 bytes and 32 fields.
The default file cap is 64 MiB, with space reserved for terminal status.
Queue overflow drops records. File-cap exhaustion stops recording.
Terminal status reports drops and the stop reason.
An I/O failure may leave an incomplete final line. Readers must discard that line.
Shutdown waits for sink I/O. Flush does not imply fsync durability.

The local journey test covers style records from retained and incremental cascade entry points.
Phase coverage is not complete. Network body and TTFB phases lack separate unified records.
Scheduler records measure pumps, not every individual task.
Long-page sampling is tested with bounded worker activity in the process facade, not a long script in the engine.
Concurrent-session and worker attribution need stronger integration tests.
GC and paint hook coverage, suite child file naming, error-exit flushing, and header-secret exclusion need fixture tests.
No GUI, non-Linux runtime, authenticated traffic, or measured overhead acceptance was tested.
Phase 3 remains pending. No performance claim is made.

# No count cap on the queue of dynamic scripts

Date: 2026-11-25. Base: main a6e07103.

## Change

- `PageLimits::max_pending_scripts` (256) is removed (`crates/bare/src/browser_loader.rs`). The page no longer stops with `Limit("pending-scripts")` when a task inserts many scripts.
- The queue needs no count cap. Each script element enters the queue once (`started`), and `BrowserLimits::max_nodes` caps the elements. One entry is a node id and a URL.
- The byte cap on queued inline text (`max_pending_script_bytes`) stays.
- Chrome has no such cap.
- Why: after its React root renders, Teams inserts a burst of more than 256 webpack chunk scripts in one task. bare stopped the page, and Teams stayed on "We're setting things up for you".

## Cost

- `tools/js/cost_matrix.sh -a -g 'loader_load|preload_load'`, base a6e07103, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`:

| Row | base | new | change |
|---|---|---|---|
| `loader_load blocking 1` | 7343264 | 7352411 | +0.125% |
| `loader_load blocking 10` | 32145983 | 32141681 | -0.013% |
| `loader_load dynamic 1` | 10666751 | 10663032 | -0.035% |
| `loader_load dynamic 10` | 64681467 | 64666894 | -0.023% |
| `loader_load mixed 1` | 10538344 | 10542007 | +0.035% |
| `loader_load mixed 10` | 63592023 | 63586933 | -0.008% |
| `loader_load modules 1` | 8196241 | 8195302 | -0.011% |
| `loader_load modules 10` | 28811739 | 28812199 | +0.002% |
| `preload_load dups 1` | 4895716 | 4896817 | +0.022% |
| `preload_load font 1` | 4895781 | 4897632 | +0.038% |
| `preload_load script 1` | 5469380 | 5468387 | -0.018% |
| `preload_load script 10` | 15656421 | 15654204 | -0.014% |
| `preload_load style 1` | 5653942 | 5655581 | +0.029% |
| `preload_load style 10` | 24361687 | 24372259 | +0.043% |

- jemalloc was skipped: no row is over the rule under glibc.

## Tests

- New: `browser_loader_queue_tests.rs` (300 inline dynamic scripts and one external script, in order). It fails on main with `Limit("pending-scripts")` and passes with the change.
- The old assertion of the count cap in `browser_loader_tests.rs` is removed. Its test helpers are now `pub(crate)`.
- `t`: all pass.

## Not tested

- A live Teams run with this change: not yet.
- test262 counts: not run. The change touches no Vex code.

## Open (hypothesis)

- bare runs dynamic scripts in insertion order. A slow script at the front holds the scripts behind it. Chrome runs an async dynamic script as soon as it arrives. This may cost Teams time to ready. Not measured.

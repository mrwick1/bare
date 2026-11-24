# Replaced text counts as dead in the DOM text buffer

Date: 2026-11-25. Base: main b3389c9e.

## Change

- The DOM keeps all text in one append-only buffer (`crates/html/src/dom.rs`). A change appends the new value and leaves the old bytes in place.
- Before, only the text of freed nodes counted as dead. The old value of `set_text`, of an attribute set again, and of a removed attribute (one that no host watches) counted as live.
- So `live_text_bytes` grew with each change. `reclaim` never rebuilt the buffer for these bytes. The `dom-text` cap (64 MiB) then stopped the page.
- Now these old values count as dead (`dead_text`). The cap sees only live text, and a rebuild gives the bytes back. A rebuild already dropped these spans: it copies only the spans of live nodes, `removed` and the host pins.
- Why: Teams (React and CSS-in-JS) sets text and attributes again and again. After its shell started to render, bare stopped the page with `Limit("dom-text")`.

## Cost

- `tools/js/cost_matrix.sh -a -g 'attr_load|browser_load mutation|style_load attrs'`, base b3389c9e, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`:

| Row | base | new | change |
|---|---|---|---|
| `attr_load churn 1` | 76210692 | 76215451 | +0.006% |
| `attr_load churn 10` | 718674398 | 719375584 | +0.098% |
| `attr_load live 1` | 18621502 | 18620931 | -0.003% |
| `attr_load live 10` | 86000303 | 86008457 | +0.009% |
| `attr_load react 1` | 169992557 | 169981596 | -0.006% |
| `attr_load react 10` | 1619607509 | 1619659239 | +0.003% |
| `browser_load mutation 1` | 43570153 | 43544749 | -0.058% |
| `browser_load mutation 10` | 442938292 | 442785205 | -0.035% |
| `style_load attrs 1` | 25127153 | 25126521 | -0.003% |
| `style_load attrs 10` | 181590123 | 181600736 | +0.006% |

- jemalloc was skipped: no row is over the rule under glibc.
- Hypothesis: a page that changes text often now rebuilds the buffer sooner, so its peak memory drops. Not measured.

## Tests

- New: `crates/html/src/dom/reclaim_text_tests.rs`. It fails on main (4 015 789 live bytes instead of 2 012) and passes with the change.
- `bare-html`: all pass. `t`: all pass. `bare` tests that match `reclaim`, `attr` or `mutation`: all pass.

## Not tested

- A live Teams run with this change: not yet.
- Peak memory: not measured.
- A removed attribute that a host watches stays out of `dead_text` after the host takes it (`take_removed_attrs`). This is small and unchanged.
- test262 counts: not run.

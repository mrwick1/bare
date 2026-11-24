# Snapshot: fewer refs, quotes and marks (2026-11-23)

## Change
- A link outside the window has no `(offscreen)` state. Other controls keep it. The offscreen pass no longer checks links.
- A plain table row line has no ref: `row: 1 | [e6] | $3.99 | [e7]`.
- Cell text has no quotes. Text that is empty, has `|`, `[`, `"`, `\` or a control character, or starts or ends with a space keeps them.
- A `clickable` with no name, no state and no line below it does not show.
- `row_role` reads a row line with no ref. A diff that counts a run of new plain rows gives no ref range.

## Why
The 2026-11-23 benchmark against flashbrowse split the v0.1.17 outline. On github.com, `(offscreen)` on 85 links was 12 % of the characters, and 7 empty `clickable` lines led the page. On the table fixture, row refs were 7 % and quotes 9 %.

## Results
Machine: archMachine. Load average about 22 (other jobs), so only tokens and coverage count, not time. 3 runs each; all runs gave the same counts, except github (live page) with a spread of 10 tokens.
Tokens are characters / 4. Coverage: words of the Chrome `innerText` truth (2026-11-23 r2 dumps) that the `all` outline holds.

| Page | v0.1.17 all | branch all | Change | v0.1.17 interactive | branch interactive | Coverage |
|---|---|---|---|---|---|---|
| table fixture | 700 | 622 | −11.2 % | 342 | 342 | 100 % → 100 % |
| example.com | 252 | 252 | 0 % | 21 | 21 | 100 % → 100 % |
| github.com | 2468–2476 | 2237–2246 | −9.0 to −9.7 % | 2139–2146 | 1907–1917 | 87.8–88.4 % both |
| Wikipedia (Rust) | 36389 | 36081 | −0.8 % | 3017 | 3018 | 95.9 % → 95.9 % |
| Hacker News | 3476 | 3474 | −0.1 % | 1797 | 1797 | truth invalid (rate-limited) |

Hacker News uses layout tables (a row holds a table), so its rows are not plain and do not change.

## Cost gate
`tools/js/cost_rows.txt` has no rows for `crates/drive/src/snap.rs` or bare-mcp. No row was run.

## Tests
- New: `crates/mcp/tests/snapshot_compact_tests.rs` (3 tests). They failed before the change, except the offscreen test, which first used an inline link (see below).
- Changed: `crates/mcp/tests/snapshot_table_tests.rs` expects rows without quotes.
- Run: `t`, `cargo test -p bare-mcp --test server` (104 passed), `--test snapshot_coverage_tests`, `-p bare-drive --test inline_links`.

## Notes
- An inline `<a>` below the fold (in a `<p>`) was never marked `(offscreen)` before this change; a block link was. Hypothesis: the offscreen pass reads block boxes only. This change does not depend on it.
- Not done: refs numbered per page (affects ref validity), and folding layout tables such as Hacker News.

# Compact table rows in `snapshot mode=all` (#960)

## Change

- A table row of plain cells is one line: `[e376] row: "1" | [e377] | "$1.99" | [e378]`. A cell shows its text and the refs of the elements in it. Those elements follow on their own lines, one level up. Cells lose their own refs.
- An empty cell stays in the row line (`| |`), so the cells keep their columns.
- A row that holds another row (a layout table, as on Hacker News) keeps its lines. A row or a cell with lines below it drops its name when the name comes from its text. A name from `aria-label` or `aria-labelledby` stays.
- `interactive` mode does not change.

## Measurements

Machine: archMachine (16 threads), under load from other runs. Release `bare-mcp`. Before: main at `9af7f582`. After: this branch. One session per run. Each round opens the page and takes `snapshot mode=all max_chars=200000`. Tokens are characters / 4. The before and after runs of a page ran back to back.

| Page | Rounds | Before (tokens) | After (tokens) | Change |
|---|---|---|---|---|
| local 30-row table (link and button per row) | 5 | 1318–1366 | 709–750 | −45% |
| news.ycombinator.com | 3 | 5598–5693 | 3478–3601 | −38% |
| github.com | 3 | 3497–3529 | 3502–3529 | 0% (no tables) |
| en.wikipedia.org/wiki/Rust_(programming_language) | 3 | 38876–39153 | 36370–36672 | −6% |

Coverage: the share of the distinct words of the before output that the after output holds. Local table 40/40. github.com 545/545. Hacker News 358/360. Wikipedia 3007/3025. Each missing "word" is a fragment of a cut name (`Bar…`) or two cell texts glued in an old row name (`firstappeared`). The words themselves are still in the output. The live pages changed between runs, so small differences can come from the page.

## Ref growth during a session

The issue's second point: the count grows over 5 opens of the same page. Verified cause: refs come from one counter of the process, and a ref is never minted again (see the module comment of `crates/drive/src/snap.rs`). The ref numbers get more digits (`e869` to `e1000`). This is the design: a ref of an older page fails as stale and never names another element. This PR does not change it.

## Tests

- New: `crates/mcp/tests/snapshot_table_tests.rs` (2 tests). They failed before the fix.
- `cargo test --locked -p bare-mcp`: pass.
- `cargo test --locked -p bare-drive`: pass, except `survive_limit::a_page_that_aborts_task_after_task_fails_the_step_with_the_reason`. It fails the same way with the `snap.rs` of main, on this loaded machine. It does not take a snapshot.

## Not tested

- Coverage against Chrome's `document.body.innerText` (no Chrome run). The word check compares with the old output only.
- Grids (`role=grid`, `gridcell`): their rows keep the old form.
- Windows: the change is platform-neutral text formatting. Not run on Windows.

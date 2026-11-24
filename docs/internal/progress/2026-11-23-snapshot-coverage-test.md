# Snapshot text coverage test (#962)

## What changed

- 9 fixtures in `tools/fixtures/coverage/` with Chrome truth files (`document.body.innerText`).
- `tools/coverage/truth.py` makes the truth through `flashbrowse mcp` (Chrome). README: `tools/coverage/README.md`.
- Test `crates/mcp/tests/snapshot_coverage_tests.rs`: word recall of `snapshot mode=all` per fixture, floors in `floors.txt`, hidden-marker check.

## Recall on main (5dc8902b), one run, debug build, bare-mcp

| Fixture | Recall | Floor | Missing |
|---|---|---|---|
| form | 0.9474 | 0.94 | `Portugal` (second `<option>`) |
| hidden | 1.0000 | 1.00 | none |
| iframe | 1.0000 | 1.00 | none (Chrome truth has parent text only) |
| list | 1.0000 | 1.00 | none |
| repeated | 1.0000 | 1.00 | none |
| shadow | 1.0000 | 1.00 | none (Chrome `innerText` omits shadow text) |
| table | 1.0000 | 1.00 | none |
| unicode | 1.0000 | 1.00 | none |
| wrappers | 1.0000 | 1.00 | none |

## Found, not fixed

- `visibility:hidden` text shows in `snapshot mode=all` (`HIDDENVISIBILITY`). The test lists it in `KNOWN_LEAKS`.
- Only the first `<option>` of a `<select>` shows; Chrome lists all. This sets the form floor.

## Checks

- Test passes on main.
- Break check: the text push in `crates/drive/src/snap.rs:286` was disabled locally. The test failed with recall 0.00 to 0.86 (form unchanged), then the change was reverted.

## Not tested

- Windows run (the test uses loopback and std only).
- Other outline changes (agent A's `snap.rs` work) after merge.
- Shadow DOM and iframe inner text: Chrome truth does not hold it.

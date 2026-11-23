# Snapshot text coverage

The test `crates/mcp/tests/snapshot_coverage_tests.rs` checks how much page text `snapshot mode=all` keeps.

## How it works

- `tools/fixtures/coverage/<name>.html` is a small static page.
- `<name>.truth.txt` holds the text that Chrome shows: `document.body.innerText`. The file is committed, so CI needs no Chrome.
- The test serves each page, takes the snapshot, and computes the word recall. Recall is the share of truth words (with repeats) that the snapshot holds. The test compares lowercase `\w+` words.
- `floors.txt` holds the lowest recall allowed per page. The test fails when recall is lower.
- A page word that starts with `HIDDEN` marks hidden text. The test fails when the snapshot shows it. `KNOWN_LEAKS` in the test lists the words that bare still shows today.

Chrome omits shadow DOM and iframe content from `innerText`. Those pages test only the text of the parent page.

## Regenerate the truth

Run this after you add or change a fixture:

    python3 tools/coverage/truth.py /path/to/flashbrowse

The script starts `flashbrowse mcp` (it drives Chrome), serves the folder on loopback, and writes one truth file per page. Do not commit the binary.
Keep whitespace between inline elements in a fixture. Chrome joins them without a space, and a joined word counts as missing.

## Add a page or raise a floor

1. Add `<name>.html`. Regenerate the truth.
2. Run `cargo test -p bare-mcp --test snapshot_coverage_tests -- --nocapture`. The test prints the recall and the missing words per page.
3. Add `<name> <floor>` to `floors.txt`. Round the measured recall down to 2 decimals.
4. After a fix raises recall, raise the floor to the new value. Never lower a floor to pass a change.

# FileList global (OFS-53)

## Change

- Bare had no `FileList` global. A page script that reads `FileList`, for example `value instanceof FileList`, threw a `ReferenceError`. A Next.js page then showed its client error page.
- `FileList` is now a lazy global of `browser_web.rs`, like `Blob` and `File`. Its constructor throws `TypeError: Illegal constructor`. Its prototype has the `FileList` tag.
- `input.files` is still an array of `File` objects. So `input.files instanceof FileList` is false.

## Tests

- New: `browser_web_filelist_tests.rs`. It failed before the fix (`typeof FileList` was `undefined`) and passes after it.
- `t` on the touched files: all pass except `downloads::run::tests::a_changed_file_starts_again_from_zero` once. That test passed 3 of 3 runs alone. It does not touch this code.

## Cost

- Not measured. The change adds one accessor function at global setup and one object pair at the first read of a lazy global. Hypothesis: below the +0.5% rule on every row.

## Not tested

- The cost rows and test262.
- Windows. The code has no platform branch.

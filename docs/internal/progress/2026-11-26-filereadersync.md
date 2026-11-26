# FileReaderSync in workers (2026-11-26)

Base: main e29fdefe.

## Problem

- WhatsApp Web does not send a video or an audio file. The spinner turns for ever and Send stays disabled.
- The media worker logs `TypeError: self.FileReaderSync is not a constructor` at `4IVy6GjxbLv.js:719:2185`.
- The code there is `new Uint8Array(new self.FileReaderSync().readAsArrayBuffer(e))`. The WAMedia file system reads slices of the Blob with it.

## Change

- New shim `crates/bare/src/shims/filereadersync.js`: `FileReaderSync` with `readAsArrayBuffer`, `readAsText`, `readAsDataURL` and `readAsBinaryString`.
- `browser_shims.rs` makes it at the first read of the name. It does so only in a realm with `DedicatedWorkerGlobalScope`, so a window has no `FileReaderSync`, as in Chrome.
- A native `bytes(blob)` gives a new ArrayBuffer with the bytes of a Blob or a File, or undefined for any other value. The shim throws a `TypeError` then, with the Chrome text.
- The bytes of a bare Blob are in memory, so a read never gives `NotReadableError`.
- Both worker paths get it: the dedicated worker and the threaded worker of a cross-origin isolated page.

## Tests

- New file `crates/bare/src/browser_worker_filereadersync_tests.rs`, 2 tests: the four reads in a dedicated worker (and `typeof FileReaderSync` is `undefined` on the window), and the four reads in a threaded worker.
- Both failed before the change (the worker never answered). Both pass.
- `t` on the changed files: the tests of `browser_handler_tests.rs`, `browser_streams_shim_tests.rs` and `browser_streams_tests.rs` pass.

## Cost

archMachine, shared with other agents. `tools/js/cost_matrix.sh -J -g streams_load` (the rows of the shim subsystem), 3 runs per row, base e29fdefe against this change.

| Row | glibc | jemalloc |
| --- | ----: | -------: |
| streams_load churn 1 | -0.002% | +0.015% |
| streams_load churn 10 | +0.013% | +0.090% |
| streams_load flow 1 | +0.024% | +0.027% |
| streams_load flow 10 | +0.026% | +0.028% |
| streams_load many 1 | +0.020% | +0.020% |
| streams_load many 10 | +0.020% | +0.033% |
| streams_load pending 1 | +0.026% | +0.030% |
| streams_load pending 10 | +0.031% | +0.032% |

No row is over +0.5%.

## Not tested

- A live WhatsApp video or audio send.
- An unknown encoding label in `readAsText` (it reads as UTF-8, as in `FileReader`).

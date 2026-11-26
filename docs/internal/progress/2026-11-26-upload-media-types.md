# 2026-11-26: uploaded files get media and office types

Branch `upload-media-types`, from `origin/main` c9d46272.

## Why

WhatsApp Web in bare sent a `.mp4` as a plain document with type `application/octet-stream`. The `type` of an uploaded `File` comes from `mime_of` in `browser_web.rs`. Its list had no video, audio or office types.

## What changed

- `mime_of` knows mp4, m4v, webm, mov, 3gp, mp3, m4a, ogg, oga, opus, wav, aac, flac, avif, bmp, doc, docx, xls, xlsx, ppt and pptx. The values are Chrome's on Linux.

## Tests

- New `browser_web_upload_types_tests.rs`. It failed before the change and passes after it.
- `t` passed: 17 tests.

## Cost rows

None. The change adds match arms to a function that runs once per uploaded file.

## Not tested

- The live WhatsApp video send with this change.
- WhatsApp may still send a video as a document: bare cannot decode video, so the preview shows "No preview available" (hypothesis).

# 2026-11-25: pictures with a blob: URL

## Change
- An `<img>` with a `blob:` URL loads from the blob URL store of the document. WhatsApp Web sets each chat picture this way. Before, the URL went to the downloader and failed, so `naturalWidth` was 0 and no picture showed.
- When a script sets `src` (IDL setter, `setAttribute`, `new Image()`), `browser_img.rs` reads the Blob at that time. The page can revoke the URL right after, as Chrome allows. For an `<img>` from the parser, `images.rs` reads the store when it queues the picture.
- A CSS `url(blob:...)` (`background-image`, `mask-image`) reads the store when `css_scan` queues it. A URL that is not in the store counts as failed.
- The loader gets the shared bytes of the Blob (`Arc<[u8]>`, `blob_url_bytes`). There is no copy and no `data:` URL.

## Tests
- New file `crates/bare/src/images_blob_url_tests.rs`, 3 tests: a blob URL revoked after `src` loads (`load`, `naturalWidth` 3, `naturalHeight` 2); a URL revoked before `src` fires `error`; a CSS `background-image: url(blob:...)` loads.
- Before the fix: the first test gave `"error,true,0,0"` and the third gave `(0, 0)` pictures. The second test passed before the fix too.
- `t`: 30 tests in 5 files pass (`images_blob_url_tests`, `browser_img_tests`, `browser_referer_tests`, `browser_web_tests`, `browser_any_stats_tests`).

## Cost rows
- No row of `tools/js/cost_rows.txt` covers the picture loader or the blob store. No row was run. The queue entry grows by one `Option<Arc<[u8]>>` (16 bytes) for each queued picture.

## Caps in this path (not changed)
- `images::MAX_PICTURES` 512: an `<img>` scan or a script batch after 512 URLs drops more pictures. The CSS set has its own 512.
- `images::BUDGET` 64 MiB of decoded pixels for each page: a chat with many pictures fails the later ones.
- `browser_web::MAX_TOTAL` 256 MiB of Blob bytes and `MAX_OBJECTS` 65536 objects (Blob, FormData, iterators): `new Blob` throws after that.
- `browser_img::MAX_REQUESTS` 4096 script requests that wait for the page.

## Not tested
- WhatsApp Web itself (no logged-in session was opened).
- A parser `<img src="blob:...">` in the markup (only script-set `src`).
- Windows.

## Notes
- The CSSOM setter `el.style.backgroundImage = 'url(...)'` did not reach the styles of the frame in the test page; `setAttribute('style', ...)` did. The test uses `setAttribute`. A `data:` URL failed the same way, and `Page::touched` did not help. Hypothesis: the CSSOM setter for `background-image` does not reach the cascade. To verify.

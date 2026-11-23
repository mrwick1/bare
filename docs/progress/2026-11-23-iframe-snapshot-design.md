# 2026-11-23: iframe content in snapshot (design, issue #963)

Design only. No code changed. Document: `docs/iframe-snapshot.md`.

## Result

- No public journey and no local fixture needs iframe content. Method: `curl` of 38 distinct public URLs of `tools/drive/public/*.steps`, count of `<iframe` tags; `grep` of the local fixtures (0 iframes).
- Iframes found: the Rust book (`noscript`, same-origin, not used), dev.to (4 YouTube embeds, not used), craigslist (hidden helper, not used), CNN lite (template text only).
- Probable need: the Gmail account menu (cross-origin frame). Not measured since child pages exist.
- Unknown: the live staging site, `html.duckduckgo.com` and `crates.io` (curl got HTTP 202 and 404).
- Engine: iframes with an http(s) `src` already run as child pages (since 2026-11-18). `snapshot`, `find`, `read` and `act` do not look into them: `Handle`, `Refs`, `Page::inspect` and the hit test have one document.
- The tool text in `crates/mcp/src/main.rs:1670` says "its content is not loaded". That is out of date.

## Decision proposed

Defer #963. Fix the tool text in its own PR. Build option C (`frame=` and `frames=`) only when a journey fails on an element in a frame.

## Not tested

No binary run, no live site, no measurement of cost. See the last section of the design document.

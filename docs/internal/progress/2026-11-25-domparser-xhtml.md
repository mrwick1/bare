# DOM: `DOMParser` takes `application/xhtml+xml`

## Problem

The Teams message pane showed "We're having trouble loading your messages". A new message in the open chat had a PDF attachment. Teams cleans message HTML with DOMPurify and `PARSER_MEDIA_TYPE: "application/xhtml+xml"`. bare threw `TypeError: Invalid MIME type` for that type. DOMPurify then called `document.implementation.createDocument`, which bare does not have, and the render failed.

The HTML Standard lists five types for `parseFromString`. bare refused one of them.

## Change

`parse_from_string` in `browser_implementation.rs` accepts `application/xhtml+xml`. It uses the HTML parser, as for the other XML types.

## Tests

- New: `crates/bare/src/browser_domparser_xhtml_tests.rs`. It failed before the change with the `TypeError`.
- `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run each, saved session)

| | Before (ws21) | After (ws22) |
|---|---|---|
| Message pane | error screen | messages, cards and the PDF attachment |
| `createDocument is not a function` | 1 | 0 |

A probe with `BARE_PROBE_NOREF` (ws19) showed that the `Referer` change (#1098) did not cause the error.

## Cost

No cost rows ran. The change adds one string to a list of four.

## Not done

- `DOMImplementation.createDocument` is still missing.
- No XML parser: XML types use the HTML parser.
- Also seen in the same run: `Element.getAnimations` and `Notification` are missing.

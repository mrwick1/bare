# ownerDocument of a node in a parsed document

## Problem

- Teams went blank about 23 s after its shell showed. All children of `body` (`#app`, the scripts, `#loading-screen`) moved out.
- A probe in the native `removeChild` and `move` paths printed the JS stack with `Error.stack`. The caller is CKEditor 5 `HtmlDataProcessor._toDom`:
  - `parseHtml(e)` is `DOMPurify.sanitize(e, {RETURN_DOM: true}).ownerDocument`.
  - `_toDom` then moves every child of `parsed.body` into a fragment.
- bare gave the page document as `ownerDocument` for a node in a `DOMParser` document. CKEditor thus moved the live `body`.

## Fix

- `bare_html::dom::Document` records when `create_document` makes a second `Document` node (`has_more_documents`).
- `ownerDocument` then walks to the tree root. A root that is a `Document` node other than the page is the owner.
- A page with one document pays one flag check.
- Gap: a detached node that a parsed document made (`parsed.createElement`) still gets the page.

## Check

- Test `browser_owner_document_tests.rs`: it fails before the fix and passes after.
- `t`: pass.
- Cost rows, archMachine, glibc, 3 runs each, base origin/main 64887d95: `attr_load`, `nodelist_load` and `select_load` (18 rows). The maximum is +0.073% (`attr_load churn 10`). No row is over the rule.
- Live Teams (archMachine, one run): after 60 s the page has 527 elements (before: about 64). The `shot` step takes 46 ms (before: 101 s).
- The screenshot shows the Teams loading screen (title bar and logo). The app does not render yet. That is the next blocker.

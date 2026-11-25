# DOM: `DOMImplementation.createDocument`

## Problem

DOMPurify falls back to `document.implementation.createDocument(NAMESPACE, 'template', null)` when `DOMParser` fails. Then it sets `documentElement.innerHTML`. bare had no `createDocument`, so the call threw `TypeError: createDocument is not a function`. CKEditor and XML helpers call it too.

## Change

- `create_document` in `browser_implementation.rs` makes a `Document` and appends the element of `createElementNS(namespace, qualifiedName)`. An empty `qualifiedName` gives a document with no element.
- `create_element_ns` in `browser_namespace.rs` now calls a new `element_ns`. The new function holds the validation and creation. `createDocument` reuses it, so the errors are the same (`InvalidCharacterError`, `NamespaceError`).
- The result is the same kind of node as the one of `createHTMLDocument`. bare has no `XMLDocument` class.
- Fewer than 2 arguments throw `TypeError`.

## Tests

- New: `crates/bare/src/browser_create_document_tests.rs`, 6 tests. 5 failed before the change with `createDocument is not a function`.
- Cases: HTML `template` takes `innerHTML` (the markup is in `content`, as the DOM Standard says); SVG root keeps its namespace and local name; empty name gives no root and no child; `1x` throws `InvalidCharacterError`; `a:b` with a null namespace throws `NamespaceError`; the result is not `document`, has `implementation`, and its element has this document as `ownerDocument`; one argument throws `TypeError`.

## Cost

No cost rows ran. The change adds one method to a prototype that a script reads on demand.

## Not done

- `doctype` is ignored. `createDocumentType` does not exist, so a script can only pass `document.doctype` or null.
- `contentType` is a constant "text/html" for every document. The XHTML, SVG and XML values are not set.
- A null namespace with a valid name still gives `Unsupported`, as `createElementNS` does.
- No XML parser and no `XMLDocument`.

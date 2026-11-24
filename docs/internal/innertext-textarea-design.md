# Rendered text around textareas

Date: 2026-11-13. Status: approved; implementation under test. Issue: #482.

## Verified failure

The candidate browser throws Unsupported when document.body.innerText walks a textarea. The existing unit test deliberately expects this gate. A minimal local page reproduces the error without Deck or a VM.

Timers and SHA-256 promises resolve on a fresh page. A synchronous unsupported getter does not prevent a later timer. The same getter inside an async continuation leaves its promise pending and also stalls a later timer. The candidate includes the earlier promise-await fix. Issue #481 records this narrower reproduction. The engine-level cause is not yet verified.

## Current design

browser_element.rs builds the current style and box trees, then collects rendered text with an explicit stack. It handles text, block boundaries, whitespace and hidden content. It stops when it encounters a visible select or textarea. The traversal is linear in the visited document nodes, plus the existing style and box work.

The HTML rendered-text algorithm excludes CSS-unrendered descendants of replaced controls. A textarea's editable value must not be copied into an ancestor's rendered text. A direct getter on a non-rendered element still uses descendant DOM text. Source: [HTML innerText algorithm](https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute).

## Options

1. Remove the gate and traverse all textarea DOM children. This can include default text that has no rendered CSS text box.
2. Append textarea.value. This changes innerText into a form-value collector and can expose text that does not belong in the result.
3. Treat rendered textarea contents as replaced control contents. Keep the element's existing boundary handling, but exclude its DOM children from rendered-text collection. Select this option.

## Proposed implementation

Change only the textarea handling in browser_element.rs. Reuse the existing traversal and style data. Add no dependency, cache, background work or extra tree traversal. Keep the select gate for its separate option and optgroup rules. Do not change form values, painting, input, or password behavior.

Preserve the fallback for detached or non-rendered elements. It reads DOM descendant text, not a live form value. Respect hidden ancestors and the current block-boundary rules.

## Checks before and after

- Replace the textarea gate expectation with a failing rendered-text regression. Keep the select gate assertion unchanged.
- Check a visible textarea between ordinary text nodes. Neither default contents nor edited value may enter the ancestor result.
- Check the direct visible getter, a hidden or detached textarea, a hidden ancestor, a block-displayed textarea, and multiple controls.
- Run the existing element text cases and bare library tests. Keep unrelated timer and formatting failures visible.
- Build an isolated candidate MCP binary. On the minimal page, run SHA-256 followed by body.innerText inside an async function, then a timer. Both must settle. Keep unsupported operations elsewhere in the engine outside this change.
- Rerun the Deck protected-input helper with the metadata fix included in an isolated test candidate. Do not weaken Deck or replace a shared browser installation.

## Limits

This removes the textarea trigger. It does not establish correct recovery from every Unsupported host operation in an async function. Keep #481 open until that broader failure is tested separately. No resource benchmark has run, and no performance claim is made. The selected approach removes a rejected operation without adding a new traversal. The implementation skips rendered textarea children and preserves the existing boundary and non-rendered fallback paths. The pre-fix rendered-text test failed with Unsupported. All nine element tests now pass. The full library run passes 1416 tests with six ignored. The isolated combined candidate passes the async textarea getter, later timer and metadata-gated HTTP check. Strict formatting and clippy retain baseline failures, recorded in progress.md.

## Current-main recheck: 2026-11-14

The branch is stacked on the rebased Fetch Metadata fix. The combined serial release library run passes 1,464 tests with six ignored. The rebuilt MCP executable passes the same-origin header-gated request. Its async SHA-256 then body text read returns the visible text, and a later timer also resolves. No test page value enters the ancestor text. The Deck UI and private-viewer run remains pending. Resource benchmarks were not repeated. No merge is included.

## Independent pre-merge check: 2026-11-14

The branch was rebased directly onto main at 5bc18d14. Fetch Metadata is not part of this branch. Its separate PR remains blocked by its request cost. The standalone textarea release library run passes 1,459 tests with six ignored. The earlier combined MCP and accepted Deck journey remain historical combined-candidate evidence; they are not standalone metadata evidence.

The same five workloads were measured on frozen main and this candidate. Each instruction and RSS interval has three fresh process samples. Machine: Linux x86-64, Ryzen 9 5900HX. Two build jobs. Shared host; no wall-time claim. Instructions use perf instructions:u and the existing tcache setting. Peak RSS uses a C fork/exec/wait4 wrapper without that setting and is in KiB. Frozen main executable hashes were checked before retention.

| Case | Main instructions | Candidate instructions | Main RSS KiB | Candidate RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| style_setget | 56,310,566–56,326,469 | 56,305,604–56,322,886 | 12,364–12,428 | 12,096–12,532 |
| sheets_linked | 8,622,008–8,623,555 | 8,621,857–8,624,827 | 12,676–12,856 | 12,648–12,808 |
| browser_mutation | 70,097,876–70,098,479 | 70,098,560–70,099,026 | 11,472–12,036 | 11,560–11,704 |
| fetch_post | 21,591,413–21,592,372 | 21,592,361–21,593,265 | 13,004–13,280 | 12,880–13,156 |
| xhr | 10,691,794–10,694,630 | 10,694,412–10,695,832 | 11,608–11,872 | 11,448–11,728 |

All five instruction rows remain below the 0.5 percent gate on glibc. No row therefore exceeds the limit under both allocators. A fresh jemalloc run and the full cost matrix were not run for this independent branch. The maximum observed increase is 0.024 percent. Fresh UI captures were not repeated for this rebase.

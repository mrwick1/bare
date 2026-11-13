# Fetch metadata for page requests

Date: 2026-11-13. Status: proposed for owner review. Issue: #475.

## Verified failure

A local header-echo service received a same-origin page `fetch()` from the installed bare MCP browser. `Sec-Fetch-Site`, `Sec-Fetch-Mode` and `Sec-Fetch-Dest` were all absent. The service returned only those three header values. It used no credentials. The browser session and the service stopped after the check.

The Deck test controller requires `Sec-Fetch-Site: same-origin` before its one-use synthetic value response. Its 403 therefore has a confirmed missing-header cause. Deck must keep that check.

## Existing design

`browser_fetch.rs` validates script headers and queues page requests. Its shared `run_hops` path builds wire headers for fetch and XHR. It already tracks the initiating document, request mode and redirects. Scripts cannot set `Sec-` headers. `bare_net::cookie::same_site` already compares schemeful sites.

The wire path allocates a vector per hop. This change adds three static header pairs and one small site classification. It adds no request, timer, queue, dependency or background work. It makes no performance claim.

## Options

1. Set headers when a request enters the queue. This is small, but a redirect can make the recorded site relation incorrect.
2. Set them in the generic HTTP client. That client serves non-browser callers and does not know the document, destination or request mode.
3. Set them in the page request hop path. It has the required context and can preserve redirect history. Select this option.

Do not hardcode `same-origin` for every request. A cross-site request must never acquire that value.

## Proposed behavior

Follow the [Fetch Metadata draft](https://w3c.github.io/webappsec-fetch-metadata/).

- Add the headers on requests to potentially trustworthy targets. Reuse the existing trustworthy-URL predicate.
- Use the actual request mode for `Sec-Fetch-Mode`.
- Use `empty` for the fetch and XHR destination. Do not add `Sec-Fetch-User` for these requests.
- Start the site relation from the initiating document. Use `same-origin`, `same-site` or `cross-site` as applicable. Reuse the existing schemeful site helper. Preserve the least-trusted relation across every redirect; a return to the original origin cannot erase a cross-site hop.
- Add browser-generated metadata after author headers and after the CORS preflight decision. Author code still cannot set it. Metadata must not create an author-header preflight.
- Keep generated metadata out of the script-visible Request headers.

This proposal covers the shared page fetch/XHR path. It does not claim metadata support for every navigation, image, stylesheet, worker or preflight request. Identify any required widening before implementation.

## Validation before implementation

Add a failing local server regression for a same-origin fetch. Assert exact wire values `same-origin`, `cors` and `empty`. Attempt an author-supplied `Sec-Fetch-Site`; the browser value must win and the author value must stay absent from Request headers.

Add classification and redirect tests. Cover a different port on the same schemeful site, a cross-site target, a downgrade, and a cross-site redirect that returns to the first origin. Cover same-origin and no-cors request modes. Confirm an untrustworthy target receives no metadata.

Update the old forbidden-header test. It currently expects `Sec-Fetch-Mode` absent on the wire; it must instead distinguish browser-generated metadata from the refused author value. Do not weaken the other forbidden-header assertions.

Run the focused fetch tests, the bare crate tests, formatting and clippy. Use two build jobs and an isolated target directory. Then rerun the header-echo check through the candidate bare MCP binary and the Deck protected-input journey. Do not replace a running shared browser or guest image without a separate installation decision.

## Limits

No code is implemented by this design. No functional regression or candidate-binary test has run. No resource benchmark has run. The existing cookie site helper must be checked for the required public-suffix behavior before reuse. If it is insufficient, stop and revise this design. No real account, credential or external login was used.

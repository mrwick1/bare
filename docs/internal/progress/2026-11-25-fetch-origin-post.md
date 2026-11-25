# Origin header on same-origin POST (2026-11-25)

## Problem

`fetch` and `XMLHttpRequest` sent no `Origin` header on a same-origin request with a method other than GET or HEAD. The Fetch Standard ("append a request `Origin` header") sends it on every such request. Chrome does the same.

Teams sets its image cookie with a same-origin `POST /api/mt/<region>/beta/users/<id>/cookiev2`. The server answered 401 to each of these calls from bare (9 calls in one live run). Chrome got 200.

## How it was found

- A temporary probe in `crates/net/src/h2.rs` printed the header names of the `cookiev2` request on the wire. Bare sent no `origin` and no `referer`. Chrome sent `origin: https://teams.cloud.microsoft` (HAR of a Chrome run).
- A first probe looked at the bearer token. In the offline replay, three calls carried a Graph token. That is an artifact of the replay: the token endpoint replies are loose matches (the request ids differ from the recording), so the replay gives the recorded token replies in order, whatever the scope. Live, every middle-tier call carried a token for `https://api.spaces.skype.com`.

## Change

`crates/bare/src/browser_fetch.rs`: the `Origin` header is added for a CORS request and for every request that is not GET or HEAD. Before, it was added for a CORS request and an opaque (no-cors, cross-origin) unsafe request only.

## Tests

- New file `crates/bare/src/browser_fetch_origin_tests.rs`: a same-origin `POST` from `fetch` and from `XMLHttpRequest` sends the page origin; a same-origin `GET` sends none. The POST test failed before the change (`left: []`) and passes after it.
- `browser_cors_tests.rs`: the old test asserted no `Origin` on a same-origin `PUT`. That was wrong against the standard. It now asserts the page origin.
- `t` on the changed files: 7 passed. `browser_cors_tests.rs`: 17 passed. The new file: 2 passed.

## Live result (archMachine, one run each, saved session, no login)

| Run | `cookiev2` replies | 401 replies in total |
| --- | --- | --- |
| Before (main 488796ea + probes) | 9 × 401 | 10 |
| After (same + this change) | 2 × 200 | 1 (`profilepicturev2`, no auth header) |

The Teams panes are still empty after the change. This 401 was not the cause of the empty chat list.

## Not tested

- The `null` origin of the referrer policy for an HTTPS page that posts to an HTTP URL. Bare sends the page origin there.
- `Referer`: bare sends none on `fetch` and XHR. Chrome sends one. Not changed here.
- The remaining `profilepicturev2` 401.
- No cost rows: the change is one condition per request in the fetch path, outside the rows of `cost_rows.txt`.

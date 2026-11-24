# BroadcastChannel across pages

Date: 2026-11-24.

## Problem

Microsoft Teams signs in with MSAL `ssoSilent`. A hidden frame loads `login.microsoftonline.com`, which redirects to `teams.microsoft.com/v2/authv2`. That page sends the reply with `new BroadcastChannel(id).postMessage({v: 1, payload})`. bare delivered a channel message inside one page only. The frame is its own page, so the top page never got the reply, and Teams stayed on the loading screen.

## Change

- `browser_port_hub.rs`: the hub lists the end of each `BroadcastChannel` under its origin and name (`join`). `peers` gives the live ends of other pages and drops ends that are gone. A `Wire` carries the origin of a broadcast sender.
- `browser_scheduler.rs`: `add_broadcast_port` makes a hub end for the channel when the page has a hub. `take_end` gives a broadcast wire its sender origin as `MessageEvent.origin`.
- `browser_broadcast.rs`: `postMessage` also sends one wire to each end of another page, with the same bounds as a port message across pages.

The frames and workers of one top page share the hub, so workers get the messages too. Close and GC already remove a port end, so they remove the channel end too.

## Cost gate

`cost_matrix.sh -a -g 'scheduler_load|navigation_load|web_apis_load|browser_load'`: 39 rows, glibc mean −0.001%, 0 over the rule. `web_apis_load segmenter` fails to run on main too (known).

## Tests

- New: `browser_broadcast_pages_tests.rs` (4 tests): a same-origin frame posts and closes, and the parent gets the message with its origin; parent and frame talk both ways in order; a frame of another origin, or a channel of another name, gets nothing; a worker sends to the page.
- `t`: all pass.

## Not tested

- The Teams sign-in end to end (next step, with the Teams tape).
- Pages of two top pages (two tabs). They do not share a hub, so they do not get each other's messages.
- Windows.

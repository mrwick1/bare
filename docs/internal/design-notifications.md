# Design: web notifications that an agent can read

Status: draft for owner review (2026-11-26). No code is built.

## 1. Problem

The owner wants WhatsApp Web in bare to send and receive messages, send media and listen to notifications. Messages already
arrive in an open chat without a reload (live check, 2026-11-26). Two parts are missing:

1. The page cannot show a notification. Bare says the permission is "denied". WhatsApp shows the banner "Message and call
   notifications are off" and never calls `new Notification`.
2. An agent (bare-mcp, the offstage `browser_*` tools, bare-drive) has no way to see a notification, to wait for one, or to
   click one.

## 2. Current state

Facts from the code at `dc129c14`:

- `crates/bare/src/browser_notification.rs`: `Notification` is a lazy accessor of the global, as `Worker` is.
  `Notification.permission` is the constant "denied". `maxActions` is 0. `requestPermission([cb])` calls a function callback at
  once and returns a promise resolved with "denied".
- `new Notification(title, options)` reads `title`, `options.body` and `options.tag` only. It makes an `EventTarget` and queues
  `Task::Notification`. That task fires a trusted `error` event when a listener for `error` exists. It roots the object until it
  runs. After that the object is not a root, so a later `click` could not reach a collected object.
- `close()` does nothing. There is no `onshow`, `onclick` or `onclose`, no `icon`, `data`, `silent`, `renotify` or
  `requireInteraction`.
- `navigator.permissions` does not exist. The `Navigator` getters in `browser_web.rs` (line ~450) are `userAgent`,
  `userAgentData`, `language`, `languages`, `platform`, `onLine`, `cookieEnabled`, `hardwareConcurrency` and
  `serviceWorker`. So `navigator.permissions.query({name: 'notifications'})` throws a `TypeError`.
- `navigator.serviceWorker` is a container with no registration (`browser_web.rs`, struct `Sw`, as in Ladybird). `register`
  rejects and `ready` never settles. So `ServiceWorkerRegistration.showNotification` is never reachable. WhatsApp logs "Failed
  to register a ServiceWorker".
- `document.visibilityState` is always "visible" and `document.hasFocus()` is always `true` (`browser_gaps.rs`).
- Profiles (`crates/bare/src/profile.rs`, `design-persistent-profile.md`): a directory `<BARE_DATA_DIR>/profiles/NAME` with
  `cookies.bin`, `local.bin` and `idb/*.log`. Each file is magic, payload and CRC-32, written with a rename. A `Saver` per page
  writes a file when its revision counter moved, at most once per second. There is no per-origin permission state.
- bare-mcp (`crates/mcp/src/main.rs`): each live session has a worker thread. The thread runs the calls of its queue and the page
  between calls (`tick_live`). A WebSocket message or a reply wakes the worker (`wake_on_done`). Then the page polls each
  second, not each 4 ms. The `console` and `network` tools read a log with a cursor (`since_last`).

## 3. What a page needs

From the Notifications API standard and Chrome as the reference:

- `Notification.permission`: "default", "granted" or "denied", per origin. Only a secure context (https, localhost) can get
  "granted". Chrome keeps the decision in the profile.
- `Notification.requestPermission()`: with no decision, Chrome shows a prompt. A dismissed prompt resolves "default".
  Firefox needs user activation for the prompt; Chrome does not (to verify for the current Chrome).
- `navigator.permissions.query({name: 'notifications'})`: a `PermissionStatus` whose `state` is "prompt", "granted" or
  "denied" ("default" maps to "prompt"). It fires `change` when the decision changes.
- `new Notification(title, {body, icon, tag, data, silent, renotify, requireInteraction, dir, lang, badge})`:
  - With "granted", a task fires `show`. Else a task fires `error`.
  - `renotify: true` with an empty `tag` throws a `TypeError`. `silent: true` with `vibrate` throws a `TypeError`.
  - `data` is a structured clone. Bare has a clone (`WebClone`), so this reuses it.
  - A new notification with the same `tag` and origin replaces the old one in the list. The old one gets no `close` event. With
    `renotify` false the replacement is silent.
- `close()`: removes the notification and fires `close` in a task. A second `close()` does nothing.
- A user click fires a trusted, cancelable `click` event. The standard says: if not cancelled, focus the page. Chrome does not
  focus by itself; pages call `window.focus()` in `onclick` (to verify). Chrome does not close a clicked page notification.
- `onshow`, `onclick`, `onclose` and `onerror` are event handler attributes.

What WhatsApp needs (hypothesis, from the public API only; no bundle was run): `'Notification' in window`,
`Notification.permission === 'granted'`, perhaps `navigator.permissions.query`, then `new Notification(...)` when the page is
not in view. Step 2 of the plan checks this live: the banner must go away after a grant.

## 4. How the agent sees a notification

### 4.1 The record

The engine keeps one record for each shown notification, in the `Browser` (not in JS):

```text
Note { id: u64, origin, title, body, icon_url, tag, silent, at_ms, state: Shown | Replaced | Closed | Clicked }
```

`id` is a counter of the page. The JS object stays a GC root while the state is `Shown`, so a later click reaches it. The record
holds no JS value. `data` stays in the JS object only.

### 4.2 Design A: a notification log in the session, read through a bare-mcp tool

- `Browser` holds `notes: Vec<Note>` (live ones) and a log of events (`shown`, `replaced`, `closed`, `clicked`,
  `permission-requested`). Each event has a sequence number.
- bare-drive `Session` gets `notifications()` and `click_notification(id)`. bare-mcp gets one tool, `notifications`:
  - `op=list` (default): events after the cursor of the agent (as `console since_last`), then the live notifications.
  - `op=wait timeout_ms=N`: return at the first new event. The worker ticks the page while it waits, as a step `expect` does.
    No poll loop: the wait wakes on the page event.
  - `op=click id=N`: fire `click` with transient user activation, as a click on a Chrome notification gives (to verify).
  - `op=close id=N`: the user closes it: remove it and fire `close`.
  - `op=grant | deny | reset origin=URL`: set the permission (section 5). Default origin: the origin of the page.
- An `act` receipt adds one line per new notification, as it does for dialogs and tabs: `notification: #3 "Alice" "hi"`.
- bare-drive gets a step `expect notification title~"x"`, so a journey file can test the path without bare-mcp.

Cost: no work on a page that does not touch `Notification`. The global stays lazy. One record is about 200 bytes plus its text.
A busy chat with 1,000 notifications a day is about 200 KiB per day if nobody reads. The log drops the events that the agent read,
and the live list drops closed and replaced records. CPU per notification: one struct and one task, under 10 µs (hypothesis,
to measure).

### 4.3 Design B: a line in the console log

`new Notification` writes `notification: "title" "body"` into the existing console log. The agent reads it with `console`.

- Cost: lowest. No new tool, no new state.
- It cannot click or close, and it cannot wait. It mixes notifications with page errors, and the console keeps only the last
  500 entries, so a busy page loses them.

### 4.4 Design C: a desktop notification through the OS

Behind a flag (`--os-notifications`, off by default). Linux: one D-Bus call to `org.freedesktop.Notifications.Notify`; Windows: a
toast. The offstage `notify_list` and `notify_wait` tools can then read them.

- Cost: one D-Bus connection per process (about 1 MiB, hypothesis) and one call per notification. Spawning `notify-send`
  costs a process per notification: about 2 ms and a few MiB for a short time.
- A headless session on a server has no notification daemon. The offstage rules forbid touching the human's desktop.
- Good for a person who uses the `bare` window. Not a path for an agent.

### 4.5 Choice

Design A for agents, with C as a later flag for the `bare` window. A is the only design with wait and click. Its cost is
zero on pages that do not use notifications, and in proportion to the notifications on pages that do. B gives no click and
loses entries. C needs a desktop.

Retention: the brief asks for a log with no cap that the agent drains. CLAUDE.md says a cap that guards a slow algorithm is a
design smell; here the cap guards against an agent that never reads. Proposal: no cap on unread events (200 bytes each).
Open question 2 asks the owner.

## 5. Who grants the permission

Options:

1. A profile setting: `permissions.bin` in the profile, written by the `Saver` like `cookies.bin`. The grant survives a
   restart, as in Chrome.
2. A bare-mcp argument: `open grant_notifications=true`. Simple, but the agent must give it on each open.
3. Auto-grant for a list of origins in an environment variable. No tool call, but a hidden setting.

Proposal: option 1, set with `notifications op=grant`. A session with no profile keeps the grant in memory. The file is a list of
`(origin, permission name, state)` in the same frame (magic, payload, CRC-32), so other permissions (`clipboard-read`,
`geolocation`) fit later. `requestPermission()` with no decision resolves "default", as a dismissed prompt, and logs a
`permission-requested` event, so the agent sees that the page asked. The agent then grants and the page asks again
(WhatsApp has a "Turn on" link in its banner).

Default with no decision: "default" (Chrome), not "denied" as today. Teams reads the permission (see the module comment); it must
still work with "default" (to verify in the Teams journey).

## 6. A page in the background

Notifications matter most when the chat is not in view.

- Timers: bare does not throttle timers. A timer runs at its due time on a hidden page too. Only the nesting clamp of the
  standard applies (`browser_scheduler.rs`). Chrome throttles hidden pages to one wake per second, and to one per minute after
  5 minutes for chained timers. WebSocket messages are not throttled in Chrome.
- WebSocket: a message wakes the bare-mcp worker (`wake_on_done`), so it is not delayed by the 1 s poll.
- Between calls: the bare-mcp worker runs the page between tool calls (#471). So a page with no agent call still gets its
  messages and can make a notification. bare-drive runs only while its steps run; a long watch needs bare-mcp.
- Visibility: the page is always "visible" and focused. WhatsApp shows no notification for a message in the open chat when the
  page has focus (hypothesis, as Chrome users see). So an agent needs a way to put the page in the background:
  `notifications op=background on|off`. It sets `visibilityState` "hidden", `hasFocus()` false, and fires `visibilitychange`
  and `blur`. A click on a notification sets it back to "visible", as Chrome does when it focuses the tab.
- Throttling as a cost saving: a hidden page could align its timers to 1 s, as Chrome does. That saves CPU on an idle
  WhatsApp page. It is not part of this design. Open question 4.

## 7. Plan (one PR each)

1. **Permission state.** `Notification.permission` and `requestPermission` read a per-origin decision (default "default").
   `navigator.permissions.query` with `PermissionStatus` (`notifications` only; other names "prompt"). Insecure context:
   "denied". Tests, Chrome as the reference: the three states, the callback form, `query` state mapping, `change` event.
2. **Profile and grant.** `permissions.bin` in the profile and in `StorageState` (so `save_state` keeps it). bare-mcp tool
   `notifications` with `grant`, `deny` and `reset`. Test: grant, close, open the profile, the permission is "granted".
   Live check: the WhatsApp banner goes away (no logged-in profile; the QR page only, or ask the owner to run it).
3. **Show, close, tag.** Full `NotificationOptions` parsing with the two `TypeError` cases. `show` task with the grant,
   `error` without. `close()` and the `close` event. Tag replacement. The object stays a root while shown. The record list and
   event log in `Browser`. Tests: event order of `show`, `close`; tag replacement fires no `close`; GC of a closed one.
4. **Agent tool.** `notifications op=list|wait|click|close`, the receipt line in `act`, and the bare-drive step
   `expect notification`. `click` fires a trusted `click` with user activation. Tests: a test page that makes a notification
   from a timer; `wait` returns it; `click` runs `onclick`; `window.focus()` in it works.
5. **Background.** `op=background on|off`: `visibilityState`, `hasFocus`, `visibilitychange`, `blur`, `focus`. Tests:
   events and values in Chrome order.
6. Later, if the owner wants it: the OS path behind a flag (Design C).

Cost rows: steps 1, 3 and 5 touch the host. Run the rows of `examples/platform_load` and `examples/handler_load`
(`tools/js/cost_rows.txt`), which cover `navigator` and event handlers. The `Notification` global stays lazy, so other rows
should not move. Steps 2 and 4 touch only bare-mcp, bare-drive and the profile: no cost row covers them. A Vex change is not
expected.

## 8. Open questions for the owner

1. Default permission with no decision: "default" as Chrome (proposed), or keep "denied" until the agent grants?
2. Retention of unread notification events: no cap (proposed, about 200 bytes each), or a cap like the console (500)?
3. Grant path: a profile setting set by `notifications op=grant` (proposed), or also an `open` argument?
4. Should a hidden page align its timers to 1 s, as Chrome does, to save CPU? It would delay a timer-driven notification
   by up to 1 s, not a WebSocket-driven one.
5. Should the OS notification path (Design C) be built for the `bare` window, and on which platforms?
6. Service workers: WhatsApp may prefer `ServiceWorkerRegistration.showNotification`. This design covers only page
   notifications. If WhatsApp needs a service worker for them, that is a separate design (`design-workers.md` covers
   dedicated workers only).

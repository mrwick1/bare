# Design: iframe content in `snapshot` (issue #963)

Status: design only. No code is built. The owner reviews this document before any code.

Base: main `f29a583d`. Date: 2026-11-23.

## 1. Summary

Question: do the bare journeys need the content of an iframe in `snapshot`?

Answer: no journey that we can check needs it. The one known case is the Gmail account menu. Its current state is unknown.

Two facts change the issue text:

1. The issue says that iframe content is not loaded. That is no longer true. Since 2026-11-18 an iframe with an http(s) `src` loads and runs as its own child page (`docs/decisions.md`, "A child page for each iframe with an http(s) src"). Same-origin and cross-origin frames are both child pages. What is missing is only the read and act path: `snapshot`, `find`, `read` and `act` see one `Browser`, and they never look into a child.
2. The tool text in `crates/mcp/src/main.rs:1670` ("its content is not loaded") and `docs/decisions.md:1253` are out of date. A fix of that text is a docs change that needs no design.

Recommendation: **defer #963.** Do not build iframe content in the outline until a journey fails on an element in a frame. Keep the design below ready (option C). Section 7 lists the trigger and the tests.

## 2. Evidence

### 2.1 Method

- Public journeys: the 48 files in `tools/drive/public/*.steps` use 38 distinct URLs. For each URL, `curl -sL -A Mozilla/5.0` fetched the HTML, and `grep -o -i '<iframe[^>]*>'` counted the tags. Run on archMachine, 2026-11-23.
- Limit: this reads the HTML that the server sends. An iframe that a script adds later is not counted. A page that needs JS to build the iframe (a login widget) would not show here. For each page that has an iframe tag, the steps file and the page were read to see if the journey needs the content.
- Local fixtures: `grep -c -i iframe` on `tools/fixtures/drive/*.html`, `tools/fixtures/staging/login.html` and `tools/fixtures/agent-eval/*.html`: 0 in every file.
- Login and gate journeys: `docs/progress.md`, `docs/progress/` and `docs/design-gmail-frames.md`.

### 2.2 Table: journey, iframe, origin, needed element

| Journey | Iframe in the page? | Origin | Needed element in the iframe? |
| --- | --- | --- | --- |
| 31 of the 38 public URLs (archlinux, blog.rust-lang, curl, MDN, docs.github, docs.python (2 URLs), docs.rs, wikipedia (3 URLs), example.com, go.dev, httpbin (2 URLs), HN, lobste.rs, next.js, nginx, node.js, NPR, react.dev, python.org, sqlite, substack, kernel.org, debian, paulgraham, w3.org, wiktionary, rust-lang.org) | No `<iframe>` in the served HTML | n/a | n/a |
| duckduckgo-html-search | No (HTTP 202 to curl: the body was not the result page; to verify with a real run) | unknown | unknown |
| crates-io-home | No (HTTP 404 to curl: the SPA route needs JS; to verify with a real run) | unknown | unknown |
| rust-book-sidebar, rust-book-chapters, rust-book-search | Yes: `<iframe class="sidebar-iframe-outer" src="toc.html">` | Same origin | No. The iframe is inside `<noscript>`. With JS on, `toc.js` fills `<mdbook-sidebar-scrollbox>` in the main document. The steps read `ol.chapter a` in the main document |
| devto-home | Yes: 4 YouTube embeds (`youtube.com/embed/...`) | Cross-origin | No. The steps do not touch them |
| craigslist-home | Yes: `#cl-local-storage`, `display:none`, a local-storage helper page | Same site | No |
| cnn-lite-front | Two `<iframe src="//fave.api.cnn.io/...{video}...">` strings inside a JS template in the page (not iframes in the served DOM) | Cross-origin | No. The steps do not touch them |
| Local fixtures: `tools/fixtures/drive` (login, agents, overview), `tools/fixtures/staging/login.html`, `tools/fixtures/agent-eval` (login, account, board, upload, ...) | No | n/a | n/a |
| Staging login journey (`tools/drive/staging-journey.steps`: login form, Agents link, search, reload) | Not checked: the real site is not public. The saved login page `tools/fixtures/staging/login.html` has no iframe, and the journey passed on staging on 2026-11-07 using only main-document selectors (`input[name=email]`, `form button[type=submit]`) | unknown | The steps use none |
| Gmail journey (`docs/design-gmail-frames.md`, 6 steps) | Yes: Chrome shows 9 to 10 frames. Step 5, the account menu, lives in `ogs.google.com/u/0/widget/account` | Cross-origin | **Yes**, the account menu. Before child pages it failed 0/3 for a different reason (a port in the transfer list). Since steps 3a to 3c it is not measured again. To verify: run step 5 on a build with child pages, and read the frame with a script |
| Other login pages with a captcha or a payment frame (the case in the issue text) | No journey and no recorded login gate holds one. NYTimes (`docs/progress.md:5118`) stops at a captcha-delivery.com challenge with HTTP 403, before any page | n/a | Not applicable: the gate is the 403 |

Result: of the 36 public URLs that curl read, 0 need iframe content (5 have an iframe tag; none is needed). The local fixtures have none. 1 journey (Gmail step 5) probably needs one. 3 rows are unknown: two public URLs that curl did not read correctly, and the live staging site.

Note on the sidebar of the Rust book: it is the only same-origin iframe with real content in the public set. A `noscript` iframe is never made when scripts run, so it does not count.

## 3. The current design

### 3.1 What exists

| Part | Today | Where |
| --- | --- | --- |
| Child page | An iframe that is connected and has an http(s) `src` (and no `srcdoc`) gets its own `Browser`: own `Interp`, heap, `Dom`, timers. It runs on the thread of the parent. Both origins. Eager load, after parent work | `crates/bare/src/browser_child.rs:1-60` (the module doc), `:496-531` |
| Where it is kept | `Kids::slots` in the parent `Dom` (iframe node, URL, `RemoteWindow`). `Kids::kids`: `Vec<Option<Child>>`, `State::{Loading, Run(Box<Browser>), Empty}`. The `FrameId` is the slot index | `browser_child.rs:134-196` |
| Viewport of the child | The border box of the iframe in the last layout of the parent, else 300 by 150 | `browser_child.rs:512`, `:727` |
| Round loop | The parent runs first. Each child runs one turn only when the parent has no task due. The page is not settled while a child loads or has work | `browser_child.rs:532`, module doc "Run", "Settle and clock" |
| Messages and ports | `postMessage` and `MessagePort` cross pages as heap-free data | module doc "Messages", "Ports"; `browser_port_hub.rs` |
| Limits | 64 child pages in all, depth 8 | `BrowserLimits::max_child_pages`, `max_child_depth` |
| Blank, `about:blank`, `srcdoc` frames | Not child pages. They keep a blank window that shares the realm of the parent. `srcdoc` content is not loaded | `browser_frames.rs`; `docs/decisions.md` "A blank iframe window shares the parent realm" |
| `contentDocument` | `null` for a child page of any origin (a browser gives the document for same-origin) | `browser_child.rs:9` |

### 3.2 Why `snapshot` shows one line

`snap::snapshot(p: &mut Page, ...)` (`crates/drive/src/snap.rs:710`) does `p.inspect(|d, frame| walk(...))`. `Page::inspect` (`crates/bare/src/control.rs:253`) reads the one `Document` and the one `Frame` (styles and layout) of `source.browser()`. It never looks at `Browser::dom::kids`. The `iframe` role is a `LEAVES` entry (`snap.rs:159`), so the walk stops at the element. The line gets its name from `title` or `src` (`snap.rs:331`).

Four structures tie one ref to one document:

- `Handle { doc: usize, node: NodeId }` (`control.rs:49`): one generation number and one node id.
- `Refs { nodes: HashMap<u32, NodeId>, ids: HashMap<NodeId, u32>, generation, ... }` (`snap.rs:73`): one node space.
- `Page::click`, `type_text`, `actionable`, `hit`: use the one `Frame` and the one `Browser`.
- `Page.frame`: one `Frame`. A child `Browser` has the machinery to make one (`render_with` in `browser_any.rs:196`), but nothing calls it for a child. So a child has a viewport and a DOM, but no layout and no styles today (hypothesis: no layout is made because no paint or snapshot asks for it; to verify with a counter in a test).

### 3.3 Complexity today

`snapshot` is O(n) in the nodes of the one document, with the layout made once per revision. Frames cost nothing in the outline. The load cost of a child is paid already, at load time, whether or not an agent reads the child.

Hypothesis: pages such as dev.to (4 YouTube embeds) load 4 child pages today, because the rule is "eager, any origin, hidden or not". No measurement exists (`docs/progress.md:13162`: "no benchmark row has an iframe with a src"). To measure: instructions and peak RSS of dev.to with children on, against `max_child_pages = 0`.

## 4. How other tools expose frames

From public documentation and source layout. Not re-read for this document: marked "to verify".

| Tool | What it gives |
| --- | --- |
| Chrome DevTools accessibility tree | `Accessibility.getFullAXTree` takes a `frameId`. The tree of a frame in the same process hangs under the iframe node (the iframe node has the child `RootWebArea` as a child). A cross-process frame (OOPIF, site isolation) is another CDP target: the client attaches to it with its own session id and asks for its own tree (to verify) |
| Playwright | Every locator belongs to a `Frame`. `page.frames()` lists them; `frameLocator('iframe')` crosses into one. The ARIA snapshot (`locator.ariaSnapshot()`) and the agent snapshot include iframe content inline, under the `iframe` line; element refs of a child carry a frame prefix (to verify the exact format and which version added it). Playwright reaches a cross-origin frame because it drives the browser over CDP, not from the page |
| WebDriver BiDi | A browsing context tree: `browsingContext.getTree` gives `children`. Each command (`script.evaluate`, `input.performActions`, `browsingContext.locateNodes`) names a context id. A frame is addressed by id, not inlined |
| Selenium and WebDriver classic | `switchTo().frame(...)`: the client enters a frame, and later commands act in it. Nothing shows the frame content until the switch |
| flashbrowse | Its frames test checks only the parent text. No model (issue #963) |

Common points:

1. Every tool that can act in a frame keeps a frame id with the element id.
2. Two styles exist: inline in one tree (Chrome AX, Playwright), or addressed by a frame id (BiDi, WebDriver).
3. A cross-origin frame is no problem for a tool that sits outside the page. In bare, the engine is the page, so the same holds: the child is a `Browser` that bare owns.

## 5. Options

All options need the same engine part first: a frame-aware `Handle` and `Refs`, a layout for a child, and `act` routed to a child. They differ in what the outline shows.

### 5.0 Shared engine part (E)

- `Handle` gets `frame: FrameId` (0 is the top page) next to `doc` and `node`. A nested child is a path, `Vec<FrameId>` (depth is at most 8), or one interned id per slot chain. Hypothesis: a path of up to 8 `u32` is cheaper to build than to intern, and handles are few.
- `Refs` keeps one node map for each frame: `HashMap<(FrameKey, NodeId), u32>` (or a key per frame). A ref stays `eN`, and all refs share one counter, so no two frames get the same number. The stale rule needs a check per frame: a child that navigates changes its `doc` number. The ref of an old document fails as "from an older page", as for the top page.
- `Page` gets `child_inspect(path, f)` and a child `Frame` cache: made on request with `render_with`, dropped when the child page runs a task or its revision changes. Hypothesis: one `Frame` per child that was asked for, not one for every child.
- Act path: `click`, `type_text`, `hit`, `scroll` and focus take a `Handle` with a path. The point of a click in the child is in the child viewport. The child viewport is at an offset in the parent. The pointer event must go to the child `Browser` (`b.handle(Event::PointerMove...)` on the child, as `Page::click` does on the top). The parent `hit` of a point in an iframe box returns the iframe element. So this needs a "hit test goes into the child" rule. It is the part of the design that no current code has (design-gmail-frames.md step 4 lists it as "to build after the message part").
- `Browser` access: the `Child` lives in `Kids::kids` behind a `RefCell`, private to `browser_child.rs`. It needs a small public method, `Browser::with_child(path, f)`, that takes the child out of the `Vec` while `f` runs (the same trick as the round loop).

### Option A: inline in the outline, both origins

Under each `iframe` line, `snapshot` walks the child `Document` with the child `Frame` and adds the lines, indented one level deeper. Refs work with `act`.

```
[e12] iframe "Payment" (frame f1, cross-origin)
  [e13] textbox "Card number"
  [e14] button "Pay"
```

- Cost in CPU: one style and layout pass for every iframe child on every full snapshot, including YouTube embeds and hidden helper frames. An agent that never wants frames pays for them. Hypothesis: a layout of a child is of the order of the layout of a page of its size; an embed page is cheap, a Gmail frame is not. Not measured.
- Memory: one `Frame` per child while the outline lives. A child `Frame` also feeds the diff (`Seen`) for `*` marks.
- Time to ready: no new load. `settle` already waits for children.
- Risk: long outlines. The outline limit (`max`) and the `offset` paging would count the child lines. A page with 20 ad frames adds noise. A rule is needed to skip hidden (`display:none`, 0x0) frames.
- Gain: one call shows everything. An agent needs no new vocabulary.

### Option B: a separate frame session, `frame=f1`

`snapshot` keeps the one line, now with a frame name and a state: `iframe "Payment" (frame f1, cross-origin, loaded)`. The agent calls `snapshot frame=f1` (or `frame=main>f1` for a nested one) to read it, and `act ref=e13` works as usual because refs are global. `find`, `read` and `links` take `frame=` too.

- Cost in CPU: zero for a page whose frames nobody reads. Layout of one child only when asked.
- Memory: one child `Frame` at a time.
- Time to ready: unchanged.
- Risk: the agent must know that it has to ask. The one-line hint "(frame f1)" shows it. A login journey must add a step. Two calls instead of one.
- The `diff` base (`Seen`) is per frame, so `act` with a diff needs a rule: the diff of the top, and a one-line note "frame f1 changed" when a child revision moved (hypothesis: one integer per child, `Browser::doc_revision`).

### Option C: on demand, inline for the frames that the agent names (recommended if built)

B for the default, plus a parameter that does A for chosen frames: `snapshot frames=f1,f2` or `frames=all`. `frames=all` does A. The default outline stays as small as today and gets the frame name and state on the iframe line.

- Cost: pay-as-asked. Default cost is a string format on each iframe line.
- Memory and time to ready: as B.
- Adds one parameter beyond B. It lets an agent read a login page with its captcha or payment frame in one call.

### Option D: same-origin only (the issue text proposal)

Do E and A, but only for frames of the origin of the parent.

- The issue says to start here "if the journeys show that this covers the need". The evidence says the opposite: the one frame that matters (the Gmail account menu, and the typical login captcha or payment frame) is cross-origin. The only same-origin content frame in the public set (the Rust book TOC) is in `noscript` and is not shown. So D covers no known need.
- Also: bare treats both origins the same (one rule, owner decision of 2026-11-18). There is no technical gain in D: the child is a separate `Browser` in both cases. The origin check would be a policy on top, with no cost saved.
- Not recommended.

### Option E0: do nothing, fix the text

Change the tool description and `decisions.md:1253` to say that iframes with a `src` run as child pages, and that the outline does not show them. An agent can still read a child by script only if it holds a reference, and it cannot: `contentDocument` is `null` and `RemoteWindow` allows only the cross-origin API. So an agent has no way to read a frame at all, even a same-origin one. That is the full cost of deferring.

### Comparison

| | A inline | B frame= | C hybrid | D same-origin |
| --- | --- | --- | --- | --- |
| Layout cost for unread frames | every full snapshot | none | none | every same-origin frame |
| Extra memory | all child frames | one at a time | chosen ones | all same-origin frames |
| Calls for a login with a frame | 1 | 2 | 1 or 2 | 1 (if same-origin) |
| New vocabulary | none | `frame=` | `frame=`, `frames=` | none |
| Covers the Gmail menu and captcha frames | yes | yes | yes | no |
| Engine part E needed | yes | yes | yes | yes |

The engine part E is the bulk of the work in every option. The option is a choice of output format, and the format can change later without changing E.

## 6. What no option solves

- `srcdoc` iframes and `about:blank` frames that a script fills (`document.write`, `contentDocument.body.innerHTML`) are not child pages. They use the shared-realm blank window. Their content is in the parent heap, not in a child `Browser`. A snapshot of them is a separate piece of work (a `Document` per blank window exists: `docs/decisions.md` "Inert iframe window..."). The evidence has no journey for this.
- A frame that Chrome renders from a service worker or a blob URL: not child pages (the rule needs http(s)).
- A frame over the limit (64 pages, depth 8) stays empty. The line must say so.
- Paint of a child in the parent box (design-gmail-frames.md step 4): not needed for the outline, needed for screenshots. Out of scope.

## 7. Recommendation

1. **Defer #963.** The evidence has 0 needed frames in the 36 public URLs that were read and the local fixtures, and one probable case (Gmail account menu) that is not re-measured since the child pages exist. Part E is a change in `Handle`, `Refs`, `Page` and the hit test, in the hot path of `act`. The cost gate would apply to it.
2. Do the text fix now (option E0): the tool description and `decisions.md:1253` say "its content is not loaded", which is wrong since 2026-11-18. This is a small change that needs no design, in its own PR.
3. Trigger to build: a journey that fails because a needed element is in a frame, or the Gmail step 5 run (below) showing that the menu is a frame that an agent must act on. Then build **option C**: E first, behind `frame=`, then `frames=`.

First step for the trigger: run Gmail step 5 on the current main with a saved session and read `diagnose`. Expected result if the menu needs a frame read: the click opens the menu in the `ogs.google.com` child page, and the outline shows only the `iframe` line. This is a hypothesis; it has not been run.

### Tests that option C needs (if it is built)

Fixtures served by two local ports (same-origin and cross-origin), run in `crates/mcp/tests/server.rs` style:

1. A same-origin iframe with a form (`input`, `button`): `snapshot frame=f1` lists the fields. `act type` fills one. `act click` on the submit button posts the form. The result is in the child document (the child navigates; the iframe line shows the new URL).
2. The same with a cross-origin iframe (second port). Cookie and `Sec-Fetch-Dest` rules do not change.
3. Nested frame (depth 2): `frame=f1>f1`, refs unique across frames.
4. A ref of a child fails as "from an older page" after the child navigates, and stays valid when only the parent re-renders.
5. A hidden frame (`display:none`) and a 0x0 frame: named on the line, not laid out unless asked (assert that no layout ran: a counter).
6. A frame that is still loading, one that failed, one over the limit: the line says `loading`, `failed`, `not loaded (limit)`.
7. A click on a control inside a child whose iframe is partly outside the parent window: the parent scrolls, the child scrolls, and the click lands (hit test goes into the child).
8. `*` marks and `removed:` in a diff after an action in a child.
9. A page with no iframe: no allocation or instruction change (cost rows of the touched subsystem, per the cost gate, plus a new row with 10 child pages, because no row has an iframe with a `src` today).

### What was not tested

- No binary was run. The map of the engine is from reading the code at `f29a583d`.
- The curl check reads served HTML only. It does not see an iframe that a script adds. 2 public URLs did not return a usable page (HTTP 202, HTTP 404).
- The live staging site and the live Gmail account menu were not checked.
- The notes on Chrome, Playwright and WebDriver BiDi are from memory and from public documentation. They were not re-read for this document.
- All cost statements are hypotheses. No instruction count or RSS was measured.

# Decisions

This file records the current decisions for `bare`. When a decision changes, update its section.

## Purpose

`bare` is a web browser written from scratch. Its purpose is to learn how the core parts of a browser work.

## Language

The project uses Rust. Servo shows that Rust works for browser engines. Safe Rust has no memory-safety crashes.

## Dependencies

Changed on 2026-11-05. Writing every part by hand cost too much time. `bare` now takes parts from open-source projects that are already fast and tested.

Allowed:

- External crates.
- Code copied from open-source projects, which we can then change and optimize.
- The Rust standard library (`std`).
- Direct calls to OS functions, declared by hand with `extern "C"`. Keep this `unsafe` code small and in per-OS modules.
- Data files from standards bodies, for example the Unicode Character Database.

Rules:

- Measure a part on our load tests before we adopt it. Speed and low memory use stay the main goals.
- Keep license notices. Copied MPL-2.0 files stay MPL-2.0. See [open-source.md](open-source.md#license-rules).
- Record each adoption and its measurements in progress.md and resources.md.

The candidate parts are in [open-source.md](open-source.md). Parts written before this change stay unless a measured replacement is better.

## Code layout

The repository is a Cargo workspace. Each browser part is one crate in `crates/`:

| Crate          | Folder            | Contents                                                   | Uses                                                             |
| -------------- | ----------------- | ---------------------------------------------------------- | ---------------------------------------------------------------- |
| `bare-unicode` | `crates/unicode/` | NFC, generated Unicode data                                | -                                                                |
| `bare-url`     | `crates/url/`     | URL parser, IDNA                                           | `bare-unicode`                                                   |
| `bare-net`     | `crates/net/`     | DNS, TCP, HTTP                                             | `bare-url`                                                       |
| `bare-html`    | `crates/html/`    | encoding, tokenizer, DOM, tree builder                     | -                                                                |
| `bare-css`     | `crates/css/`     | tokenizer, parser, selectors, cascade                      | `bare-html` (from topic 3)                                       |
| `bare-layout`  | `crates/layout/`  | box tree, text, block and inline layout                    | `bare-html`, `bare-css`                                          |
| `bare-paint`   | `crates/paint/`   | display list, pixel buffer, glyphs, PNG capture            | `bare-css`, `bare-html`, `bare-layout`                           |
| `bare-window`  | `crates/window/`  | window interface, Wayland backend (smithay-client-toolkit) | -                                                                |
| `bare`         | `crates/bare/`    | the app (`main.rs`)                                        | `bare-net`, `bare-url`, `bare-html`, `bare-paint`, `bare-window` |

Why crates, not folders in one crate:

- A crate uses another crate only through its `pub` items. Each part has a clear interface, and no part can reach into another part's internals.
- Cargo rebuilds only the crates that changed and the crates that use them. The large Unicode table builds once.
- Each crate is tested on its own.

Rules:

- A new module from the module plan becomes a new crate (for example `crates/html/`, named `bare-html`).
- Dependencies point one way: from the app down to the basic crates. No cycles.
- Unit tests stay in the same file as the code, in a `tests` module.

## Unicode data

`data/unicode/` holds the Unicode data files and their license. `tools/gen_unicode_tables.py` turns them into `crates/unicode/src/tables.rs`. The generated file is committed, so a build needs no network and no Python.

To update Unicode: download the new files over the old ones, run the script, and run `cargo test`.

## HTML data

`data/html/entities.json` is the WHATWG list of named character references. `tools/gen_html_entities.py` turns it into `crates/html/src/entities.rs`. The generated file is committed. To update it: download the new file over the old one and run the script.

## DOM storage

The DOM is an arena: one `Vec` of nodes in a `Document`. A node refers to another by its place in the `Vec` (`NodeId`). Children are a linked list inside the arena, and names are interned (an `Atom` for each name), and texts and attribute values live in one text buffer, so a node is 36 bytes with no allocation of its own. The reasons and the other options are at the top of `crates/html/src/dom.rs`.

## Box tree

Chrome, Firefox, WebKit, Servo and Ladybird all store a box tree (Chrome: `LayoutObject`, Firefox: `nsIFrame`). `bare` does not. The rules that make boxes are functions in `crates/layout/src/boxes.rs`, and layout calls them while it walks the DOM. Only the result of layout is stored.

- Gain: no memory and no build pass for a second tree. The walk costs about 40 ns for each box (`bare-stress scale boxes-`).
- Cost: the rules run again on each layout. When JavaScript changes the page (module 9), browsers use the stored tree to redo only a part of the layout. `bare` can keep the layout results for each node in a side table instead, as `Styles` does for styles.

## Inline layout

- No copy of the text: a text fragment is a byte range in the DOM text (`crates/layout/src/text.rs`). Chrome and Firefox keep a second copy of each paragraph, with the spaces collapsed.
- Culled inline boxes: an inline box with nothing to paint, no margin, border or padding, and the font of its parent makes no fragment, as in Chrome. Without this, 10,000 nested `<span>` over 10,000 lines made 100,000,000 fragments.
- The open inline boxes are a stack, and each entry keeps the most that it and the boxes around it reach above and below the baseline. A new line gets the height of the boxes that it carries over in one step, not one step for each box.
- `vertical-align` costs nothing on a line that does not use it. A box keeps its shift and the `top` or `bottom` box that it is in (`root`), and a line gets an `Item::Place` where the text changes place. A line with no change has no item, so the fast path stores nothing, and the `Item` stays 20 bytes. The `top` and `bottom` boxes are placed once, when the line is written, so nothing is laid out twice.
- Lengths use the rounding of Chrome (1/64 px for the line height, a whole pixel for the half-leading above) so the lines are as tall as in Chrome. `round` and `floor` of `f32` are casts (`font::round`, `font::floor`): the methods call the C library on a build without SSE4.1.
- A culled inline box does not work out its reach. It compares its font and line height with its parent: the same inputs give the same reach.

## Floats

- Places in the BFC while layout runs: each block gets a `Pos` (where it starts in its block formatting context, and the margins above it that are still open), as LayoutNG passes a "BFC offset" and a "margin strut". The fragments still keep places relative to the parent, so margin collapsing moves a parent with its children for free. A float or a line uses the `Pos` to ask which floats are next to it.
- The exclusion space is a step function of `y`, not a list of floats (`crates/layout/src/floats.rs`). A question is a binary search. Firefox walks its list of floats for each line: that was quadratic here (20,000 floats with a paragraph each: 840 ms; 200,000: more than 20 s). With the steps: 20 ms and 213 ms. Chrome's shelves (`NGExclusionSpace`) are a step function too, with more detail for `shape-outside`.
- A line asks for the room with the height of the strut, once. Chrome asks again when the line turns out taller. Cost of our way: a line with a big inline-block can overlap a float that starts lower down (`docs/progress.md`).

## Overflow

- An `overflow: auto` box is laid out, and laid out again with a scrollbar if the content does not fit, as LayoutNG does. The difference: the first pass stops at the first child that goes below the room, because then the box needs the scrollbar anyway. A box of 400 px with 2,000,000 lines took 1,403 ms with two full passes and 749 ms with the stop.
- In the second pass, the `auto` boxes inside use the scrollbars that they found in the first pass and do not try again. Without this, each level of nested `auto` boxes doubles the work. The cost: an inner box whose width changed keeps its old answer.
- Scrollbars are a constant of 15 px, the width of Chrome's classic scrollbars on Linux and Windows. A platform with overlay scrollbars can set it to 0.

## Position

- An absolute box leaves a fragment of zero size at its static position. When its containing block is done, one walk over the fragments of that block finds them and lays the boxes out at the end of its list, as its children. The walk skips the positioned blocks inside: they took their own boxes. So each fragment is walked once (twice if the page has `fixed` boxes, which go to the window). The static fragments go out of the list at the end of layout, in one pass. LayoutNG passes "out-of-flow candidates" up the fragment tree instead, with their offsets. A flat list of fragments makes the walk the simpler way.
- With no static position that waits, no walk runs: a page with no absolute box pays nothing.
- `position: relative` changes no layout: the offsets wait in a map, and the last pass moves the blocks. Inline boxes and their text move when their line is written, because their fragments are flat (text is not inside the box fragment).
- `position: sticky` changes no layout. The box stays at its flow place, and a scroll does not lay out again. The constraints are recorded once in `Areas` (`crates/layout/src/sticky.rs`), and the offset for the scroll of the moment is computed when a geometry read or a paint needs it, as for a scroll offset. A stored offset would need a new layout or a new walk for each scroll step. A page with no sticky box builds no list.

## Flex

- To stretch an item to the height of its line, LayoutNG and Firefox lay it out a second time with the new height, unless a cache says that the result stays the same. Here the second layout runs only if the content can depend on the height: a percent met an unknown base in the first layout (`Cx::unknown`, a counter in `block::resolve`), or the item is a scroll container, a positioned box, a flex, grid or table container, or has a `height`. Else only the height of its box changes. 200,000 rows of three stretched cards: 386 ms, and 488 ms with the second layout each time.
- The items are laid out straight into the one list of fragments, and each item keeps its range. Only an item that is laid out again gets a list of its own, and then (or when `order` sorts the items) one pass writes the fragments again in order. A list for each item cost 385 bytes for each item: 200,000 items took 93 MB and 139 ms with lists, 61 MB and 127 ms without. An `Item` is 160 bytes while its container is laid out.
- The intrinsic widths of a flex item are kept for the layout that runs (`Cx::sizes`), like those of an inline-block. Each flex container measures its items, so 30 rows one inside the other measured the same text 30 times: 79 ms for 200,000 words, and 12 ms with the cache (9 ms with the change above; blocks take 5 ms).
- A list that a nested container fills is not kept to use again when it is big: 30 levels each kept a list as big as all the content (9 MB for a page of 0.4 MB).
- `min-width` and `min-height` now start as `auto`. It is 0 for all other boxes, so only flex reads it.
- Baseline alignment in a flex line: the items aligned by `baseline` (first) and by `last baseline` share one baseline each. The first group is flush with the cross-start of the line, the last group with its cross-end. With `wrap-reverse` the two swap. A column item has its baseline on its cross-start edge. A scroll container keeps its natural baseline, but not below its bottom border edge. `align-content: last baseline` is `stretch`, as in Chrome. The baseline of a container is read as in grid (`block::PEEK`).

## Grid

- A `Value` is 8 bytes and `Copy`, and a track list is not. A track list, a line, or a set of areas goes to a side list: `Value::Grid(i)` points to a `grid::Grid` in `Side::grids` (declared) or `Styles::grids` (computed). It is the same idea as `Length::Calc`. The cascade keeps each computed list once (a hash of the value), so 100,000 equal rules keep one list. A `style=""` attribute and a `var()` offset their indexes. `inherit` copies the index. Chrome stores the track lists in a `ComputedGridTrackList` that styles share by pointer.
- A track list is parsed from the raw tokens (`crates/css/src/grid.rs`), not by `Parts`, because `repeat()`, `[a b]`, `1fr` and strings do not fit in a part. Line names are kept, as `Part::Names`, because the lines of an item can use them. The cost is one `Box<str>` for each name. `grid-auto-flow` is a number (`row` is 0, `column` adds 1, `dense` adds 2). `none` and `auto` stay keywords.
- Placement keeps the used cells in a bit set (`Occ`), one row of words for each row of the grid. A search for a free place skips 64 cells at a time. `dense` starts a search at the row where an item of the same shape was found last: the rows before it are full for that shape and a cell never becomes free, so the result is the same as the spec. A grid of N items costs about O(N + cells / 64). A plain loop over the cells costs O(N x cells) for a dense grid with spans.
- Items are laid out straight into one temporary list of fragments, at the width of their columns. The rows come from their heights. An item that stretches and holds boxes is laid out a second time at its stretched height, and the first fragments stay in the list, dead. Only the live ranges are copied to the page. A stretched item with only text gets a taller fragment: no second layout. An item with a percent height also needs the second layout, because its percent had no base in the first one. The count of static positions that waited inside a dead range is taken out of `Cx::waiting`. LayoutNG lays out a stretched item again too, unless its cache says that the result is the same.
- The contributions of an item (min-content, max-content, automatic minimum) are measured only if a track that it covers needs them. A track of fixed size (`100px`, or `minmax(0, 1fr)` with a known width) does not.
- A percent track in a grid with no size on that side is `auto` for the sizing. A `fit-content(%)` there is `max-content`. A grid with no height uses the max-content sizes for the rows (section 11.7 with no free space), and if its `min-height` (in px) is more, the rows are sized again in that room.
- An `auto-fit` track that holds no item has size 0 and no gutter. A gutter shows only between two tracks that show. The free space of `justify-content` and `align-content` goes to the tracks that show.
- A child that is out of the flow is no item. If a run of text holds one, the lines place its static position. If the run is only spaces and out-of-flow boxes, the box gets a static position at the corner of the content box.
- Baseline alignment follows CSS Box Alignment 3 and Chrome. In a row, the items that are aligned by `baseline` share one baseline, and those aligned by `last baseline` share another. The first group sits flush with the start of the row, the last group flush with its end. A row sizes for the items in it: each item adds the shift to the shared baseline to its size. An item with an auto margin on top or at the bottom is not in a group. An item that spans rows joins the first spanned row (first baseline) or the last one (last baseline). A group with one item is `start` or `end`. `justify-items` and `justify-self` with `baseline` are `start`, with `last baseline` `end`. `align-content: last baseline` is `stretch` (Chrome), `baseline` is `start`. Subgrid and masonry are not built.
- The baseline of a grid container is the shared baseline of its first row if an item has one, else the baseline of the first item of the first row that has one: its first baseline if it has 8 fragments or fewer (`block::PEEK`), else its last baseline. A read of a big item costs more than +0.5% on `geometry_load scroll`.
- A grid has at most 10,000 tracks on each side (`MAX_TRACKS`). An item past the last track goes in the last track, and a span stops at the last line (standard 9). Without it, items past the limit took one new column each, and the bit set grew with rows times columns.
- `block::flow` is gone: grid was the last container that used it.

## Tables

- No stored table state. A table is laid out in one function (`crates/layout/src/table.rs`) from the children of the box and the styles. It builds the grid in `Vec`s that live for this layout and drops them. The same idea as the box tree above: Chrome (`LayoutNGTable`) and Firefox (`nsTableFrame` with a cell map) store a grid. We read the DOM again when we need it.
- Anonymous rows, groups and cells make no fragment. They are only a place in the grid. Paint has nothing to draw for them, and a fragment would cost memory.
- One layout for each cell. The cell is laid out at its column width, its height is read, and the cell is stretched and its content moved for `vertical-align` after that. Chrome lays a cell out again when `vertical-align` or the row height changes the result in some cases. We do not: a percent height in a cell is the cost (a known gap).
- Column widths: the algorithm of `BasicTableLayoutStrategy` (Firefox) and `NGTableAlgorithmUtils` (Chrome). Four widths for the table (least, percent, fixed, most), with the width between two of them in proportion, so a table is the same width in Chrome. The cost is linear in the number of cells and columns. A cell that spans columns is spread after the cells of one column, with the same function, so the work is the sum of the spans. A span over 1,000 columns is cut, so a page cannot make a quadratic spread.
- Collapsed borders: two dense arrays, one for the horizontal and one for the vertical edges, and the widest border wins at each edge. Memory is linear in the grid. A table over 2,000,000 edges falls back to the cell borders. Each cell and the table get half of the edge, as a half pixel for an odd width (Chrome gives the same positions: 8.5 and not 9 for a 1 px border).
- Presentational hints are a layer of the cascade, not a rewrite of the DOM. The attributes of the HTML standard (`width`, `cellpadding`, ...) act as declarations with the lowest priority of the author origin (the key `1 << 96`), as the standard says. The text of the hint is part of the key of the style cache, so two cells with the same hint share one style. The hints are read only in a page that has a `<table>` element (`Hints::new` gives `None` otherwise), so other pages pay nothing.
- The table decides its own width inside `sized_with` (`block.rs`): a table is a block level box with a shrink-to-fit width that is never less than the least width of its columns. `solve_margins` is the one place that finds the margins for a given width.
- An `inline-table` is an atomic inline with the table context. The table fragment includes its captions, as the box of the table in Chrome.

## Settle rule

A page is settled when a round does no work and no task that blocks is due within the quiet window (`page.quiet`). The rule is in the header of `crates/bare/src/browser_session.rs`. The choices:

- Recurring timers and frame loops are skipped, not cancelled. A skipped timer stays queued with its due time, so `resume` and the wait for a blocking task still run it on time. The alternative, to stop or freeze the timers at the settle, would change the page and would break a later `resume`.
- A timer is judged by its own last run, not by its kind. A clock, a poll with no fetch and a heartbeat are quiet. A poll that fetches, or a timer that builds DOM, is not. The first run of a recurring timer is waited for (it is at most `quiet` away), because nothing is known before it. This costs up to one period of wall time, and gives the timer a verdict from a real run and not from a guess about its code.
- A run is quiet if it changed the DOM at most 8 times (the document revision counters, always on) and queued no fetch, script, traversal or task except its own re-arm. The rendering step and a message (`postMessage`, React's `MessageChannel`) that a run starts are not counted as queued tasks: each is judged on its own, and a noisy one makes the timers be judged again. A page whose clock sets state at each tick queues both, and without this it never settled (staging `/calls`, 95 to 146 rounds, 25 to 39 s). The 8 allows a clock with several fields and a spinner. It does not allow a loop that grows the page by a few nodes each tick forever; the page limits stop that.
- A re-armed `setTimeout` is detected by the identity of the callback function (it is the callback that is running). It is free: one compare in `setTimeout`. A new closure each time (`() => tick()`) is not detected and blocks while it is within the window. Comparing the code of the closure would need the interpreter and was not measured to be needed.
- A timer under 16 ms (one frame) is a work loop and always blocks. It also keeps the old behavior for a `setInterval(f, 0)` spin, which a task budget must still stop.
- A frame loop is treated like a timer: the rendering step is quiet if the frame changed the DOM at most 8 times and queued nothing but the next step. A spinner or a progress bar that writes one style in each frame does not keep the page busy. A frame that builds DOM, starts a fetch or queues a task does. A call to `requestAnimationFrame` from outside a frame (a click, a timer) takes the verdict back, so a frame that someone asked for is never skipped.
- An idle callback chain (`requestIdleCallback`) is judged like a frame loop. A period that ran callbacks is quiet by the rule above, and then the queued `Task::Idle` and `Task::IdleTimeout` are skipped. Until the first period ran, nothing is skipped, so a first callback always runs (React scheduler and analytics defer work there). A request from outside the chain (a click, a timer) takes the verdict back. A request inside an idle callback does not. `Task::Idle` and `Task::IdleTimeout` are not counted as queued work, as for the frame and a message: a timer that asks for an idle callback is quiet, and a noisy period makes the timers be judged again. `PageResult::idle_callbacks` is the number of callbacks that wait in a skipped chain.
- Any fetch the driver makes for a link that a timer, frame or idle callback added clears all verdicts (`Dom::forget_quiet`): a style sheet (also a failed one and a style preload), and a script or opaque preload. A script or `fetch` that the callback queued is counted by the run itself. A cached URL sends nothing and clears nothing. Images are not fetched.
- No clock is changed. The virtual clock moves only while a blocking task is near, so a virtual-clock session that settles has not run its skipped timers for the rest of the time.

## Parallel downloads

A page asks for many files. Each one costs a round trip (about 40 ms to staging), so 17 files in a row cost 0.6 s. The loader and `fetch` start the files together and take the results in the order of the page. The numbers are in [resources.md](resources.md#parallel-downloads). The choices:

- Worker threads, not async I/O and not a poll loop. The page thread runs JS and cannot be `Send`; a download is a blocking call that owns its data. `Client::spawn` runs a job on a worker and gives a `Task` to wait on. A new async runtime would add a large dependency and a second way to do I/O.
- At most 6 workers and 6 idle connections for each server, as Chrome does for HTTP/1.1. A `Client` and its forks (`Client::fork`) share the pool, the root certificates and the workers. A page makes three clients (document, scripts, `fetch`); before, each opened its own connection and read the system roots.
- A worker waits 1 s for a job, then its thread ends. A page that asks for a few files at a time starts at most 6 threads, not one for each file. A parked thread uses no CPU.
- At most 2 new connections are set up at once (`Gate`). A burst of 6 new connections to staging loses SYN packets, and each lost one costs a retry after about 1 s. Measured: 6 at once, 2 to 4 late; 2 at once, none (to verify on other servers).
- The loader reads the targets of the parsed document in order and starts the next ones while fewer than `WINDOW` (32) wait. The window was 6 at first: one slow file then held back every file after it, though workers were free. The workers limit the wire, so a larger window costs only queued jobs. A page that ends drops the jobs that have not started. A result is taken when it is the turn of the element. Script order, `defer`, `async`, `onload` order and the errors are the same as before; only the waiting is shared. The text of a file that finished early counts against the byte caps when it is taken, not when it is read.
- `fetch` and XHR: the first 32 queued requests (`AHEAD`) that are a GET or a HEAD start together, up to the first request of another method. A request with another method runs alone, when all before it are done, on the page thread. The server sees these in the order of the script.
- No worker is used until a reply took 10 ms or more to start (`Client::parallel_pays`), and not for a single download. On a fast link (loopback, a LAN) a thread costs more than the round trip it hides: without this the loader rows of `resources.md` rose by up to 4% in instructions. The first request of a page is the document, so a remote server marks itself before the first script.
- Timing is off unless `BARE_NET_TRACE` is set. When it is off, the cost is one read of a `OnceLock` for each request, and no clock read for the trace.
- HTTP/2 is used when the server picks `h2` in ALPN (see [HTTP/2 is our own code](#http2-is-our-own-code-one-reader-thread-for-each-connection)). Staging answers HTTP/1.1 only (Chrome reports `http/1.1` for all 69 requests of three loads; `h2` gets no ALPN, see [staging-network.md](staging-network.md)), so staging does not change.
- The request body is copied for a worker (a `Pending` holds values of the realm and cannot leave the thread).
- A session keeps its first client. Each document and each input (`Session::resume`) gets a fork, so the idle connections and the slow-link flag (`parallel_pays`) stay, and the cache of each document is its own. Before, each input made a new client with no connections.
- Every connection sets `TCP_NODELAY`. With Nagle, the first request on a new TLS 1.3 connection waited for the ACK of the client Finished (about one round trip).

## Compressed bodies and the connect backup

Follow-ups of [Parallel downloads](#parallel-downloads). The numbers are in [resources.md](resources.md#compressed-bodies-and-the-connect-backup). The choices:

- The client asks for `gzip, deflate` and, over https only, `br` (as Chrome does). The decoder is a chain of `Read` stages over the body, built on the first read. The page loader is the exception: a gzip or deflate download is read whole and decoded in one step (see the end of this section).
- Fetch rules: codings are undone from the last to the first, at most 5. Headers stay as sent, so `content-encoding` and `content-length` are visible to a script, and `fetch` shows the decoded bytes. A coding that the client does not know (`zstd`) is passed through unchanged, and `Response::encoded()` is true. The callers (loader, session, `fetch`) refuse such a body with an error, as before.
- The decoded size has the cap of the wire size (`MAX_REPLY`, 128 MiB), and the caller caps apply to decoded bytes. A body that grows past the cap is an error, so a zip bomb stops at the limit. A bad gzip, deflate or br body is an error. A body with no content (HEAD, 204, 304) is not decoded.
- When the decoder ends, the client drains the raw body. That checks the framing and returns the connection to the pool. A connection that is not drained is not reused.
- `deflate` can be a zlib stream or a raw stream. The first 2 bytes decide.
- The client sends no `Accept-Encoding` to a loopback host (`localhost`, `127.x.x.x`, `::1`). It saves no byte there and costs both ends CPU, and the loopback rows rose up to 2.15% in instructions when it was sent. A header that the caller sets wins. `IpAddr::parse` was too slow for this check (about 380 instructions for each request), so a string test is used.
- `Vary: Accept-Encoding` alone no longer blocks the client cache. The client sends the same `Accept-Encoding` for every request to a host unless the caller sets one, so the key does not differ in practice.
- The client sends `Accept-Language: en-US,en;q=0.9` on every request that has none, over HTTP/1.1 and HTTP/2. It matches `navigator.languages`. It skips loopback hosts, as `Accept-Encoding` does. Some sites (mozilla.org) serve a locale chooser when the header is missing. A reply with `Vary: Accept-Language` is not cached by the client, because only `Vary: Accept-Encoding` is.
- A `<meta http-equiv=refresh>` in the head starts a timer at document load. The timer calls `location.replace`, so the entry is replaced. It counts as queued work. `SessionLimits::max_documents` ends a refresh loop.
- Crates: flate2 with miniz_oxide (already built for `png`; zlib-rs is 19% faster, but the gain for 687 KB is 0.4 ms) and brotli-decompressor (no encoder). zstd is not added: its decoder is C or slower pure Rust, and servers answer gzip and br.
- Connect backup: a connect that is not done 250 ms after the first SYN starts a second socket to the same address (as the backup connect job of Chrome). The first socket to finish wins and the other is closed. A failed connect (a refusal) starts no backup. The `Gate` of 2 new connections is not changed, because the backup runs inside the same `connect` call. The staging stall was a lost SYN: the kernel retry comes at 1 s; the backup connects at about 285 ms.
- Happy Eyeballs between IPv6 and IPv4 already existed in `tcp.rs` (interleaved addresses, 250 ms). Staging has one IPv4 address.
- XHR `progress`: `total` stays the wire `Content-Length` and `loaded` counts decoded bytes, as the spec says.
- Whole-body decode (added after the RSS check): the loader keeps every script and style sheet whole, so it reads a gzip or deflate body whole and decodes it in one call (`Body::decoded_whole`). The decoder needs no 32 KiB window then, and the output has its final size from the gzip trailer. 6 workers decode at once, and each keeps its high-water mark in its own heap. It gave back about half of the +920 to +944 KiB that the streaming decoders added to the peak RSS of `/login`. Streams, `br` and a list of codings keep the streaming decoders. Details: [resources.md](resources.md#peak-rss-of-the-decoders).

## Identifier lookup

- A name is resolved by the scope it is in, not by a search of the environment chain. The first walk of a script records the static scopes (script, function, block with lexical declarations, `for`/`for-in`/`for-of` with `let` or `const`, `catch`, `switch` with lexical declarations) and the scope of each identifier site. The second run of a site stores the number of environments to go up and the index of the binding. The first run uses the full search, so code that runs once pays nothing.
- A class is a barrier. A site behind a barrier, and a site named `arguments`, use the full search.
- A stored index is a hint. A name check guards it, and a miss runs the full search. A wrong hop count is the only unsafe error, so a debug build checks every resolved lookup against the full search.
- A name that no scope declares goes to the global object. A per-name hint holds the position of its property, and a key check guards it.
- Frame slots for variables that no closure captures: done for leaf functions only. See [Frame slots](#frame-slots).
- `with` opens a barrier for its body (see [The `with` statement](#the-with-statement)). A direct `eval` would have to do the same.
- Reason: a bundle reads outer variables through a chain of closures, and every read searched each environment on the way. See [resources.md](resources.md#cost-of-identifier-lookup).

## Subclassing built-in constructors

- A built-in object is a plain heap object plus a side table keyed by its id (`colls`, `weak.colls`, `date.times`, `promise.map`, the buffer and view kinds). So a subclass instance needs only the right prototype. No constructor makes a hidden "subclass" object.
- `construct_nt_inner` (Construct with a different `new.target`) puts `new.target` in `cls.native_nt` before it calls the native constructor. The constructor takes it first (`native_nt.take()`), so a nested construct inside an argument conversion cannot see it. `nt_proto` reads `prototype` of `new.target` before the object exists. `nt_patch` sets it after, for the constructors that make the object in a helper (`Promise`, typed arrays, `DataView`, `TextDecoder`). `Date` passes the prototype to `date_new`.
- A host constructor opts in with `add_ctor_nt` and reads `host_proto_from_new_target(default)`. A plain `new F` passes no `new.target`, so the host constructor gets `default` and runs no property read. This is exact because the `prototype` of a host constructor is read-only. The `default` must be that `prototype`. The read cost about 1% on rows that make events.
- `NewPromiseCapability` for a constructor other than `Promise` runs the constructor with an executor that keeps `resolve` and `reject` in a holder object. A side table `caps` maps the new promise to the two functions. The mark phase queues them once, at the end, for each reached promise in the table. A per-object check cost +0.8% on every GC row, so it is not there. The intrinsic `Promise` never uses the table. Its resolving pair is the old direct path, so a plain `Promise.resolve(x)` allocates no function.
- A custom capability in the combinators uses the pair id `u64::MAX`. The "already resolved" flag then lives in the user functions, not in `done_pairs`.
- `species_ctor` replaces the old gate for `then` and `finally`. The species constructor of an array, a typed array, an `ArrayBuffer` and a `Promise` is read once, with the V8 messages. A species that is the intrinsic constructor takes the old fast path.
- The brand of an error object is not stored. `Object.prototype.toString` tests an own `stack` and `Error.prototype` in the chain. Reason: no extra field for each error, and the tag matches V8 for the cases of the corpus. It is a heuristic, listed in progress.md.
- Done later (2026-11-07): `Request`, `Response`, `Headers`, `URL`, `Blob`, `File`, `FormData`, `AbortSignal`, `Function`, `Symbol`, the `Intl` constructors, `flat` and `flatMap`, and an own `exec` on a `RegExp` subclass. See the next section.
- Cost rule for this change: no `instructions:u` row worse than +0.5% against c66c8a5. The worst row is +0.41%. See [progress.md](progress.md#class-x-extends-built-in).

## Native subclass conformance

- A host constructor reads `Get(new.target, "prototype")` first, before it converts its arguments. Chrome does the same, and a getter on `prototype` shows the order. This replaces the old order (arguments first) of `URLSearchParams`, `Blob`, `File` and `FormData`.
- When `prototype` of `new.target` is not an object, a host constructor gives `Object.prototype`. Chrome does that (a V8 API object has no intrinsic default prototype). A JS built-in constructor still takes the intrinsic prototype of its own kind, as the specification says.
- Headers, Request, Response and URL opt in with `add_ctor_nt`. An internal object of the same kind (`Request.clone()`, `fetch()` results, `url.searchParams`) takes the intrinsic prototype, not the one of the subclass. Chrome does the same.
- `AbortSignal` opts in only to throw `Failed to construct 'AbortSignal': Illegal constructor` after the prototype read, as Chrome does.
- `Function`, `AsyncFunction` and `GeneratorFunction` share `Nat::FnCtor(kind)`. The source is parsed as a one-declaration script, walked without being run, and closed over the global environment. The default prototype of `AsyncFunction` is a lazy intrinsic (`async_fn_proto`), as `gen_intr` is for generators.
- `Symbol` is a constructor for `new.target` purposes only, so `class X extends Symbol {}` is legal and `new X` throws `Symbol is not a constructor`, as Node does.
- The Intl constructors read `new.target` and use the plain `new` path at no extra cost.
- A RegExp subclass with its own `exec` runs the generic algorithms of the specification (RegExpExec, `lastIndex` and `flags` read as properties, a species-built splitter and matcher). The builtin fast paths stay exact only while `exec` is the intrinsic, so every `RegExp` row pays one check (`user_exec`), not a property read per call. `RegExp[Symbol.species]` exists now.
- `flat` and `flatMap` build the result with `ArraySpeciesCreate`. An array whose species is the intrinsic keeps the old path.
- Done later (2026-11-07): `AsyncGeneratorFunction`, also through `Nat::FnCtor(3)`. See "Async generators and `for await`".
- Not done: `Function.prototype.toString` source text of a dynamic function; own `arguments` and `caller` of a dynamic function; a Proxy as a species result (no trap dispatch); the V8 reads of the `global` and `unicode` getters in `replace` and `match`; lone surrogates in a dynamic function body; `this_regexp` still needs a real RegExp receiver.

## Async generators and `for await`

- Async generators run on the async machine (`eval_async.rs`), as generators and async functions do. A `Run` has an `agen` flag. A generator object maps to an `AGen` in `aw.agens`: the state (`Start`, `Yield`, `Running`, `Draining`, `Done`) and the queue of requests. A request is the resume kind (`GenHow`) and its promise. The queue, the drain and the await of a `return` follow AsyncGeneratorEnqueue, AsyncGeneratorResumeNext, AsyncGeneratorDrainQueue and AsyncGeneratorAwaitReturn.
- `yield v` awaits `v` (frame `AYield`), and `return v` awaits `v` (frame `ARet`). The preflight marks every `return` with a value in an async generator, so these returns run on the machine.
- A `for await` and an async `yield*` keep the iteration in an `It` slot, as `for-of` does. `It.i = 1` marks a sync iterator that runs through the async-from-sync iterator. That iterator is not an object: its `next`, `return` and `throw` call the sync method and await the value. A rejection of the value closes the sync iterator, and a missing `throw` closes it and rejects with a `TypeError`. That is the current specification and test262. Node 22 does neither.
- Async `yield*` with `return`: when the inner iterator has no `return`, the value is awaited. When the result of `return` is done, its value is not awaited again. Node 22 does both, and the specification awaits the done value. We follow Node 22 for the done value, because the corpus checks the order of microtasks against Node.
- The new frames are handled in the cold functions `ag_deliver`, `ag_unwind`, `ag_yield` and `aof_start`. Inline, they cost up to +0.9% in `drive` on the async and generator rows, because `drive` grew. Out of line, those rows are 0.25% faster than main on average.
- A reached generator object traces its suspended run and the values and promises of its queue. A generator that no code can reach is collected with its run. A running generator that awaits is held by the activation of the await, as an async function is.
- Not done: `Array.fromAsync` (it needs its own promise state machine; 66 test262 tests); `Symbol.asyncDispose`; `yield` or `await` inside a `for await` binding pattern (the preflight gives "await in a for-of binding").

## The `with` statement

- `with (obj) body` makes an object environment record (ECMA-262 9.1.1.2): an ordinary environment with one binding, named `WITH_NAME`, flagged `WITHB`, whose value is the object. A closure that captures it keeps it, and the collector traces the object. `Env` did not change.
- The first walk opens a `Barrier` scope for the body. A site that must leave the body (also in a function or an arrow made in it) stays dynamic. A site that a scope inside the body declares stays resolved, because no object environment lies between the site and its scope. A site outside the body is not changed: the object environment is in the chain of the body only. The function that holds the `with` gets `FL_NOFR`. The bytecode scope walk (`bc_scope.rs`) opens the same barrier, and `bc_gen` still refuses the statement.
- `Interp::with_any` turns on at the first `with` that runs. It is the only cost of the feature for code that has none. The plain search `lookup` did not change. A dynamic site (`find_name_slow`) calls `lookup_dyn`: `lookup` with the flag off, `lookup_with` with it on. `lookup_with` asks each object environment on the way: `HasProperty`, then `@@unscopables` (a proxy trap and a getter run).
- A hit is the binding of the object environment. It has neither `INIT` nor `MUT`, so `get_name` and `assign_name` leave their fast path at the checks that they make already. The cold functions `get_name_uninit` and `assign_name_refused` see `WITHB` and do GetBindingValue and SetMutableBinding on the object (each asks `HasProperty` again, as the specification says; strict code throws `ReferenceError` if the property is gone). The error of a trap during the search waits in `HScope::with_err`, because `lookup` cannot return it. A call through a name takes the object as `this` (`link_base`, one test of `with_any`). `typeof name` and `delete name` resolve the name once, in cold functions.
- The statement runs as a native call (`exec_with` runs the body with `exec`, as `for-in` does), so a `with` is a native frame and counts for `max_traversal_depth`.
- Gated, with `Unsupported`: `with` in a generator or an async function (the resumable machine does not know the environment).
- Deviations: a name is resolved when the code reads it, not before the right side of an assignment runs. `x++` and `x += 1` resolve the name twice, so a proxy sees two `has` calls. A parenthesized callee `(f)()` and a tagged template tag do not pass the object as `this`. There is no `eval`, so the test262 tests that use `eval` still fail.
- Cost lesson: the walker is one group of functions that call each other (`w_stmt`, `w_expr`, `w_pat`, `w_func`, `w_class`). A new function in that group, even `#[cold]` and `#[inline(never)]`, made LLVM stop inlining `w_expr_inner` and `w_pat_inner`, and `scripts_load few 10` went +0.58%. The `With` arm is in `w_stmt_inner` and walks its children there. The gate (`w_with_gate`) calls nothing in the walker.

## Frame slots

- A leaf function keeps its variables in a frame, not in an environment. A leaf function has no inner function and no class. A frame is a run of bindings at the end of the bindings of environment 1, from `Interp::fp`. A call makes no environment. A block, a loop head, a `switch` or a `catch` inside the function makes none either. A `let` or `const` goes back to its TDZ each time its block is entered. V8 keeps such variables in registers and SpiderMonkey in frame slots.
- A function keeps its environment when it has an inner function or class, `arguments`, `this` (an arrow function may use it: it reads the one of its closure), `new.target`, `super` or `delete name`. It also keeps it when it is a constructor, an async function, a generator, a named function expression, has a parameter pattern or default, repeats a parameter name, or has more than 255 names. A generator or an async function must keep its variables across a suspension. This engine has no `eval`. A function that holds a `with` keeps its environment too, because the full search cannot read a frame slot.
- Frame slots are for leaf functions only. A function with an inner function keeps its environment, because a closure reads the variables of its parent through the environment chain. Not done: a capture analysis that gives a frame to a function whose names no closure reads.
- A function that runs once pays nothing for frame slots. The first walk of a script only sets flags in the function (`FL_NOFR`, `FL_THIS`, `FL_EAGER`). It does not classify the function. The classification (`fr_leaf`) and the layout (`fr_layout`) run in `fr_gate`, which is `#[cold]`, at the second call of the function. A function with a loop or a call is marked `FR_EAGER` and gets its frame at its first call, because one call of it can be long or recursive.
- The first call of a function runs in an environment. No site of it resolves during that call (`FR_WAIT`): a site that resolves for an environment would be wrong for the frame of the second call. `typeof name` reads its site twice in one call, so the rule is in `find_name_slow` and not in the code that sets the first-run mark. If the function is on the call stack again when it is called (a recursive call, or a getter that calls back), `fr_gate` finds it there and keeps the environment for good.
- A class member has `FuncMode::Env`. The parser sets it. `call_script` checks the mode, so a class member pays one compare for each call and nothing else. A method of a class is never a leaf in this design. Not done: frame slots for class members. They would need the check in each call of a member, and `class_load` rows are within 0.3% of the limit already.
- The walker does not clone the program `Rc` for a leaf node (`w_expr_inner` and `w_pat_inner` return early for an identifier or a literal). This saves the cost of the walk, which is now paid for each function once. The same change in `w_stmt_inner` made `scripts_load few 10` +0.9% (code layout), so it was not kept.
- Not done: no environment for a block of a function that has an inner function. The profile shows the environment arena at 0.5% to 8.6% of the rows (see [resources.md](resources.md#cost-of-frame-slots)). The cost of a call is binding setup and name access, and a block environment is not among the top ones.
- Memory: `FScope` has the same size as before (32 bytes: `lo` is the first scope until the function is decided and the template start after, `fr_len` is a `u8`). Each `RScope` has 4 bytes more (28 against 24: the catch scope holds the `try` statement, and `base == NONE` marks a scope that is not a frame scope). The first version had 8 and 12 bytes more. See [progress.md](progress.md#follow-up-of-frame-slots-encodeuri-generator-rows-and-metadata).

## Shapes and inline caches

- Plan: [engine-plan.md](engine-plan.md). Two lanes: shapes with inline caches on the tree walker (lane A), and a bytecode interpreter (lane B). Both are approved. They share the cache API below. A is smaller and gives memory. B gives most of the time.
- The tree walker stays as tier 0 for code that runs once. It is not removed (Arjun, 2026-11-07; step B13). `Auto` mode runs a function on the walker first and compiles it later.
- The API is in `heap_shape.rs`. `Ic` is plain `Copy` data of 24 bytes with no pointer: `shape u32`, `slot u32`, `kind u8` (empty, own, proto, absent), `holder ObjId`, `epoch u32`. A stale `holder` fails the generation check of the handle. `Heap::ic_own(o, &ic)` is `#[inline(always)]` and answers only an own hit (`None` otherwise). `Heap::get_ic(o, key, &mut ic)` and `Heap::set_ic(o, key, v, &mut ic)` are `#[cold]`: they do the plain `get` or `set` and fill the cache. Until step A2 `ic_own` always gives `None` and the other two call `get` and `set`.
- The caller calls `ic_own` inline and `get_ic` out of line on `None`. The tree walker and the bytecode op `GetField dst, obj, ic_index, name_const` use the same two calls. The tree walker keeps its entries in `Interp::ics: IcState` (`eval_ic.rs`). The bytecode interpreter keeps them in `Code::ics`. The two tables are disjoint fields of the heap, so there is no borrow conflict. A function can run on the tree first and on bytecode later. The caches are separate, so the only cost is one more warm-up.
- Each lane adds its state as one sub-struct (`ics: IcState`, `bc: BcState`) in its first step. Later steps edit the sub-struct and do not change the field list of `Interp`. This keeps merge conflicts small.
- Shapes (step A1): a transition tree. Roots differ by object kind. A shape holds the key, the attributes and the count. The key table is shared along a chain. An object keeps `proto` as now, plus `shape: u32` and `slots: Vec<Value>` (16 bytes for each property instead of 48). Elements stay in `elems`. Dictionary mode is today's `PropMap`. An object goes to dictionary mode when a property other than the last one is deleted, an attribute of a property changes, `setPrototypeOf` runs, it has more than 128 properties, its chain from the root is longer than 1024, or it is exotic (proxy, host named or indexed, typed array, `String` wrapper). A dictionary object never hits a cache.
- Invalidation (step A2): `Heap::proto_epoch` is one counter. Each object that a fill walked over gets the flag `IC_CHAIN`. Adding, deleting or reconfiguring a property of a flagged object, or `setPrototypeOf` on it, bumps the epoch. A prototype hit checks the shape, `obj.proto` and the epoch. It reads the value from the holder at hit time, so a value change needs no invalidation. Hypothesis: epoch bumps are rare after start. If a page makes many, use a validity cell for each prototype.
- A debug build compares every hit with `Heap::get` and panics on a mismatch. A wrong cache is a silent wrong answer, so this check stays.
- Perf rule for every step of the plan: no `instructions:u` row worse than +0.5% against the previous main, with glibc and with jemalloc. A row that is worse only with glibc is heap noise.
- Only one engineer works in `heap.rs` at a time (A0 to A7 are a chain). New bytecode files (`bc*.rs`) are separate modules of the crate and are not in the `include!` blob of `eval.rs`.

## Bytecode

- Plan: [engine-plan.md](engine-plan.md), lane B. The format is in `crates/js/src/bc.rs` (step B0). The code generator is in `bc_gen.rs`, `bc_gen_expr.rs` and `bc_gen_stmt.rs`, and the verifier in `bc_verify.rs` (step B2). The loop (B3) comes later. A bytecode design alone is not faster: Boa has bytecode and lost to this engine on 6 of 8 rows. So `Value` stays `Copy`, there are no reference counts and no clones in the hot path. Stop and re-plan if the `byte` mode is not 3 times faster on `eval_load loop 10` at step B3.
- A register machine with three-address instructions. The registers of a frame are one slice of one `Vec<Value>`. The alternative was a stack machine (QuickJS). A register machine needs fewer instructions for the same code, and each instruction does more work for the same decode cost.
- One instruction is one 32-bit word `[op:8][a:8][b:8][c:8]`. Four forms: `Abc` (three 8-bit operands), `Abx` (`a` and a 16-bit unsigned operand), `AsBx` (`a` and a 16-bit signed operand, for a conditional jump or a small integer) and `Sj` (a 24-bit signed jump). A property instruction has four operands, so its `AbcX` form has one raw 32-bit word after the instruction (the name constant). A jump offset counts words from the word after the instruction.
- Wide form: the prefix word `Wide` makes the operands 16 bits (`Abc`: three 16-bit operands; the others: `a` of 16 bits and one 32-bit operand). `bc::encode` uses the narrow form when every operand fits, so the generator never chooses. Reason for words and not bytes: decoding is a shift and a mask, the loop reads `code[pc]` with one bounds check, and the code is safe Rust. Bytes are about 30% smaller but cost more to decode (hypothesis, not measured).
- At most 255 registers for each function in step B2. A function that does not fit stays on the tree walker. The wide form can name more, so the limit can rise later with no change of format.
- `Code { ops, consts, ics, handlers, lines, nregs }`. `consts` holds numbers, strings (UTF-16 code units) and interned names. `ics` holds one `Cell<Ic>` for each property instruction (see [Shapes and inline caches](#shapes-and-inline-caches)). `handlers` maps a range of words to a handler word and the register that gets the exception. The first range that holds the word is the innermost one. `finally` uses a completion register and ordinary jumps, as Ignition and QuickJS do. `lines` maps a word to a source offset for error messages. `bc::Code::disassemble` prints all of it.
- Dispatch is `loop { match op }` in safe Rust, which compiles to a jump table. Computed goto does not exist in Rust, and explicit tail calls (`become`) are unstable on rustc 1.96 (error E0658). Every slow path is `#[cold]` and `#[inline(never)]`. The same change took the frame of `machine` from 1,120 to 416 bytes. Target for the loop: 400 bytes of native frame or less.
- Frames: one `Vec<Frame>`. A JS to JS call pushes a frame and continues the same loop, with no Rust recursion. A native function that calls back (`map`, `sort`, a getter, a proxy trap) starts a nested loop. `max_call_depth` and the `RangeError` stay as they are. Limits and unsupported constructs still bypass `catch` and `finally`, as today.
- Scopes, stage 1: a bytecode function keeps the exact scope layout of the tree walker (declaration order, one `Env` for each scope that has one now). So a bytecode function and a tree function can sit in one scope chain, and the two engines share the `Env` arena. Stage 2 (step B12) moves uncaptured locals to registers and changes the layout rule for both engines together.
- The switch: `Interp::set_exec(Exec::{Tree, Auto, Byte})`. The default is `Tree` until step B11. Examples and the `bare` session read `BARE_JS_EXEC`. The library reads no environment variable. `Auto` runs a function on the tree walker first and compiles it at its second call, or at its first call when it has a loop. This is the rule of `fr_gate` for frame slots, for the same reason: code that runs once pays nothing. A function that the compiler cannot compile stays on the tree walker for good, and `Interp::stats()` counts it.
- Decision (Arjun, 2026-11-07, step B13): the tree walker stays as tier 0 for code that runs once. It is not removed. The gates run in `tree`, `auto` and `byte` modes for as long as both engines exist.
- The `bc*.rs` files are modules of the crate with `pub(crate)` access to what they need. They are not in the `include!` blob of `eval.rs`, which keeps the compile time and the code layout of the walker steady. The bytecode state is one sub-struct (`Interp::bc: BcState`, `eval_bc.rs`).
- Not done in B0: no opcode is generated or run, and the op numbers may change until B3. B2 may add ops. Generators, async functions, classes, destructuring and `for-in` come in later steps. Classes and templates first use coarse ops that call the helpers of `eval_classes.rs`.
- Code generator (B2). It compiles one function. The code runs in the environment that `bind_call` of the walker makes (`this`, the parameters, `arguments` and the `var` names). `DeclEnv s` adds the `let`, `const` and function bindings of the function scope. Each block, `switch`, `for` head with `let` or `const`, and `catch` that has a scope in `bc_walk` gets one `PushEnv s` and one `PopEnv`. `CopyEnv` copies the environment of a `for` with `let` before the test and before the update, as `for_head` and `for_update` do. A name gets its op from `bc_resolve`: `GetEnv` and `SetEnv` (hops and index), `GetGlobal` and `SetGlobal`, or `GetName` and `SetName` (the full search: `arguments`, a site behind a class, or a binding that does not exist yet). The generator follows the order of the walker: `ToKey` coerces a computed key before the value, `ClosureName` names an anonymous function, a call through a member (also in parentheses) passes the object as `this`.
- `finally` (B2): two registers, the kind (0 normal, 1 throw, 2 + i for exit i) and the value (the exception or the return value). A `break`, `continue` or `return` that leaves a `try` with `finally` pops the environments to the depth of the `try`, sets the kind and jumps into the `finally` block. The end of the block compares the kind with each exit and jumps, then throws the value for kind 1. `Handler::depth` is the environment depth of the handler, so the loop (B3) pops the environments of the `try` before it enters the handler.
- Limits of B2: a jump is always narrow (24 bits for `Jmp`, 16 bits for a conditional jump). A function that needs a longer jump stays on the tree walker. Script bodies are not compiled (B3 decides how the top level runs). An unsupported node gives a reason, and `Interp::bc_gen_check()` counts the reasons.
- The verifier (B2) checks each `Code` before the loop runs it: every word decodes; registers, constants and inline caches exist and each inline cache has one instruction; jump and handler targets are the first word of an instruction; the environment depth is the same on every path, never below 0 and not below the depth of a handler in its range; no path runs past the end. So the loop (B3) can index registers and constants with no check of its own (hypothesis, to measure in B3).

## Code style

- Plain code. No clever tricks.
- Comments explain each Rust concept the first time it appears, with a comparison to JavaScript.
- Each module has tests.

## Architecture: one engine process for all tabs

All tabs run in one engine process. Chrome is different: it uses one renderer process for each site.

Goals:

- Less memory. All tabs share one copy of the engine.
- Each tab has its own heap. No object is shared between two tabs.
- The engine counts the memory of each tab.
- The engine finds the cause when memory or CPU use is too high. The cause can be the page or the engine.
- When a page uses too much, the engine limits it. The engine can freeze the tab, or discard it and reload it.
- A watchdog stops JavaScript that runs too long, for example `while (true) {}`.

Known limits:

- Isolation in one process does not stop Spectre. A page can read memory of other tabs in the same process. Source: Google, "Spectre is here to stay" (2019). Mitigations: no JIT, coarse timers, no `SharedArrayBuffer`. These reduce the risk. They do not remove it.
- A crash in the engine stops all tabs. A Rust panic or an out-of-memory abort stops the whole process.
- The OS cannot limit memory for one tab in a shared process. The engine must do the accounting itself.

## Platforms

Linux (Wayland) is the priority. Windows and macOS come later.

Design rules, so that a port is small:

- Paint draws into a plain pixel buffer (`Vec<u32>`). It does not draw into a window.
- Only one trait touches the OS window. It opens a window, shows a pixel buffer, reads input events and reports a resize.
- Each OS has its own implementation of that trait:
  - Linux: the Wayland protocol over a Unix socket, with a shared-memory pixel buffer. Built in `bare-window` on smithay-client-toolkit, chosen on 2026-11-05 over winit with softbuffer (see [resources.md](resources.md#wayland-window)). The interface is `App`, `Control`, `Waker` and `Window` in `crates/window/src/lib.rs`. A trait object is not needed: a port adds a module with the same names.
  - Windows: Win32 functions (`CreateWindowExW`, `StretchDIBits`).
  - macOS: Cocoa through the Objective-C runtime (`objc_msgSend`).
- Other OS calls (for example memory with `mmap`) also go behind small per-OS functions.
- Each port is built and tested on its own OS.

## TLS

`bare` implements TLS 1.3 and its crypto by hand (SHA-256, HKDF, X25519, AES-GCM, X.509 certificate checks). This is for learning only. Hand-written crypto is not safe against real attackers. Do not use `bare` for sensitive sites.

## Fonts

Changed on 2026-11-05. Text is drawn from the outlines of Noto Sans Mono, the font whose numbers layout uses. `data/fonts/` holds Regular and Bold, each cut down to 22 KB (Basic Latin, Latin-1, common punctuation, bullets, boxes and arrows), with the OFL license. The files are built into the program, so no system font is read and the pixels do not depend on the machine. Two more built-in faces fill the gaps: a Noto Sans Symbols2 subset (4 KB; check marks, crosses, stars) and a Noto Sans Arabic Regular subset (86 KB; the Arabic blocks U+0600-06FF, 0750-077F, 08A0-08FF, FB50-FDFF, FE70-FEFF). So there are four built-in faces. The Arabic face is read on the first Arabic letter, so a page with none pays nothing. A letter that no face has (Chinese, Japanese, Korean, emoji) is not drawn. No system font is read, ever. Pull request #313 read an installed Arabic font; that broke this rule and was removed. There is no bitmap font.

## Paint

- Layout's fragments become a display list (rectangles, text runs, clips) in CSS painting order, and a second pass draws the list into a `Vec<u32>` of opaque pixels. The list holds only what is on the canvas. The `Painter` keeps its buffers and its glyph cache between frames, so a repeat paint allocates nothing.
- Rectangles are hand-written: 3.4 times faster than tiny-skia `fill_rect` in a spike. Glyph outlines are filled by ab_glyph into coverage masks, cached for each (face, glyph, size, quarter pixel). tiny-skia was measured and left out: 6.5 times slower for glyphs than ab_glyph and 1.1 MB more binary. fontdue was faster but cannot place a glyph at a fraction of a pixel. The PNG capture uses the `png` crate (261 KB for a page-like 1280 by 800 picture, against 4.1 MB with no compression). See [resources.md](resources.md#paint-and-pixel-capture) for the numbers.
- Gaps (no `z-index`, solid borders only, no radius, no images or scrollbars) are listed in resources.md. Add each one when a target page needs it.

## Module plan

The next target is the voicebot staging login journey. Implement HTTPS and resource loading, JavaScript and a single-page event loop, browser APIs, window/paint/input, and test automation, in that order. Tabs remain deferred. JavaScript uses the event loop before tabs exist.

The topic order, exit checks and state are in [progress.md](progress.md#voicebot-staging-plan). Existing module numbers remain stable for code and benchmark references.

For each topic: implement, run huge load tests, then optimize CPU time and RAM use. Repeat the load-and-optimize cycle before moving to the next topic. Record results and tradeoffs in [resources.md](resources.md).

Browser conventions do not define the internal architecture. Standards and reference-browser tests check observable behavior. Choose algorithms, data layout and ownership from measurements in `bare`.

## Text control selection follows Chrome on Linux

Changed on 2026-11-07. A text control (`<input>`, `<textarea>`) never reports the selection direction `none`. The direction `none` is stored and read as `forward`. A control whose caret was never set has the caret at 0, like a textarea. A `value` write that differs puts the caret at the end.

Reason: the HTML Standard lets a platform that has no directionless selection use `forward` ("set the selection range", direction `none`). Chrome 155 on Linux has none. Recorded in headless Chrome 155: `selectionDirection` is `forward` for a fresh input, after `setSelectionRange(1, 3)`, after `select()` and after a blur, with or without focus. Before this change bare answered `none` for a control that did not hold the page selection, and put the caret of a fresh input at the end. Tests that were read from the standard by hand now hold the recorded values.

Consequence: the code that told the owner of the page selection apart (`dir_of`, and the `none` branch of `FocusState::sel_owner`) is gone. `sel_owner` stays for one purpose: Chrome sends `selectionchange` to a text control when focus moves the page selection into it.

## HTTP/2 is our own code, one reader thread for each connection

Decided on 2026-11-07. The client offers `h2` and `http/1.1` in ALPN, and speaks HTTP/2 when the server picks it. The code is `crates/net/src/h2.rs` and `hpack.rs`.

Reason: the profile of `/login` (#186) put 50% of the time in requests that wait for each other and 13% in new connections. One connection with many streams removes both when a server offers `h2`. The measured gain on a local server with a round trip is 11% at 40 ms and 17% at 100 ms ([resources.md](resources.md#http2)).

Own code and not the `h2` crate: the crate needs tokio, adds about 19 crates, a 12 s clean build and +906 KiB stripped (measured on a test crate), and needs a bridge between an async runtime and our blocking TLS stream. Our code adds 78 KiB to the stripped binary. The client is synchronous, so one reader thread for each connection is enough. The Huffman table of HPACK is copied from the `h2` crate (MIT).

Choices:

- The reader waits on the socket with `peek` and takes the TLS stream only for one read. A writer never waits behind a reader that waits for the peer. Lock order is `io`, then `st`.
- The window is 1 MiB for a stream and 4 MiB for the connection. `WINDOW_UPDATE` follows what the caller read. A slow caller stops the server, and memory stays bounded.
- Push is off. A frame is at most 16 KiB. A header list is at most 256 KiB, and a block 256 KiB. An oversized list fails its stream, and the HPACK table still takes the block, so the connection stays in step.
- A stream that the server did not handle (GOAWAY above the last id, REFUSED_STREAM) is marked `Unprocessed` and runs again once on a new connection, for any method. A GET that fails on an old session runs again, as on an old HTTP/1.1 connection. A POST that may have reached the server is never sent twice.
- There is no PING keepalive. A dead session is found when a request fails.
- The connect gate (2 at once) is kept. A request that waited at the gate looks for a session again before it dials. The first dial can still race up to the gate: the extra connection serves its own request, and a later one does not replace a session that is usable.
- The loader keeps its 6 workers and page order. A larger window is a separate change.

Consequence: a server that offers `h2` gets one connection for each origin, not up to 6. `BARE_NET_TRACE` has a `proto` column (`h2` or empty).

## Media elements act as a browser with no media format

Changed on 2026-11-07. `<audio>`, `<video>` and `new Audio(src)` exist (`HTMLMediaElement`, `HTMLAudioElement`, `HTMLVideoElement`, `Audio`, `TimeRanges`, `MediaError`). This engine has no decoder and no audio output, so it never plays. It acts as a browser that supports no format. `canPlayType()` gives `""`. Every source fails with `MEDIA_ERR_SRC_NOT_SUPPORTED` (code 4). `play()` rejects with `NotSupportedError`. `readyState` stays 0 and `duration` stays `NaN`. The events and their order follow Chrome 155 for a source it cannot play (recorded, 77 cases).

Reason: a live React page calls `new Audio(url)` and went down with `ReferenceError: Audio is not defined`. A page must see the same API and handle the same failure as in a browser that cannot play the file. The engine never fakes a result: it does not say `probably`, it does not fire `canplay` and it does not move `currentTime`.

The engine makes no request for the media. Chrome asks the server (a range request) and the events depend on the answer. Here the load fails at once with the text `MEDIA_ELEMENT_ERROR: Format error`, and `progress` and `suspend` never fire for a source that Chrome could fetch. A decoder is needed before a request has a use. The page cannot tell the failure of a fetch from the failure of a format.

Consequences: an element that the parser makes with a `src` does not load until the page calls `load()` or `play()` (the tree builder has no hook). `<source>` children are not read. The globals are lazy accessors (`browser_lazy.rs`), so a page without media pays six accessor functions.

## The heap collects when it has grown by three quarters

Problem: no collection ran inside one driver call. Typing 50 keys into the React form took the process to 214 MiB.

Decision: `Interp::gc_due` is true, at the end of a driver call and in each loader round, when the accounted heap is above `max(8 MiB, 1.75 x the heap left by the last collect)`. The first version used 1.5 (QuickJS and SpiderMonkey use 1.5).

Sweep (`type_load` on `tools/js/typing`, main 3018876 plus the cheaper collect, `instructions:u` of one run, cost of a key from the difference of 50 keys and 0 keys, peak RSS from `getrusage`; the rows with no collect at all cost 410.7 M and 190 MiB at 50 keys):

|   Factor |     Floor | Instructions for each key | Peak RSS, 20 keys | Peak RSS, 50 keys |
| -------: | --------: | ------------------------: | ----------------: | ----------------: |
|     1.50 |     8 MiB |                   417.8 M |          33.1 MiB |          33.1 MiB |
|     1.50 |    12 MiB |                   413.4 M |          36.2 MiB |          36.2 MiB |
| **1.75** | **8 MiB** |               **413.3 M** |      **36.3 MiB** |      **36.1 MiB** |
|     1.75 |    12 MiB |                   413.4 M |          36.1 MiB |          36.3 MiB |
|     2.00 |     8 MiB |                   411.0 M |          39.5 MiB |          41.2 MiB |
|     2.00 |    12 MiB |                   411.1 M |          39.0 MiB |          40.7 MiB |
|     2.50 |     8 MiB |                   408.8 M |          46.3 MiB |          46.6 MiB |
|     2.50 |    12 MiB |                   408.9 M |          47.0 MiB |          46.6 MiB |

An earlier sweep on the same page, before the frame slots, gave the same order with 1.25 to 3.0 and floors of 8 to 24 MiB (floor 16 MiB: 42 to 44 MiB; floor 24 MiB: 56 MiB; factor 1.25 with floor 8 MiB: 455 M against 441 M for 1.5).

Why 1.75 and 8 MiB: the live heap of this page is 5.4 to 6.8 MiB, so the factor sets the limit (9.5 to 11.9 MiB) and the floor does not. Factor 1.75 costs 4.5 M instructions for each key less than 1.5 (-1.1%) for 3 MiB of RSS, and keeps the process under 40 MiB. Factor 2.0 saves 2.3 M more (-0.5%) but the peak goes to 41 MiB. The floor stays at 8 MiB: a page with a small live heap must not collect before it has made 8 MiB of garbage, and no floor between 8 and 12 MiB changes the result on this page. The RSS steps are not smooth because a collect happens at whole keys.

Cost: a key now costs 0.6% more than with no collect (413.3 M against 410.7 M). The first version cost 3.6%. The cheaper collect (a slot table for the reached objects, a cheap hash for the host maps that `trace` reads) halved the cost of each collect: 29.1 M to 13.5 M instructions for each key at the same trigger.

Not collected inside a call: the collect needs depth 0 and no statement frames, so a long synchronous script still keeps its garbage until it returns. The event cap and the loader rounds are its collect points.

## Template parsing follows Chrome 155 where it differs from the standard

The tree builder has the "in template" insertion mode. Chrome 155 differs from the standard in two cases, found with headless Chrome (`tools/fixtures/ui/tplmodes-cases.txt`). The builder follows Chrome, because a page is written against Chrome. No html5lib test covers these cases.

- The standard keeps a template "in template" after `base`, `basefont`, `bgsound`, `noframes` and `title`. Chrome does not: `<template><title>t</title><td>x` drops the `td`. The standard and Chrome agree for `link`, `meta`, `script`, `style` and `template`.
- In a table mode inside a template, the standard ignores a `<form>` start tag. Chrome inserts an empty `form` and does not set the form element pointer.

To follow the standard, add the five names to the first arm of `Builder::in_template` and ignore the form tag in the arm for `form` in `in_body`.

## Custom elements run reactions at the end of each host call

- The native tree watcher cannot run script, so it only queues reactions. `Host::call` runs the queue when the call ends, as `[CEReactions]` does. Reason: one place for all DOM calls, and no change to each DOM function.
- The state of an element is a number in the native document, not a flag on the wrapper. Reason: the wrapper can be collected, and the state must survive that.
- Nothing runs before the first `define`: lazy globals, one `Cell` read per host call, no watcher work. Reason: the cost rule (no row over +0.5%). Result: worst row +0.542% on a row that is noise, all others under +0.45%. See [progress.md](progress.md#custom-elements).
- The page is parsed before its scripts run. A definition upgrades the elements that exist, with a parse-progress emulation for the elements after a blocking script. Reason: an incremental parser is a larger change and no target page needs it. The constructor of a parser-created element sees its children (Chrome: it does not).

## Custom element constructors run at parse time by cutting the tree

- The page is still parsed before any script runs. For a defined element that the parser "reaches" (`ce_release`), the tree is cut: the element loses its attributes, its children and its parent link, and every later node in each ancestor is hidden, with silent pointer operations (`Document::parse_cut`). The constructor runs on that tree, the element goes back with its attributes, the reactions run, and the rest comes back (`parse_close`). Reason: an incremental parser is a large change, and the cut gives a constructor the same view that Chrome gives it. Cost: O(depth) for each custom element, and nothing for a page with none.
- Known limit: a script that holds a wrapper of a hidden node can reach it, and a hidden node that the constructor detaches is not put back.
- The contents of a template belong to a second `Document` node of the same arena (`Ce.tpl`), made on first use. Reason: `adoptedCallback` and `ownerDocument` need two owners, and a second arena would copy every node operation.
- A form-associated element has no stored form owner. A reaction reads it from the tree and compares it with the last value told. Reason: the owner depends on the tree, an id and a `fieldset`, and the watcher cannot read the tree while a change is in progress.

## SVG is a box that paint fills, and a hint hides what is inside

- An `<svg>` is an atomic inline box, and its shapes have no box (`display: none` from a presentational hint, so the layout code does not change). Paint reads the shapes from the tree and draws them as polygons in the box. Reason: layout needs only the size, and a shape box would need a second kind of box with no use outside paint.
- `fill` and `stroke` are properties of the cascade, so a class rule, an inherited value and `currentcolor` work as in Chrome. The other stroke values are read from the attributes. Reason: each property costs about 24,000 instructions at start and 0.4% on a row of styles, so only the two that pages set with CSS were added.
- The hint is built only when the page has an element that is not HTML. Reason: the sheet of the browser names `svg`, so the atom is on every page.
- `compute` starts from the initial values and reads only the properties with a winner. Reason: the three new properties cost more than the rule allows with the old loop. Result: cascade rows -12% to -16%.

## The window keeps a frame until its render key moves

- A window frame (cascade, layout, scroll areas) is made again only when its render key moves. The key holds the tree revision, the text revision, the window size, the sheet generation, and the hover, active, focus and form-state revisions that the rules read. Reason: a full update is simple and correct, and the plan says to measure it before invalidation. Most input (a click with no change, typing, a scroll) moves no part of the key.
- When only the user or form state moved, the window runs the cascade and compares the two style sets (`Styles::change`). Colors and other paint-only values keep the layout. A change of size or display lays out again. Reason: a `:hover` color on a page of 100,000 lines then costs 28 ms instead of about 100 ms. No incremental cascade or dirty region was built.
- The document stores the hovered, pressed and focused elements, not their ancestor chains. A selector walks up from the element. Reason: a stored chain made each focus move cost about 350 instructions (`focus_load` +1.0% to +1.35%). The walk costs the same as a scan of the chain.
- The user action pseudo-classes are one `Simple::User` variant, matched by a `#[cold]` function. Reason: five inline arms in `simple_matches` cost deep-tree rows up to +0.7%, because they changed the code of the hot loop.
- The form state of `Document::set_ui` also moves `revision` (for the selector caches). The render key takes it out of the tree part, and puts it back only when a rule has a form pseudo-class. Reason: a checkbox click or the first letter in a field must not lay out a page with no `:checked` or `:placeholder-shown` rule.

## Storage objects have named properties, and are made on the first read

- `localStorage` and `sessionStorage` are host named objects (`Heap::new_named_open`). `localStorage.token` is `getItem('token')`, `localStorage.token = v` is `setItem`, `delete` is `removeItem`, and `Object.keys`, `in`, `for in` and `JSON.stringify` see the items. Names and values are UTF-16 and keep lone surrogates. Reason: Chrome 155 does this. The headless run of the staging journey on 2026-11-07 showed `Object.keys(sessionStorage)` as `[]` while `length` was 1.
- A name does not hide the prototype chain (Chrome `Storage` has no `[LegacyOverrideBuiltIns]`). `localStorage.getItem`, `length` and the other methods stay the methods. A write to such a name makes an ordinary property. The engine has a second kind of named object for this: `NamedSource::named_masking` answers `false` for it, and `preventExtensions`, `seal` and `freeze` of it fail. The dataset map is the first kind, and its names hide the chain.
- Chrome is the reference. `tools/fixtures/ui/storage-cases.txt` has 45 cases and `chrome-storage.txt` has the answers of headless Chrome 155. A case that needs an origin starts with `@url https://a.test/`. The Chrome helper (`tools/ui/chrome_cdp.mjs`) answers that URL through CDP `Fetch`, so there is no network and the origin is real.
- The old test `storage_keeps_utf16_and_is_not_a_property_bag` expected `localStorage.token = 'p'` to be dropped (read back as empty, `getItem` gave `null`, `Object.keys` was empty) and a `TypeError` for that write in strict mode. Chrome stores the item, so the test conflicted with the target. It is now `storage_keeps_utf16_and_has_named_properties`. It expects `p,3,p,3` (the property, `length`, `getItem` and the number of keys) and a `TypeError` for `localStorage.length = 1` in strict mode (the accessor has no setter).
- Each object is made on the first read of `window.localStorage` or `window.sessionStorage`. Both are accessors of `window`, as in Chrome. Reason: a named object is an entry in `Heap::exotics`, and then every property write and every step of the mark loop takes the exotic path. The first version made both objects at start. Browser rows rose by 1% to 2% (`range_load`, `session_load`, `observer_load`) although the heap had only 2 more objects. With lazy objects those rows are unchanged. A page that reads storage pays the same as a page that uses `dataset`.
- Not done, and different from Chrome: the order of names is the insertion order (Chrome uses a hash table, and the Standard leaves it open); `Object.keys` and `for in` do not list a name that the chain hides (Chrome lists it, except an accessor like `length`); there is no `storage` event.

## Cookies: one jar for the network and for scripts

- Since 2026-11-09 one jar serves HTTP and `document.cookie`: `bare_net::cookie::Jar` (`crates/net/src/cookie.rs`). It replaces the jar for scripts only that `crates/bare/src/browser_cookie.rs` had (2026-11-07: three crates and a public suffix list were too much for one gap of the staging journey). The rules of that jar are kept, and Chrome 155 checks them (`tools/fixtures/ui/cookie-cases.txt`, 19 cases, all still match).
- The jar lives in `Client` (`Arc<Mutex<Jar>>`, shared by forks) and in `WebStorage` (the same `Arc`). A session makes its client use the jar of its storage, so a new session with the old storage keeps the cookies, and a new `open` of the driver starts with none. A worker job runs with the jar and the site of the client that spawned it (a worker is shared by all forks and it was made for the first one).
- `Client::start` is the one place that adds `Cookie` and reads `Set-Cookie`: every path (`get`, `send`, `send_once`, the streams, HTTP/1.1 and HTTP/2, each redirect hop) goes through it. A host with no cookie costs one lock and one hash lookup. A `Cookie` header of the caller stops the jar from adding one.
- `Request.cookies` says what a request does: `on` (credentials) and the context. `Embedded` is a subresource, `fetch` or XHR of the document of the client (`Client::set_site`); `Navigation(initiator)` is a top-level load.
- Chrome 155 and RFC 6265bis, as far as the jar goes: the site is scheme and registrable domain; `SameSite` unset acts as `Lax`; a cross-site subresource gets and sets only `SameSite=None; Secure` cookies; a cross-site top-level GET gets `Lax` too; a chain that leaves the site of the initiator stays cross-site. Loopback hosts count as secure contexts (`Secure` cookies work on `http://localhost`).
- Eviction: by last use (a `Cookie` header or a `document.cookie` read), not by creation. Limits: 4096 bytes for name and value, 180 cookies for a domain (cut to 150), 3000 in all. RFC 6265bis asks for at least 50 for a domain; Chrome's 180 is kept because `document.cookie` was already checked against it.
- Not done: the full Public Suffix List (a short embedded list of one-label names and about 80 common two-label suffixes), `Partitioned` (ignored), `Priority`, persistence on disk, schemeless same-site (a scheme change is cross-site), the Chrome rule that gives a `POST` Lax cookies for 2 minutes, `cookieStore`, and `Headers.getSetCookie` (still empty).

## S5 control interface

- The control interface is a Rust API in the engine (`bare::control`): `Page` methods for navigation, queries, actions, time and pixels. The test driver is a separate crate (`crates/drive`, binary `bare-drive`) that uses only that API. Reason: an in-process call costs nothing to set up. A socket needs a protocol, a server loop, a serializer and a second process, and the S5 checks need none of them. A driver call to a window costs 29 us (round trip through the window loop). A click step costs 3 to 81 us more than the same events sent straight to the page.
- Headless, the driver owns the `Page` on its own thread. With `--window`, the page and its Wayland window run on another thread (`control::Remote`). Each call is a job that the window loop runs in `App::tick`, between input events, and a frame follows it. Reason: one code path for both modes, and the window keeps its own loop. `WebStorage` is not `Send`, so a job opens a new session of the same profile on the window thread.
- An action makes `bare_window::Event`s and gives them to `Page::handle`, as the Wayland loop does. A click hit-tests the window frame. Reason: S5 must use the input path of the window. No action writes the DOM, a value, the focus or the storage. A `js` check exists for assertions only.
- Before an action, the target must be connected, rendered, enabled and on top at the center of its visible part, after a scroll into view if needed. Else the action is refused and nothing is sent. Reason: these are the checks of Playwright. A user cannot click an element that another element covers.
- Timers run between input events: `App::tick` gives the time of the next task, and the window loop sleeps in `poll` until then at the latest. Reason: S4 topic 6 left this gap, and a real journey needs it (a page loads its data after a timer).
- A wait runs the due tasks and sleeps until the next one. When no task is queued, nothing can change the page, so the step fails at once. Reason: no fixed sleeps, and a failure needs no full time limit. A reply in flight counts as a queued task.
- Locators follow Playwright: role and accessible name, label, text, placeholder, CSS. A locator must match one element. Reason: a duplicate target is a test bug. A first-match rule hides it.
- The output is one JSON object. Secrets are replaced in every text when the JSON is written, so a secret that the driver learns late is also hidden in earlier records. A capture is withheld when a secret is in the text or in a field of the page. Reason: a password field shows dots, but an email field and the page text show their text in the pixels.
- Network records come from `bare_net::trace` (`enable`, `take`). The list is one for each process, so one driver runs at a time in a process. Reason: it holds the URL without the query, the status and the times already. A second recorder would cost a second hook in the hot path.

## Incremental frames: damage, restyle, layout reuse, culled paint

Goal: after a small change, the window frame costs in proportion to the change, not to the page. Measured before (100,000 lines, p50): a script text write 77 ms, a hover color 22 ms, a checkbox click 16 ms, a click that changes nothing 5.2 ms. Target: under 5 ms for the first three.

What the other engines do:

- Chrome (Blink, LayoutNG). Style: invalidation sets select the elements to restyle ("self", descendant and sibling sets). `:hover` changes recalculate only elements that have an `AffectedBy*` flag (source: `style-invalidation.md` in Blink). Layout: a change sets `NeedsLayout` on the box and on its containing blocks. `BlockNode::Layout` keeps the last layout result with its input constraint space, and reuses it when the space is identical (`operator==`) and the box is not marked for layout (source: `layout_ng.md`). The kept fragment is cloned without its offset. Paint: `PaintController` keeps display items for each client and repaints only invalidated clients (to verify: we did not read this source). Hit testing skips a subtree whose overflow rect does not hold the point (to verify).
- WebKit. `markContainingBlocksForLayout` sets the dirty bits up the containing blocks. It stops at a layout boundary (`isLayoutBoundary`): the view, a box with layout and size containment, or a box that clips its overflow with a fixed width and height and is not a table part. Layout then starts at that root (source: `RenderObject.cpp`).
- Servo. Restyle computes `RestyleDamage` from the old and new styles. A repaint-only change skips layout and rebuilds only the stacking and display lists (servo#36978). Layout results are kept on each box (`CacheableLayoutResult` in `LayoutBoxBase`, servo#36082). Undamaged block boxes and their fragments are reused (servo#37751, servo#42687 and later).

The common idea is the same in all three: mark the changed boxes and their ancestors, keep the old result of each clean box with its inputs, and use it again when the inputs are equal. A layout boundary (WebKit) is an early stop of the same walk. The LayoutNG cache is the most general: it needs no special box, and the walk from the root costs only the work of the changed path and one copy of the clean subtrees. Our fragments are one flat list, so a reused subtree is one `memcpy`. We take the LayoutNG design.

Design:

1. Damage log (`bare_html::dom`). The window turns on a log of tree changes. An entry names a node: a link, an unlink, an attribute, a text, or a form state change. The log and the watcher of `set_watcher` share one box (`Hooks`), so `Document` has no new field and a change still checks one `Option`. A new field cost up to +0.27% on the parse and layout rows. A change that the log cannot describe (`touch`, `set_custom_state`, `set_states`, `attach_shadow`, the parser cut) marks the log "full". A full log, or a log with more than 4,096 entries (`MAX_MARKS`), makes the next frame a full update. A change to a `style`, `link` or `base` element, a rule set built again, or a page with a shadow root also gives a full update.
2. Incremental restyle (`bare_css::cascade`). The style cache of a cascade (`Memo`) and its value tables stay in `Styles` for the window. So the same (parent style, rules) gives the same style number, and two equal numbers mean equal values. `RuleSet` finds, for each user state (hover, active, focus, form), the widest scope that a selector reads it in: the element itself (only the subject compound), its subtree (a compound to the left, through descendant or child combinators), or its siblings (a `+` or `~` on the way). The window restyles these roots: the elements whose hover, active or focus state changed (the symmetric difference of the old and new ancestor chains), the subtree of a linked node, an element with a changed attribute (its subtree), and, when the rules use sibling combinators or structural pseudo-classes, the changed element and its later siblings, each with its subtree (`Scopes::at`). A form state change gives a full update when a rule reads a form pseudo-class. A root in the "self" scope restyles only itself, and its subtree only if its style number changed. The walk starts with the ancestor filter filled from the ancestors of the root. The restyle gives the elements whose style number changed.
3. Paint or layout. For each changed element, `Change::paint_only` decides. Paint-only changes keep the layout. Any other change marks the element dirty. Dirty bits go up through the flat parents to the root. A text change marks the text node. A link marks the node and its parent. An unlink marks the parent. An attribute change marks the element. A form state change marks nothing: only selectors read it, so a style that changed marks the element.
4. Layout reuse (`bare_layout`). A layout for the window keeps, for each "stable" block, its inputs and its result: the available width and height, the depth, the list number before and after, the `Done` of `block::sized`, and the counters and the effects (scrollbars, relative offsets) of its subtree. A stable block is the root, or a block child of a stable block in normal block flow. A block in a flex, grid or table container, an inline-block, a float or a positioned box is not stable: its parent lays it out with other inputs. The layout also keeps the fragments as they are before `absolute` (places relative to the parent). In the next layout, `block::children` reuses a child block when all of these are true: the child and its subtree are not dirty, its kept inputs are equal, no float is open in the formatting context before and after it, no list marker waits for a line, no scrollbar pass of an ancestor runs again, and no static position of an absolute box leaves its subtree. Then it copies the kept relative fragments in one `memcpy`, moves their `end` indexes, and gives the kept `Done`. The parent places it as usual. The old fragments of the children are found in order with a cursor over the old children of the parent, so an inserted or removed child does not break the match of the others.
5. Containment. This design needs no separate boundary. A dirty block lays out again. Its clean children are copied, and its parent places them again. A change stops where a block gives the same `Done`: its siblings and ancestors are copied or placed with the same values. The cost of a change is the dirty path, the clean siblings along it (a copy), and the passes over all fragments that remain (`drop_statics`, `absolute`, the scroll areas, the extents). These passes are simple loops over a flat array.
6. Extents. After each layout, the window computes an extent for each fragment: the union of the border boxes of the fragments inside it, with room for glyphs around text. A fixed box and a sticky box have an infinite extent (`FREE`), so their ancestors are never skipped. The extents are made on the first band list or hit test after a layout.
7. Culled display list. The display list holds the page band around the window: the window and one window size on each side. The builder skips a fragment whose extent is outside the band. Inside a box that clips, a fixed box or a sticky box, it skips nothing. The controls and text fields of the frame are found in the same band (`each_in_band`), not in a pass over all fragments. A page with fewer than 4,096 fragments has one list for the whole page: it is cheap, and a scroll never builds it again. When a scroll moves the window out of the band, the next frame builds the list again.
8. Hit test. The hit test skips a block whose extent does not hold the point. The window keeps the last hit (point, scroll offsets, frame epoch). Each frame update moves the epoch, paint-only updates too. A click (move, press, release at one point) then runs one hit test, unless the frame changed between the events.
9. Check. With `BARE_CHECK_FRAMES=1` and in the page tests, each frame is compared with a full cascade, layout and paint from nothing (`Page::check_frame`): the pixels, each fragment and the page bars. The property test `random_changes_draw_the_frame_made_from_nothing` runs 400 random changes (text, attributes, links, unlinks, hover, focus, checkboxes, scrolls) on a page with floats, positioned, sticky and fixed boxes, a scroll box and lists. `a_restyle_of_the_picked_roots_equals_a_full_cascade` does the same for the restyle alone.
10. Preflight walk. Before each layout, a walk of the visible tree counts the fragments and refuses a page over the limits (`browser_geometry.rs`). A window layout keeps the count of each subtree with 64 nodes or more. The next window layout skips each clean subtree and uses its count. A count is valid only for the next window walk.
11. Memory. A frame update writes the new fragments in the memory of the old frame (`Kept::recycle`), and a full update does the same. The kept fragments before `absolute` are a second copy of the fragment list. At 100,000 lines the RSS after the first paint grows by 6.3 MiB (glibc) to 12.6 MiB (jemalloc).
12. Shared styles. The cascade cache shares the frame styles (`Rc<Styles>`). The restyle drops the cache entry, which is stale after a change, and then changes the styles in place. When another holder still shares them, the frame gets a full update.

What is not incremental: a resize (the viewport is an input of each block, so all blocks lay out again), a change of a style sheet, a page with a shadow root, a form state change when a rule reads a form pseudo-class, a float above the change in the same formatting context, and the boxes inside flex, grid and table containers (their container lays them out again; the existing `memo.rs` helps inside one layout).

## Input that does not wait

An input event ran the page until it settled (`Session::resume(&[])`): all replies, prefetches and the next document. Each key and each click blocked the window until the page was quiet. On staging a login click took 1.8 to 2.3 s. A browser runs the listeners and the due tasks, then goes back to its event loop. The choices:

- A live session (`SessionConfig::live`) for the window and the control interface (`Page::open_with` sets it). `resume(&[])` is then one turn of an event loop. The headless `Session::run` and `resume` with inputs keep the settle rule when `live` is off, so tests and examples stay deterministic.
- In a live turn, `fetch` and XHR start every queued request on a worker at once (also one request alone, and on a fast link). A request that is not a GET or a HEAD starts when all requests before it are done. A turn settles only the replies that are in, in the order of the queue. `Task::done` reads the slot of a worker job without a wait.
- After `load`, a dynamic script that is not downloaded waits in the queue for a later turn. Its download and those of the scripts after it start at once. The scripts run in the order of the queue, as before. The page downloads stay in the document (`Scripts::net`) from one turn to the next.
- After `load`, a turn stops after a slice of 8 ms of work and never sleeps. The window loop reads the input events and draws a frame between two turns. A single task that runs longer than the slice is not cut.
- A navigation that the page asks for (a link, a form submit, `location`, a reload from script, a traversal to another document) downloads the next document on a worker. The old document stays on screen and runs nothing until the new one is in. Then a turn parses it and runs it. The old `Browser` is still dropped before the new one is made, so a heap is never held twice. `Page::navigate` (the address bar, the driver `goto` and `reload`) and `Page::open` also return at once (2026-11-08). They end the old document, and later turns load the next one. Until it is in, the page has no document: a locator or an `expect` waits, and the window shows the last frame of the old page.
- `Page::tick` asks to be called again at once when work is left, and in 4 ms while a reply or a next document is on its way. The window and the control interface call `Page::wake_on_done`: a worker that ends a job wakes the loop (`bare_net::parallel::on_end`), and the tick then asks for a call each second only, as a backstop. A window with a request in flight sleeps in `poll` and uses no CPU. A page with no queued task and nothing in flight still gives `None`: no wake, no CPU.
- A turn does not collect the heap at its end. The rounds collect when the heap asks (`gc_due`). A collect after each turn would cost more than the turn.
- After `load`, a style sheet, an `@import` of it or a preload that a script adds starts on a worker (`Run::on_the_way`). The turn does not wait for it. A later turn supplies the sheet, or takes the preload, and fires its `load` or `error` when the reply is in. Until then a read sees the page without the sheet, as in Chrome.
- A document that a page navigation loaded in the background is split (`Scripts::split`). Its classic scripts run across turns: a turn stops at the end of the 8 ms slice, or before a parsed script that is not downloaded. All its downloads start at once. The page takes input between the turns. The order of the scripts, `DOMContentLoaded` and `load` does not change. `load` also waits for the sheets and preloads that are on their way.
- A live `Session::run` and `Session::navigate` download the document on a worker and return at once (`Session::start`). A headless session (`live` off) keeps the settle rule. `Page::wait_load` runs the ticks until `load` for a caller that needs a loaded page.
- A live session parses a document that a page navigation loaded in slices of 8 ms (`Parsing`, `tree::Parser::feed`, 16 KB at a time). The old document stays on screen and takes input until the tree is done. The loader keeps its pre-parsed model: no script runs before the whole tree is built, so the tree and the order of the scripts and events are those of `tree::parse`. A scan of the text ahead of the parser (`browser_loader/scan.rs`) starts the downloads of the classic scripts and the style sheets, and `run_page` takes them.
- Every live document is split. A sheet or a preload that a script adds before `load` starts on a worker and does not stop the scripts after it.

## Open questions

- A separate process for sensitive sites, for example banking.
- When to replace the bitmap font with TrueType.

## Cascade reuse for geometry reads

A geometry read (`getBoundingClientRect`, `getClientRects`, `offsetWidth`, an IntersectionObserver frame) ran a full cascade of the document each time. On the staging login this was 150 reads and 320 to 450 ms. The numbers are in [resources.md](resources.md#page-load-on-live-staging). The choices:

- Keep the last cascade (`Built::last`, an `Rc<Styles>`) with the rule set that made it. A new rule set drops it.
- Key: `revision`, `text_revision`, the node count, `UserState::revision` (hover, active, focus) and `want`. These move at every change that the selectors read: tree, attributes, text, `set_ui`, `set_states` and the user state. `attach_shadow` moves no counter, but it adds a node, so the node count moves.
- A custom element state change moves no counter. A document with custom element state does not keep or use the cascade. Moving `revision` in `set_custom_state` was rejected: it would also drop the query caches at each state change.
- The charges and caps of a read are the same with or without reuse, so a page hits the same limits.
- The layout is not kept: the cascade was 95% of the geometry time.
- `Styles` is shared as `Rc`, not cloned: a clone costs on each miss.


## Image decoders

`<img>` needs PNG, JPEG and WebP decoders. All three are pure Rust and live in the new crate `bare-image`.

- `png` 0.18. Chosen for the PNG format: it is the reference Rust decoder. It reads the header without the pixel data.
- `zune-jpeg` 0.5. Chosen over `jpeg-decoder`: it is faster and has no dependency beyond `zune-core`. It decodes to RGBA directly.
- `image-webp` 0.2. The only maintained pure-Rust WebP decoder. It has two small dependencies.
- No GIF. A GIF needs frames and timers. It is out of scope until a page needs it.
- SVG files reuse the inline `<svg>` painter. They are painted on white and on black, and the alpha comes from the difference. No new SVG engine.
- Limits fail closed: 32 MiB of file bytes, 16 M pixels (64 MiB decoded) for each picture, 64 MiB decoded for each page, 512 pictures for each page, 4 downloads at a time.
- The pixel limit is lower than the 64 MP that was first suggested. 64 MP would allow 256 MiB for one picture.
- Cost: `bare-drive` grows from 12.8 MiB to 13.5 MiB (release, whole feature, not only the decoders). The build time was not measured on its own. The incremental release build of `bare-js` examples and `bare` took 1 m 55 s.

## CSS background-image and mask-image

Changed on 2026-11-10. This replaces PR #278 (PR #325). Wikipedia draws its icons with these two properties, and the black squares are gone.

- What is drawn: `background-image: url(...)` and `mask-image: url(...)` (and `-webkit-mask-image`), with `background-size`, `background-position`, `background-repeat` and their `mask-*` names. A mask shows the color of the box through the alpha of the picture. Raster pictures and SVG files both work. A picture loads through the same queue and limits as `<img>`.
- Gaps: gradients and `image-set()` read as `none` and are not drawn. Only the first layer of a list is used. Edge-offset positions, `space` and `round` are not supported. A vector picture is drawn at its natural size and then scaled as a raster, so it is soft when it is made larger. A mask covers the background color only. Rounded corners are ignored. List each gap again in resources.md when a target page needs it.
- The picture properties are not in the 94 core slots of `ComputedStyle`. A style holds one `u32`: the number of its set of 12 picture values, from one interned table. Set 0 is the starting values. Reason: the old branch kept 12 more values in every style, and 16 cost rows went over +0.5%. Nearly every element has set 0, so it pays one store.
- The cascade reads a picture declaration only for an element that has one. `Entry::pics` counts them, and they sit after the other declarations of the rule. `cascade_as` sets a flag, and `style_of` calls the cold `picture_pass`. Reason: a call inside `cascade_as` cost 0.7% on the layout stress rows (the style and the key closure stayed alive, and `resolve` lost its inlining).
- A sheet gets its `url(...)` addresses made absolute when it is loaded, because the sheets are joined and the base address is lost. The address of an `@import` stays as it was: the loader joins it.
- The cost rule did not change: no row over +0.5% under both allocators. A flip in the inlining of unrelated code counts as cost and must be fixed in the code that changed (`dataset_load keys`, see progress.md).

## A script budget ends only its task

A runaway script ends the task, not the page. Chrome does the same: it shows "page unresponsive" and the DOM stays.

- The class is the JS execution budget: `Limit` of `Steps`, `CallDepth`, `TraversalDepth`, `StatementFrames` and `DrainJobs` (`Browser::survive`, `browser.rs`). The step budget was already new for each task (`Interp::begin`), so a task cannot use what the last one left.
- `survive` logs one console error (level `error`, the budget and the kind of task), clears the sticky stop states that the budget left (the failed job of the microtask queue, and the halts of the scheduler, navigation, viewport and network pumps) and lets the page go on. A timer that ran away is not armed again; the jobs behind a failed job still run.
- The page has a budget of 5 aborts (`BrowserLimits::max_task_aborts`). The next one is `Fatal::Aborted`: the page stops, `Page::stopped` gives the reason, and `bare-drive` adds "(the page stopped: ...)" to the failure of a step that waits. This bounds a chain of runaway timers (5 tasks of 100 M steps are about 9 s).
- `Limit(Host("dom-selector"))` ends only its task too (PR #314). It refuses one uncached selector longer than 64 KiB (1,024 bytes before; see "The same script text keeps one slot, and a selector may be 64 KiB"). The input is one string of one call, so the fault belongs to that task. A page that builds such a selector (GitHub) must go on. It counts in the same 5 aborts (`is_budget`, `browser.rs`). `dom-selector-complexity` is not in the class.
- Everything else stays fatal and sticky: the other `Limit(Host(..))` (refusals that a retry repeats), heap and memory caps (they can come from garbage that stays), `Unsupported`, a module script. Reason: these are gates or caps of the host, not the time of one script. A cap that a real page reached is fixed by a better measure, not by a new class (next section).
- State after an abort. Each native DOM operation is complete when a budget is checked, so the tree is always valid. A host call that clears and refills children (`textContent=`, `replaceChildren`) charged its steps in the middle of the change, which left half of the children; it now charges first. Other multi-step host calls were not audited one by one (`innerHTML=` parses to a separate fragment first).
- Not done: `RangeError` for a deep recursion. The engine still ends a deep call with the uncatchable `CallDepth`; it now aborts the task instead of the page.

## Host caps measure what the engine does

Two caps stopped pages that Chrome loads. Neither changed class: a cap that a real page hits is wrong, and the fix is the measure or the value.

- `computed-work` bounded nodes x (rules + 1). apnews has 30,000 nodes, of which 5,000 are elements, and 8,274 rules: 252 million, over the cap of 134 million. The indexed cascade checks 1.3 million candidates and takes 24 ms. The cap now falls back to styled elements x (rules + 1) when the first bound is over (`styled_work`, cold path). Text nodes are not styled. The cheap bound stays the first test, so small pages pay no new work. The bound is still not the real count of candidates: that count needs the rule index in `bare_css`. It is the next step.
- The step budget was charged with the same bound on every read (`it.charge(work)`), also on a read that only restyles. The next section replaces this.
- `fetch-queue` held 64 requests. gitlab calls `fetch` 64 times in one task, before one can finish: no leak and no count bug (finished requests leave the queue in order). Chrome queues them, so the queue holds 1,024 requests and 8 MiB of URL and header text (`max_pending_bytes`; bodies stay under `max_retained`). The text is summed only once the queue holds 64 requests. Over either bound the page stops, as before: this is a bound against a hostile page, not a case that a real page reaches.

## Unhandled promise rejections

A promise that is rejected with no handler is reported after the microtask checkpoint, as the HTML standard says ("HostPromiseRejectionTracker", "notify about rejected promises").

- The tracker works at reject time. `settle` puts a promise that has no handler on a pending list, in a cold path. Nothing scans all promises: that is O(all promises) for each checkpoint.
- Without a host, nothing is tracked. Nobody can hear it, and the list must not keep promises alive.
- `drain_jobs` ends with one notification when the pending list or the reported list is not empty. An empty checkpoint pays two length checks.
- The host hook is `Host::promise_rejections`. The browser fires a cancelable `unhandledrejection` at `window` for each promise that is still unhandled. When it is not canceled, the console gets one error: `Uncaught (in promise) <reason>`. For an error object the reason is `name: message` and the first frame of `stack`.
- A reported promise that gets a handler later fires `rejectionhandled`. A `then` and an `await` both count. The reported list is checked at each checkpoint, so `eval_async.rs` needs no hook.
- One round for each checkpoint, as the standard's queued task does. A rejection inside a listener waits for the next checkpoint, so a listener that rejects again cannot loop.
- Bounds: 1024 pending promises (`MAX_PENDING_REJECTIONS`); more are counted and logged as one line. 256 reported promises (`MAX_REPORTED_REJECTIONS`); over it, the oldest is forgotten and cannot fire `rejectionhandled`.
- GC: the pending list is a root. The reported list is weak (the standard's "outstanding rejected promises weak set"): `collect` drops the unreachable ones.
- The listeners run as one host call with the budget of the checkpoint. No new step budget starts.
- Cost: no new field in `PromiseData`. The full matrix has zero rows over +0.5%.

## The cascade charges steps for the elements it styles

The step budget is 100 M steps for each task. A host call must charge about the work that it does. Five live pages ended tasks with `Steps` because the cascade charged an upper bound on every read: dev.to, kubernetes docs, nasa, redis and hsbc. In all five, `cascade_with` was more than 97% of the charged steps.

- A read that finds the last styles right charges 1 step, as before.
- A read after changes restyles the elements that the changes reach (`RuleSet::restyle`) and charges for those: each root styled with its subtree, and the subtree of a root whose style changed. A root whose style did not change costs one element. It charges `elements x (rules / 4 + 1)`. Main charged nodes x (rules + 1) first, and then charged the changed elements on top. dev.to: 84 M steps for each read, and a restyle of a few nodes paid for the whole page.
- A full compute (the first read, new rules, or a restyle that cannot run) charges `styled elements x (rules / 4 + 1)` before it runs. Small pages (bound under 2^20) use nodes. The `computed-work` cap check is unchanged and still comes first. It bounds the time and the memory, and the charge does not replace it.
- The divisor is a measurement, not a rule of the cascade (`UNITS_PER_STEP`, `browser_computed.rs`). One step of a JS loop costs 273 instructions (`for (let i...) s += i`, 4 steps for each iteration, the loop hit the budget at 27.3 G instructions). A full cascade of dev.to (2,018 elements, 16,198 rules) costs 125 M instructions (`perf stat -e instructions:u`, runs with 40 extra computes minus runs with none, divided by 40, two pairs: 121 M and 128 M). It charges 8.2 M steps now, so 15 instructions for each step: the charge is about 18 times the real work. Typical cost is 2 to 4 instructions for each element x rule. Before it charged 84 M steps, 1.5 instructions for each step.
- With divisor 1 the same fix still left kubernetes and redis over the budget. 2 and 4 pass all five pages. 4 keeps a margin of 2 or more.
- Limit: `bare_css` has no count of the candidates that the rule index gives for an element, so this is a model of the work. A page whose rules all match all elements (`*:not(.zN)`, 2,000 elements x 2,000 rules) costs 334 instructions for each element x rule (1.34 G for one cascade, 135 ms). Charged 1 M steps, it is 5 times under the JS rate. The time of one such cascade is bounded by `computed-work` (4.5 s at the cap); one task can run several. A real count of candidates, charged as steps, is the next step. It needs a counter in `crates/css`.

The cascade also charges its real candidates (2026-11-10). The model above is paid before the cascade runs. `RuleSet::tested_candidates` counts the rules that the index gave for each styled element (one add for each element; a counter in the `walk` loop itself cost +1.2% on `bare-stress boxes-display-none`, a `Cell` in `RuleSet::candidates` costs nothing measurable). After a full compute the task pays the candidates over the model, one step each; a restyle pays the larger of the two. Rules that match every element (`*:not(.zN)`, 60,000 elements x 1,500 rules, 2 s for one cascade) ran 4 cascades in one task (8 s) after #326 and run 1 now (before #326: 1). Pages with indexed rules never reach the model, so the charge of dev.to, kubernetes, nasa, redis and hsbc is the same.

## `Worker` fails to start instead of being missing

`new Worker(url)` threw `ReferenceError`, which ended the script that called it. openstreetmap never defined `L.OSM.Map` (MapLibre makes its worker from a Blob) and svelte.dev logged `Worker is not defined`. A real worker needs a second realm and a structured clone between two heaps. bare has neither.

- `Worker` exists, and every worker fails to start. A task after the constructor fires a simple `error` event at the object. This is what a browser does when the worker script cannot load. Pages handle that case: they use their fallback, or they log and go on. A missing global is not a case that pages handle, so the whole script ended.
- A page that waits for a message from the worker waits for ever. This is the same as for a worker that never answers. The page does not stop.
- `postMessage` sends nothing. It still throws `DataCloneError` for a function or a symbol, as `MessagePort` does. It does not look inside objects.
- Bound: 64 live worker objects for each page (`MAX_WORKERS`). The next one throws `NotSupportedError`. A worker that was collected frees its place. The bound is against a page that makes workers in a loop; a map library makes one.
- One console warning for each page names the first worker URL: `Worker scripts do not run in bare (<url>)`. The failure is visible, and it does not repeat.
- `SharedWorker` and `ServiceWorker` are unchanged (absent).

## What holds the memory at the peak of a page load

Problem: public pages reach 200 to 300 MiB of peak RSS (vimeo 260, redis.io 125 to 330, bsky.app 198, ikea 145 to 225), while the heap at the end of the run is 10 to 30 MiB. The first guess was memory that worker threads take for a short time (the four 64 MiB glibc arena heaps of vimeo). It was wrong.

Method: a temporary counting allocator in `bare-drive` (not committed). It sampled one allocation for each 64 KiB allocated, with a backtrace, and kept the live samples at the highest live byte count. It also counted live bytes for each thread. Page faults by first touch (`perf record -e page-faults`) gave the same thread split.

What it showed (vimeo, one run, 287 MiB live at the peak, 246 MiB sampled):

| Holder | MiB | Share |
| --- | ---: | ---: |
| Page thread (the drive thread): JS object elements (`Vec<Option<Value>>`), the object table, interned strings | 167 | 68% |
| Parsed scripts (`Program::into_owned`: tree arenas and the owned source) | 63 | 26% |
| Script slots (`add_script_kind`) | 16 | 6% |
| Worker threads (all) | under 1 | 0% |

- The "worker arena" regions are the heap of the thread that runs the page. In `bare-drive` that is a spawned thread, not the main thread, so glibc gives it a non-main arena. The workers share it (`M_ARENA_MAX` 2).
- The memory is live. One collection ran, at the end of the load: 200 MiB before and 143 MiB after on vimeo. The heap collects only between tasks (see [the heap collects when it has grown by three quarters](#the-heap-collects-when-it-has-grown-by-three-quarters)), so a long script keeps its garbage until it returns. `heap_kib` is read after the page is dropped, which is why it shows 30 MiB.
- Other pages: redis.io has 41% parsed scripts, 38% other JS tables, 11% script slots and 7% client cache bodies (`Client::keep` on a worker, 7.5 MiB of the 64 MiB `MAX_CACHE`). bsky.app has 45% parsed scripts (81 of 182 MiB), 31% other JS tables, 14% script slots and 5% cache bodies. Threads other than the page thread hold 9 MiB on redis.io and 19 MiB on bsky.app.
- ikea holds 70 of 178 MiB on workers: picture decode. One cause was fixed (below). The rest is the decoded pictures, 4 bytes for each pixel, at the size of the file and not at the size of the box (31% of the peak). `BUDGET` bounds it at 64 MiB.
- twitch was not attributed. It stopped at `Limit(Scripts)` in 3 runs (51 MiB, 750 ms) and reached 195 to 199 MiB in 9 later runs of the same binary.

Decision (fixed): `bare_image::decode` made three copies of each picture at the peak: the decoder output (3 or 4 bytes for each pixel), a `Vec<u32>` of pixels, and the `Arc<[u32]>` that the document holds (`Vec` to `Arc` copies). That is 8 bytes for each pixel for a moment, 4 at the end. `decode` now makes the `Arc<[u32]>` first, the decoder writes into the start of it, and the conversion to `0xAARRGGBB` runs in place from the last pixel to the first. The peak of one decode is the pixels once. Test: a PNG of 1500 by 1000 held 12.28 MB for 6.00 MB of pixels before (2.05 times) and under 1.25 times after.

Not fixed, and why:

- The JS heap and the parsed scripts are 80% of the peak on the three big pages. The files that change them are in `crates/js/src`, which this task did not touch. Candidates, by size: collect inside a long script at a safe point; hold object elements in 8 bytes, not 16 (`Option<Value>`); keep the tree of a script that ran once and is not a function body only until it has run.
- Pictures at the size of the box: the box size is known only after layout, and a smaller bitmap changes the pixels of a screenshot. It needs its own design.
- `Client::keep` copies the body of each cached reply while the caller holds its own. Moving the buffer in would remove the copy for a page that reads each file once; it changes `bare-net` for 5 to 7% of the peak.

AST size (bsky.app bundle, 9,731,470 bytes of source, `parse_check stats`): 2,927,810 nodes and 940,355 list items, tree 74,813,948 bytes. That is 7.69 bytes of tree for each byte of source, 25.6 bytes for each node and 19.3 for each node or list item. The parse takes 391 ms. The node sizes are at most 24 bytes (`Expr`, `Pat`), 32 (`Stmt`) and 44 (`Func`), and a list item is 4 bytes.

Worst case of `EvalLimits::max_script_nodes` (8 Mi nodes and list items): 8,388,608 times 44 bytes is 352 MiB if every item were a `Func`. A tree with the mix of bsky.app has 19.3 bytes for each item, so 154 MiB. The first number is over 256 MiB, and it is a bound for one script. `max_scripts` (4096) and `max_script_source` (64 MiB for each script) put no bound on the sum of the trees of a page. Proposal (not done here): count `Stats::bytes`, which `Program::stats` already computes, and refuse a script past 192 MiB of tree (about 2.5 times the bsky.app bundle), and keep one sum of tree bytes for a page with the same limit. Each parser arena doubles while it grows, so the high-water mark of a parse is up to twice the final size before `into_owned` shrinks it. That is part of the same bound.

## `currentScript` lives through the microtask checkpoint, and `ImageData` gives zeros

Problem: tailwindcss.com, hashnode.com and nodejs.org log `Invariant: Expected document.currentScript to be a <script> element. Received null instead.` Next.js reads `document.currentScript` in a promise job of a classic script. `Loader::run_one` cleared `currentScript` when the script ended, and the checkpoint ran later. openstreetmap.org logs `ImageData is not defined`.

`currentScript` (HTML Standard, "execute the script element", then "clean up after running script"): the first sets the `currentScript` of the document for a classic script. The second runs a microtask checkpoint when the JavaScript execution context stack is empty. The step that sets `currentScript` back comes after that checkpoint. So a promise job of a classic script sees the element. Decision: `run_one` runs the script, then the dynamic inline scripts that it queued, then the checkpoint with `currentScript` set to the script, then clears it. A script that throws gets the same checkpoint. A module script never sets `currentScript` (the spec sets it for classic scripts only). A timer, an event task and any other task see `null`.

The order of tasks does not change. A dynamic inline script still runs right after the script that inserted it, before the checkpoint. A browser runs it inside `appendChild`, so its promise jobs run at the checkpoint of the outer script. The loader now does the same: those jobs see the outer script, not the inserted one.

`ImageData` (HTML Standard, canvas): the constructor, `createImageData`, `getImageData`. `bare` draws nothing on a canvas (`browser_canvas.rs` is a no-op context), so `getImageData` and `createImageData` give transparent black: a `Uint8ClampedArray` of zero bytes. A page that reads pixels back from a drawing gets zeros. `putImageData` stays a no-op. One `ImageData` holds at most 64 MiB (`width * height * 4`). A larger one throws a `RangeError` with the message `Out of memory at ImageData creation`. Chrome also throws on a failed allocation. We did not check its message; the text is ours. Gradients are objects with a no-op `addColorStop`. `createPattern` gives `null` for an `<img>` that is not complete (the case of the spec) and an object with a no-op `setTransform` for any other image source. `isPointInPath` and `isPointInStroke` give `false`. `width`, `height`, `data` and `colorSpace` are own read-only data properties, not prototype accessors.

## Fragment parsing in a table context

`innerHTML`, `outerHTML`, `insertAdjacentHTML` and `createContextualFragment` parse in a table context now: `table`, `thead`, `tbody`, `tfoot`, `tr`, `td`, `th`, `caption`, `colgroup` and `col`. hsbc.com stopped on the gate: video.js sets `innerHTML` on a `thead` and on a `tr`.

- How: "reset the insertion mode appropriately" gives "in table", "in table body", "in row" or "in column group". The tree builder has these modes only for a `<template>` (the stack of template insertion modes). The root of the fragment acts as a template whose template insertion mode is already set to the mode of the context. In both cases the stack has no table element, so the same rules apply: text is not foster parented, and an end tag like `</table>` finds no table in scope.
- `td`, `th` and `caption` give "in cell" and "in caption". These differ from "in body" only for tags that need a cell or a caption on the stack. The stack of a fragment has none, so these contexts read as "in body". `col` has no mode in the reset steps: it is "in body".
- Proof: the 91 fragment tests of html5lib-tests with these contexts give the same tree (`tools/fixtures/html/table-fragments.dat`). There are none for `th` and `col`.
- Limits: the gate stays for `select`, `html`, `frameset`, `frame` and for SVG and MathML contexts. The change is in the setup of a fragment parse only. The page parse does not run it.

## History state caps

npmjs.com pushes one state of about 1.48 MB (9,301 objects) on load. The old cap of one state was 1 MiB, and a state over a cap stopped the page with a `Limit`.

- One state: 16 MiB. Firefox uses the same number (`browser.history.maxStateObjectSize` = 16777216 in `modules/libpref/init/StaticPrefList.yaml`; checked in `nsDocShell::UpdateURLAndHistory`). Chrome has no size check in `History::StateObjectAdded` (`third_party/blink/renderer/core/frame/history.cc`, checked 2026-11-10). bare counts a string as 2 bytes for each UTF-16 unit, 48 bytes for each object, and 16 bytes for each map entry.
- All states of one document: 32 MiB. Without this cap a page could hold 50 entries x 16 MiB. 32 MiB leaves room for about 20 states of the npmjs size.
- Objects of one state: 2^18 (was 2^14). The bytes cap bounds them too (48 bytes each). Depth stays 64.
- A state over the bytes, total or objects cap throws a `DataCloneError` `DOMException`. The page can catch it, and nothing is committed. The size is the page's choice, so it must not stop the page. Firefox throws too, but its error is `NS_ERROR_ILLEGAL_VALUE`, not a `DOMException`. bare uses `DataCloneError` because the serialize step of the standard throws that error.
- The depth cap stays a `Limit`. It guards the native stack of the walk.

## Locale extensions, console and `CSS`

`Intl` reads three `-u-` keys: `hc`, `ca` and `nu`. ECMA-402 lists these for `DateTimeFormat` (`hc`, `ca`, `nu`) and `NumberFormat` (`nu`), and `Collator` reads `co`, `kn` and `kf`. For `en`, ICU has one non-default collation (`emoji`, `eor`); bare gates it as `Unsupported`. Other keys change no output of bare, so it ignores them, as the spec does for a key a constructor does not list.

- The option wins over the keyword. A keyword stays in `resolvedOptions().locale` only when it was used. This matches Node 22.
- A `ca` or `nu` value that ICU has and bare has not is `Unsupported`, as the option is. An unknown value is ignored.
- `-t-` and `-x-` are checked for syntax and then dropped from the resolved locale. The canonical tag sorts the extensions and the keys, as UTS 35 says.
- A kept extension is in a side table (`IntlState::exts`), not in the formatter. A formatter with no extension pays no byte. The create functions set it in cold code, only while a constructor runs.

`console` has the whole WHATWG namespace. The sink has three levels (`Log`, `Warn`, `Error`), so `info`, `debug`, `trace`, `dir`, `dirxml` and `table` use `Log`, and `assert` uses `Error`.

- `time`, `count` and the label maps hold 1,024 labels. A label has 256 bytes at most. Over the bound, one warning is logged and nothing is kept.
- A group indents by 2 spaces. The depth stops at 64.

`CSS` has `supports` and `escape`. `supports` calls `bare_css::at_rules::supports_matches`, so `@supports` and `CSS.supports` use one parser. `CSS.registerProperty` stays undefined: bare has no registered custom properties.

## Slack in the JS heap tables at the peak

Problem: the JS tables hold 68% of the peak RSS of vimeo (see [what holds the memory at the peak of a page load](#what-holds-the-memory-at-the-peak-of-a-page-load)). The question was how much of the tables is slack: capacity that holds no entry, or a representation wider than the content.

Method: a temporary probe (not committed). When the accounted heap passed its last high by 1 MiB, `Heap::charge` read `Heap::footprint()`, counted the holes in the element vectors, and asked `mincore` which whole pages of each slack range were resident. Sites: vimeo, bsky.app and redis.io, 3 runs each, step file `timeout 30000`, `open URL`, `expect js "document.readyState === 'complete'"`, `shot s`. Base `50e71ca8cc`. The peak RSS is from 3 runs of the base `bare-drive` with no probe.

Slack at the peak of the accounted heap, MiB, median of 3 runs, and its share of the median peak RSS:

| Table | vimeo (peak 291) | bsky.app (peak 199) | redis.io (peak 281) |
| --- | ---: | ---: | ---: |
| Elements: capacity over the length | 37.6 (12.9%) | 0.75 (0.4%) | 1.84 (0.7%) |
| Elements: the resident part of that (estimate) | 25.0 (8.6%) | 0.54 (0.3%) | 1.84 (0.7%) |
| Elements: holes inside the length | 58.3 (20.0%) | 0 | 0.16 (0.1%) |
| Property slots: capacity over the length | 0.90 (0.3%) | 1.10 (0.6%) | 3.62 (1.3%) |
| Dictionary entries: capacity over the length | 0.87 (0.3%) | 0.77 (0.4%) | 0.08 (0.0%) |
| Object slab: payload capacity with no object | 17.9 (6.2%) | 0.84 (0.4%) | 18.5 (6.6%) |
| Object slab: the resident part of that | 0 | 0 | 0 |
| Strings: bytes saved at one byte for each unit (strings with all units under 256) | 0.98 (0.3%) | 5.30 (2.7%) | 1.39 (0.5%) |
| Elements that the shrink rule "capacity over 2 times the length and 64 bytes" frees | 0.01 | 0 | 0 |

- The accounted heap at its peak: 106.9, 35.2 and 44.9 MiB. The RSS at that moment, in the runs with the probe: 256, 208 and 252 MiB.
- The resident estimate for the elements counts the slack bytes on pages that also hold used elements in full, and the whole slack pages that `mincore` reports resident (vimeo: 7.0 of 19.7 MiB of whole pages).
- The object slab grows its payload `Vec` by doubling. `mincore` shows that none of the capacity past the last object is resident. Hypothesis: glibc grows a large block with `mremap`, and the new pages stay untouched until an object is put there.

Decision: no table shrinks. The rule: change a table only if its slack is at least 5% of the peak RSS on at least 2 of the 3 sites. Only resident bytes count, because untouched capacity is not in RSS and freeing it does not lower the peak. The object slab passes by capacity on vimeo and redis.io, but none of it is resident. The elements pass on vimeo only.

Why a shrink on collect would not help these pages: on vimeo the heap grows from 4 to 107 MiB in one loader round, and no collect runs inside it. Also, a vector that grew by doubling has less than 2 times its length as capacity, so the rule above finds 0.01 MiB.

What vimeo holds: 4,551 objects with a length of at least 64 that are more than half holes. They hold 58.2 MiB of holes and 90.9 MiB of element capacity (31% of the peak). Most are plain objects with no named properties (not arrays): 1,475 have a length of 951 and a capacity of 1,632, 670 have a length of 901, and 598 (of one shape with named properties) have a length of 801. In the lengths from 257 to 1,024, 3.5 M of the elements are holes (96%). Cause: a write to index `i` goes into the dense vector when `i` is at most `elems.len() + ELEM_GAP` (1,024). So a plain object whose integer keys spread up to 950 gets 951 elements of 16 bytes for a few values.

Next candidate (not done, because it is for vimeo only): keep the index keys of a mostly empty object in its dictionary, not in `elems`. A dictionary entry is 48 bytes and an element is 16, so the dictionary is smaller when under one third of the elements are used. To verify first: the rule of V8 and SpiderMonkey for sparse elements, and which vimeo script makes these objects.

Latin-1 strings: the saving is 2.7% of the peak on bsky.app and under 1% on the other two. It is recorded only. It changes the string paths everywhere.

## The same script text keeps one slot, and a selector may be 64 KiB

Two limits of `bare` stopped pages that Chrome loads. Chrome has neither limit.

- `Limit(Scripts)` stopped eu-europa and substack. The cause was not the pages. `bare-drive` polls `expect js` once for each tick, and each poll called `Interp::run_script` with the same 39 bytes. A realm keeps every parsed script until it ends, so the 4,096th poll hit `max_scripts`. A temporary counter showed 9,211 and 4,055 adds of that one text on the two pages. The pages themselves added 12 and 96 different texts.
- `add_script_kind` now gives a classic script the slot of an earlier script with the same text (`Interp::script_ids`: 4,096 buckets of 4 bytes, picked by the hash of the text, with a byte compare on a hit; a text that lands on a used bucket takes it over, so a collision costs one extra slot and never a wrong answer). The hash is a multiply hash with an overlapping read of the last 8 bytes, because the remainder copy of the shared hash cost more than the rest for a short text. A realm at `max_scripts` refuses first, so a refusal costs no lookup, and a repeated text is refused too once 4,096 different texts exist (as before this change). A map of all texts cost +0.77% on the `many` row of `scripts_load`; the table costs about +0.3%. A new run still instantiates the code again, so it makes its own functions, and it checks its own `let` and `const` names. It adds nothing to `script_source` or `script_nodes`. Modules do not share a slot, because each module has its own record. `max_scripts` stays at 4,096.
- One slot of a small script costs about 1 KB: 246 B of syntax nodes, 92 B of caches, 240 B of `Code`, 416 B of `Program`, and about 10 small allocations. 4,096 slots are about 4 MB. Different texts still fill the table, as before, and `max_script_source` and `max_script_nodes` still bound the memory.
- `max_selector_len` is 64 KiB (1,024 bytes before). GitHub sends lists of 3.4 to 4.0 KB to `querySelectorAll`. Over the cap the call still gives `Limit(Host("dom-selector"))`, which ends only its task.
- A selector parse is charged 4 steps for each byte. The worst parse measured is a list of one-letter tags: about 1,050 instructions for each byte (68 M instructions for 64 KiB). A step costs about 370 instructions in a DOM loop. A page that parses uncached 64 KiB selectors in a loop ends with `Steps` after about 380 of them.
- The new-name bound before a parse is `name_count + len / 2 + 1 <= max_names`. Only tag and attribute names are interned, and each name needs a byte and a separator. The old bound used `len`, which would have refused every long selector once the name table is not empty.
- The parsed-selector cache (`sel_cache`) held at most 256 selectors, with no bound on their size. It now also holds at most 128 KiB of selector text (`SEL_CACHE_BYTES`). It clears when either bound is over. A parsed selector is up to about 50 times its text (a list like `a,a,a`), so the worst case is about 6.5 MB (an estimate from the node sizes, not a measured peak).

## Page APIs added in PR #316, #317 and #320

Each line says what the code does now. Detail and limits are in the progress sections of those PRs.

- Observers (#316): the base DOM prototypes (`EventTarget`, `Node`, `Element`, `HTMLElement`, `SVGElement`, `Document`, `Text`, `Comment`) and the window get a `Symbol.toStringTag` at the first symbol lookup that misses (`browser_handlers::tags`), so Vue does not wrap a DOM node in a proxy and `IntersectionObserver.observe` accepts it. The observer code did not change.
- `document.write` (#316): a script that the parser runs inserts the markup after its own element and before that element's next sibling, and later writes keep their order. After load, the first `write` or `open` replaces the children of `<html>` with an empty `<head>` and `<body>`, and later writes append to the body until `close`. A written `<script>` does not run, and a tag split across two writes is not joined.
- SVG (#316): an SVG element gets the class of its tag (`SVGElement`, graphics, geometry and root classes; `SVGAnimatedString` for `className`). `width`, `height` and `viewBox` give `baseVal` and `animVal` snapshots. `getTotalLength` and `getPointAtLength` use the path flattening of `bare_paint`, `getBBox` reads `x`, `y`, `width` and `height`, and `getCTM` and `getScreenCTM` give an identity matrix.
- Canvas (#316): `getContext('2d')` gives one inert context for each canvas, and any other type gives `null`. Drawing methods do nothing, `measureText` gives 6 units for each character, and `toDataURL` gives a transparent 1 by 1 PNG.
- Indirect eval (#316): `eval` is the global eval (`Interp::eval_global`). It runs in a new lexical environment under the global one. Non-strict `var` and function declarations go to the global object; strict ones stay in the eval. Direct eval does not see the caller's locals.
- Named frames (#317): `window[name]`, `window[index]` and `frames.length` find the iframes that are in the document, by name, id or index. The child document is inert: it has no navigation, script or fetch. `document[name]` gives the iframe element.
- Stream request bodies (#317): a `ReadableStream` as the body of `fetch` or `new Request` is not buffered. It throws a `TypeError` ("ReadableStream request bodies are not supported"), so `fetch` rejects one request and a feature test with `try` and `catch` goes on. It was `Unsupported`, which ended the page.
- URLs with user info (#317): `http` and `https` URLs keep the user name and password (`URL.username`, `URL.password`, `href`). The `origin` and `host` leave them out. `fetch` and `new Request` reject such a URL with `TypeError` before they queue a request. `history.pushState` with another user info throws `SecurityError`. HTTP host names use the IDNA conversion of `bare_url`.
- `Function.prototype.toString` (#320): it gives the source text of a script function or class, `function NAME() { [native code] }` for a native one, and throws `TypeError` for a value that is not a function. The text of a method, getter or setter starts at its parameters.
- Legacy dates (#320): `Date.parse` and `new Date(text)` read forms that are not ISO 8601 or `toUTCString` with a port of the V8 reader (`6 November 2026`, `Month DD, YYYY`, `YYYY/MM/DD`, `MM/DD/YYYY`, AM and PM, `GMT+HHMM`, zone names). A text with no zone is local time, and a text that fits no form is `NaN`.
- `await` in object patterns (#320): an async function binds object patterns with defaults and nested object patterns with `await` in `var`, `let` and `const`. Array patterns, computed keys, catch parameters, `for-of` and `for-in` left sides and assignment patterns with `await` are still `Unsupported`.

## One property operation asks the host at most three times

- A heap operation that needs the host returns `NeedsHost` or `NeedsNamed`. The heap changes nothing when it asks. The evaluator answers the ask (`Host::fill_indexed`, `Host::named_write`) and runs the operation again. `MAX_FILLS` (`eval.rs`) is the number of repeats: 3. A fourth ask is a host bug. It gives an error, never a loop.
- Each source asks at most once in one operation. The receiver, when it is a host object (`new_indexed`, `new_named`), asks when its list is out of date; a write can also ask `NeedsNamed`. The frames object on the chain of `window` and `document` asks when its list is out of date or a context is missing; one fill does both. `EventTarget.prototype` (`Heap::set_lazy_chain`) asks once for each name. The own lazy names of the receiver (`Heap::set_lazy_on`) ask once for each name; these receivers are `window` and interface prototypes, never host objects.
- A write of an `on<x>` name to `window` asks three times: the frames object, `EventTarget.prototype`, then `window`. The bound was 2 before. target.com stopped there with `Heap(NeedsHost)` (`docs/progress.md`, "Host asks and cached prototypes").
- Rule for a host object that caches a list: ask only when the list is out of date, and bring it up to date in one fill. A fill never changes the tree, so the list stays up to date until the operation ends.
- The frames object keeps its iframe list, its keys and a map from name to iframe for one document revision (`Document::revision`). The fill charges one step for each node of the document, once for each revision. Before, each named query walked the tree. It charged one step for each node for each query, in prepaid groups of 8, and asked again each time the 8 ran out. That ask could come in any repeat of an operation.

## Every prototype that the host caches is a root

- `Dom::wrap` takes the prototype of a new wrapper from a host cache (a `OnceCell` or a `Cell`). `Host::roots` must hold each cached prototype from the moment the host makes it, whether or not other state of its module exists.
- `ce_roots` returned early when the page had not used the custom element registry. So it left out `HTMLUnknownElement` (`ce.unk`) and `ce.int`. A collect freed them, and the next unknown element (`document.createElement('walrus')`) got a dead prototype: `Heap(Stale)`. They are now roots before the early return.
- How wrappers stay live: `by_node` and `by_obj` are not roots. The document wrapper is a root, and `Host::trace` links each wrapper to the wrappers of its parent and children. So a wrapper lives while the tree or a JS value reaches it. `Dom::prune` removes dead wrappers from both maps after each collect. A collect runs only when no call is active (`Interp::collect`), so `wrap` never meets a collect between making a wrapper and its insert. The CSSOM keeps its sheet objects and their owner wrappers as permanent roots (`browser_cssom::keep`). A shadow root wrapper goes in the main `by_node`.
- `browser_heap_stop_tests.rs` makes one element for each cached prototype of `Dom::wrap`, collects, and makes them again.

## `Intl.Locale` and `Intl.RelativeTimeFormat`

`Intl.Locale` and `Intl.RelativeTimeFormat` exist. wise.com needs `Intl.Locale` and theverge.com needs `Intl.RelativeTimeFormat`. `DisplayNames`, `ListFormat`, `DurationFormat` and `supportedValuesOf` stay absent.

- `Locale`: the tag goes through `canonical_tag` (the code of `getCanonicalLocales`). Options replace `language`, `script`, `region` and the `-u-` keywords. The getters are `baseName`, `language`, `script`, `region`, `calendar`, `caseFirst`, `collation`, `hourCycle`, `numeric` and `numberingSystem`. `getCalendars` and the other `get*` methods are absent. `new Intl.Locale("und").language` is `"und"` (ECMA-402 `GetLocaleLanguage`); Node 22 gives `undefined`.
- Likely subtags: a table of 40 languages in `intl_locale.rs` (about 1 KB). The full CLDR table has about 1,900 rows. bare has data for English only, and a page asks `maximize` about a common locale. A language that is not in the table stays as it is. `und-Latn` and `und-US` use the first row of the table that has the script or the region.
- `canonical_tag` now sorts variants (`sl-1994-biske-rozaj`). Node 22 does this. The old test expected the unsorted order.
- `RelativeTimeFormat`: the patterns are in `intl_rtf_data.rs` (made by `tools/js/intl_rtf_gen.mjs` from Node's ICU, 7.5 KB of source). There are three groups: `en` and `en-US`, `en-CA`, `en-GB` and `en-IN`. The generator checks each pattern against Node. The number part is the `NumberFormat` code (3 fraction digits) and the plural is `PluralRules`.
- `numeric: "auto"` uses a word when the value rounds to -1, 0 or 1 at two fraction digits (`1.005` is "tomorrow", `1.0051` is "in 1.005 days"). This is what ICU does (checked on Node 22).
- Both constructors are accessors on `Intl` until the first read, like the global `Intl`. A realm that reads `Intl` for another constructor pays for 2 functions, not 21. Without this, `intl_load make 1` was +0.97%.
- The new Rust code of the tag logic and the data are `include!`d by `intl.rs`, not new modules. Two new modules moved the lexer and parser to other codegen units: `lex_load small` +2.1%, `parse_load chain` +1.3%.
- Not supported: a `numberingSystem` other than `latn` (gate), a language that ICU has and bare has not (its text is English), the `variants` and `firstDayOfWeek` options, and CLDR alias rules (`sgn-US` is `ase`, `en-posix`).

## Sparse index writes on plain objects

A far index write can make hundreds of resident holes for a few values. Plain objects now check density when a write grows the element length by more than 16 slots. The existing 1,024-slot maximum gap still applies. The write stays dense when at least one quarter of the proposed slots hold values, including the new value. Each element costs 16 bytes. A dictionary entry costs 48 bytes, plus 24 bytes for its index share above eight entries. One quarter occupancy therefore gives dense storage a small preference over the dictionary break-even point of 2/9.

The density helper is cold and out of line. It scans at most 4,096 slots per decision. Byte accounting and storage can each call it, so one property write scans at most 8,192 slots. Larger existing vectors keep the old gap rule. In-range writes, appends and gaps of at most 16 do not scan. Arrays and objects with named properties keep the old rule. This keeps the storage policy for reverse fills of pre-sized arrays and retains shape-based named reads. Reject writes beyond the existing 1,024-slot gap before the cold helper call. This avoids cold calls for each distant index during reverse fill. The dense write and append paths do not call the helper. A local workload with 30 spread indices and 1,000 named reads per object rose from 192,903,763 to 197,798,628 instructions (+2.54%) when named objects took the sparse rule. There is no dictionary-to-dense migration.

Sources read on 2026-11-10:

- [V8's conversion check](https://github.com/v8/v8/blob/main/src/objects/js-objects-inl.h) rejects gaps of at least `kMaxGap`, then compares the new capacity with a dictionary size estimate. [Its constants](https://github.com/v8/v8/blob/main/src/objects/js-objects.h) set `kMaxGap` to 1,024 and `kMaxUncheckedOldFastElementsLength` to 500. Small capacities bypass the density check. [NumberDictionary](https://github.com/v8/v8/blob/main/src/objects/dictionary.h) sets `kPreferFastElementsSizeFactor` to 3. The estimate multiplies dictionary capacity by entry size and this factor. [GetFastElementsUsage](https://github.com/v8/v8/blob/main/src/objects/js-objects.cc) uses the length for packed storage and counts used entries in holey storage.
- [SpiderMonkey's constants](https://github.com/mozilla-firefox/firefox/blob/main/js/src/vm/NativeObject.h) set `MIN_SPARSE_INDEX` to 1,000 and `SPARSE_DENSITY_RATIO` to 8. [willBeSparseElements](https://github.com/mozilla-firefox/firefox/blob/main/js/src/vm/NativeObject.cpp) counts non-hole values until it meets the required density. Packed storage uses its initialized length. [extendDenseElements](https://github.com/mozilla-firefox/firefox/blob/main/js/src/vm/NativeObject-inl.h) calls the check above that minimum and keeps an indexed object sparse to avoid repeated scans.

## Lazy compatibility APIs and snapshot collection prototypes (#333)

Rule: keep these compatibility APIs lazy and state their limits. They do not establish complete standards support. This entry records #333 after the fact. Source: `browser_gaps.rs`, `browser_shims.rs`, `shims/{animation,idb,path2d}.js` and the merge diff `8f6a38e9..f857a90b`.

- A missing name in `Heap::browser_name` asks the host. `browser_shims::define` evaluates one source through `eval_global` if its sentinel global is absent. The name groups are `animate`/`Animation`, `Path2D`, and `indexedDB`/`IDB*`. An unused shim creates no script or objects. This sentinel check is not a separate permanent initialization flag.
- Collection snapshot arrays share a lazily made prototype with `item`, above `Array.prototype`. `array_from_proto` sets it at creation. A later `set_prototype_of` sent arrays to dictionary mode and regressed the collection row. These arrays remain snapshots. They lack `namedItem`; `querySelectorAll` does not gain `item`.
- `Element.animate` ignores keyframes and options. Its animation finishes on a zero-delay timer. It supplies promises, state changes and finish/cancel listeners. It has no visual effect or timeline. `currentTime` is zero or null. `Path2D` records no path; its eleven path methods do nothing.
- The database shim keeps sorted store rows in one realm. It provides basic stores, key ranges, requests and cursors. Requests run in microtasks; request errors abort and undo recorded writes. Values use `structuredClone` when available. It has no disk persistence, indexes, `IDBIndex`, blocked-connection protocol or `getAll` options object. A defined API name is not proof of full IndexedDB semantics.
- The other small additions are reflected `noModule`, always-true `document.hasFocus`, collection iterators, and replaceable `window.event`. The event is held for the whole dispatch and restored after nested dispatch. It is not filtered for shadow-tree listeners.

Evidence: #333's progress section reports nine failing-first tests and limited live probes. No new browser comparison was run for this entry. #333 did not complete the full test262 gate or repeat `exec_diff` after its final rebase. Later-main gates cannot repair that chronology. See the verification audit at the end of progress.md for retained commands, results and the targeted historical fetch measurement. The fetch increase has cause unverified; this entry does not waive a cost row.

## Accept live memory comparisons only after resource and work checks

Readiness alone is insufficient when a main application resource fails. Retain every failed run with its resource status, bytes and error. Report failed or partial runs separately from successful equal-work comparisons. Match URL, viewport, steps, binary SHA and run order. Record application sentinels or DOM counts, error types, elapsed time and peak RSS. Identify the heap metric; glibc live heap is not JS-only heap. Report median and range without attributing causation from a median alone.

Task 34 evidence: the retained #340 bsky BASE bundle failed at status 0/zero bytes; NEW fetched status 200/2,488,274 bytes. Both readiness checks passed. The 22.3/197.7 MiB pair is unequal work, not evidence of a memory regression. Three frozen pairs on the #340 parent/merge did not repeat that large gap. The Verge mixed-work median gap remains unisolated; the older d8550bbba0 comparison predates Intl. Vimeo pair 4 has no recorded network resource failure on either tree, but both fail the same idle-page readiness check. A deferred-resource or deterministic loader cause is unverified.

Four targeted instruction rows distinguish per-PR changes from accumulated drift. Repeated allocator-specific changes do not establish allocator noise. No cost waiver follows from this audit. Evidence and limits: [Task 34 in progress.md](progress.md#sparse-live-measurements-and-allocator-drift-task-34), `/data/bare-tmp/sla/`, and `/data/bare-tmp/se/round2/final/`. Existing Tasks 29, 30 and 31 retain API, loader and historical-fetch ownership.

## Fatal heap stops retain only the variant name

A fatal heap error reports a static variant name, such as `Heap("NeedsHost")`.
The loader maps every current heap error variant in `fatal_of`.
The match is exhaustive. A new variant requires a label.
The mapping runs only on the error path.
It discards object handles, indices, values and execution text.
Public stop text keeps `page stopped` and `Heap` for survey matching.
Recovery, budgets, evaluator fill limits and heap layout do not change.

The diagnostic tests failed first on main: all public heap stops were only `Heap`.
The fixed tests cover all 16 variants and the public page stop reason.
A constructed sticky stop blocks page scripts and I/O on three later loader runs.
Measured sizes stay at 32 bytes for Fatal, 104 for Stop, 1,200 for Scripts and 368 for PageResult.
The required cost case is the full matrix because Fatal is stored in loader state.
Final BASE 617cb19f89: all 805 rows passed under both allocators with fresh BASE counts.
No row exceeded +0.5% under either allocator. Workspace tests and all 48 public journeys passed.
Target reached complete in all six live runs without a heap fault. Apnews kept the same Steps-abort stop on both builds.

## Full style computations charge their indexed candidates

Changed on 2026-11-10. A full compute pays one step per document node before it runs, then one step per tested rule candidate. The independent `computed-work` cap still checks nodes or styled elements times rules before the compute. It bounds one native operation. A failed charge does not leave a partial DOM write: style computation reads the document.

The old full-compute estimate charged styled elements times all rules divided by four. On apnews, five estimates charged 103.35 M steps while the script used 78,712 interpreter ticks. The rule index excludes most rules. The estimate ended the main bundle and later tasks. Restyle charges keep their existing model. The task budget remains 100 M steps.

CSSOM defines the values returned by [`getComputedStyle`](https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle), not an execution-step formula. The charge measures this implementation's traversal and indexed candidate work. It does not alter resolved values.

## Pending shorthand substitutions need state beyond serialized text

A longhand of a shorthand with `var()` retains its original shorthand until computation. Serialize an exposed pending longhand as an empty value. Do not discard its state or substitute the variable at write time. See [CSS Variables 1 section 3.2](https://www.w3.org/TR/2022/CR-css-variables-1-20220616/#variables-in-shorthands) and [CSSOM declaration serialization](https://drafts.csswg.org/cssom/#serialize-a-css-declaration-block).

Task 32 found a scope boundary. `browser_style::style_set_text` serializes the block into the DOM attribute. `style_read` treats the cached block as a parse of that text. The cascade reads the attribute. A partial pending shorthand cannot pass through this path with both standard serialization and preserved computation. Empty serialized values lose their shorthand on a later parse. Writing the whole shorthand restores removed members. Resolving variables at the write loses later custom-property changes.

The browser check retained the right margin after `margin-top: 1px` over `margin: var(--m)`. Its serialized right margin was empty. Changing `--m` changed the computed right margin from `4px` to `8px`. Copying `cssText` to another element lost that margin. Serialization alone is not a lossless state store.

No implementation changes were made. A follow-up needs permission to change the style state and its input to the cascade. It must preserve the source shorthand, surviving longhand mask, priority and declaration order independently of attribute text. It must also invalidate that state for attribute replacement and handle stylesheet publication. Those paths include `browser_style.rs` and computed-style/cascade integration, which Task 32 excludes. Coordinate with their owners before work. Retain bounded storage and budget charges. Measure any hot representation change with the full cost matrix.

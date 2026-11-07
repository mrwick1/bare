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

| Crate          | Folder            | Contents                    | Uses                   |
| -------------- | ----------------- | --------------------------- | ---------------------- |
| `bare-unicode` | `crates/unicode/` | NFC, generated Unicode data | -                      |
| `bare-url`     | `crates/url/`     | URL parser, IDNA            | `bare-unicode`         |
| `bare-net`     | `crates/net/`     | DNS, TCP, HTTP              | `bare-url`             |
| `bare-html`    | `crates/html/`    | encoding, tokenizer, DOM, tree builder | -           |
| `bare-css`     | `crates/css/`     | tokenizer, parser, selectors, cascade | `bare-html` (from topic 3) |
| `bare-layout`  | `crates/layout/`  | box tree, text, block and inline layout | `bare-html`, `bare-css` |
| `bare-paint`   | `crates/paint/`   | display list, pixel buffer, glyphs, PNG capture | `bare-css`, `bare-html`, `bare-layout` |
| `bare-window`  | `crates/window/`  | window interface, Wayland backend (smithay-client-toolkit) | - |
| `bare`         | `crates/bare/`    | the app (`main.rs`)         | `bare-net`, `bare-url`, `bare-html`, `bare-paint`, `bare-window` |

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

## Grid

- A `Value` is 8 bytes and `Copy`, and a track list is not. A track list, a line, or a set of areas goes to a side list: `Value::Grid(i)` points to a `grid::Grid` in `Side::grids` (declared) or `Styles::grids` (computed). It is the same idea as `Length::Calc`. The cascade keeps each computed list once (a hash of the value), so 100,000 equal rules keep one list. A `style=""` attribute and a `var()` offset their indexes. `inherit` copies the index. Chrome stores the track lists in a `ComputedGridTrackList` that styles share by pointer.
- A track list is parsed from the raw tokens (`crates/css/src/grid.rs`), not by `Parts`, because `repeat()`, `[a b]`, `1fr` and strings do not fit in a part. Line names are kept, as `Part::Names`, because the lines of an item can use them. The cost is one `Box<str>` for each name. `grid-auto-flow` is a number (`row` is 0, `column` adds 1, `dense` adds 2). `none` and `auto` stay keywords.
- Placement keeps the used cells in a bit set (`Occ`), one row of words for each row of the grid. A search for a free place skips 64 cells at a time. `dense` starts a search at the row where an item of the same shape was found last: the rows before it are full for that shape and a cell never becomes free, so the result is the same as the spec. A grid of N items costs about O(N + cells / 64). A plain loop over the cells costs O(N x cells) for a dense grid with spans.
- Items are laid out straight into one temporary list of fragments, at the width of their columns. The rows come from their heights. An item that stretches and holds boxes is laid out a second time at its stretched height, and the first fragments stay in the list, dead. Only the live ranges are copied to the page. A stretched item with only text gets a taller fragment: no second layout. An item with a percent height also needs the second layout, because its percent had no base in the first one. The count of static positions that waited inside a dead range is taken out of `Cx::waiting`. LayoutNG lays out a stretched item again too, unless its cache says that the result is the same.
- The contributions of an item (min-content, max-content, automatic minimum) are measured only if a track that it covers needs them. A track of fixed size (`100px`, or `minmax(0, 1fr)` with a known width) does not.
- A percent track in a grid with no size on that side is `auto` for the sizing. A `fit-content(%)` there is `max-content`. A grid with no height uses the max-content sizes for the rows (section 11.7 with no free space), and if its `min-height` (in px) is more, the rows are sized again in that room.
- An `auto-fit` track that holds no item has size 0 and no gutter. A gutter shows only between two tracks that show. The free space of `justify-content` and `align-content` goes to the tracks that show.
- A child that is out of the flow is no item. If a run of text holds one, the lines place its static position. If the run is only spaces and out-of-flow boxes, the box gets a static position at the corner of the content box.
- Baseline alignment is `start`, and `last baseline` is `end`. The first baseline of the container is the one of the first item in the first row. Subgrid and masonry are not built.
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
- The loader reads the targets of the parsed document in order and starts the next ones while fewer than `WINDOW` (6) wait. A result is taken when it is the turn of the element. Script order, `defer`, `async`, `onload` order and the errors are the same as before; only the waiting is shared. The text of a file that finished early counts against the byte caps when it is taken, not when it is read.
- `fetch` and XHR: the first 6 queued requests that are a GET or a HEAD start together. A request with another method runs alone, when all before it are done, on the page thread. The server sees these in the order of the script.
- No worker is used until a reply took 10 ms or more to start (`Client::parallel_pays`), and not for a single download. On a fast link (loopback, a LAN) a thread costs more than the round trip it hides: without this the loader rows of `resources.md` rose by up to 4% in instructions. The first request of a page is the document, so a remote server marks itself before the first script.
- Timing is off unless `BARE_NET_TRACE` is set. When it is off, the cost is one read of a `OnceLock` for each request, and no clock read for the trace.
- HTTP/2 is used when the server picks `h2` in ALPN (see [HTTP/2 is our own code](#http2-is-our-own-code-one-reader-thread-for-each-connection)). Staging answers HTTP/1.1 only (Chrome reports `http/1.1` for all 69 requests of three loads; `h2` gets no ALPN, see [staging-network.md](staging-network.md)), so staging does not change.
- The request body is copied for a worker (a `Pending` holds values of the realm and cannot leave the thread).

## Compressed bodies and the connect backup

Follow-ups of [Parallel downloads](#parallel-downloads). The numbers are in [resources.md](resources.md#compressed-bodies-and-the-connect-backup). The choices:

- The client asks for `gzip, deflate` and, over https only, `br` (as Chrome does). The decoder is a chain of `Read` stages over the body, built on the first read. The page loader is the exception: a gzip or deflate download is read whole and decoded in one step (see the end of this section).
- Fetch rules: codings are undone from the last to the first, at most 5. Headers stay as sent, so `content-encoding` and `content-length` are visible to a script, and `fetch` shows the decoded bytes. A coding that the client does not know (`zstd`) is passed through unchanged, and `Response::encoded()` is true. The callers (loader, session, `fetch`) refuse such a body with an error, as before.
- The decoded size has the cap of the wire size (`MAX_REPLY`, 128 MiB), and the caller caps apply to decoded bytes. A body that grows past the cap is an error, so a zip bomb stops at the limit. A bad gzip, deflate or br body is an error. A body with no content (HEAD, 204, 304) is not decoded.
- When the decoder ends, the client drains the raw body. That checks the framing and returns the connection to the pool. A connection that is not drained is not reused.
- `deflate` can be a zlib stream or a raw stream. The first 2 bytes decide.
- The client sends no `Accept-Encoding` to a loopback host (`localhost`, `127.x.x.x`, `::1`). It saves no byte there and costs both ends CPU, and the loopback rows rose up to 2.15% in instructions when it was sent. A header that the caller sets wins. `IpAddr::parse` was too slow for this check (about 380 instructions for each request), so a string test is used.
- `Vary: Accept-Encoding` alone no longer blocks the client cache. The client sends the same `Accept-Encoding` for every request to a host unless the caller sets one, so the key does not differ in practice.
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
- If `with` or a direct `eval` is added, the first walk must open a barrier for the code that they can reach.
- Reason: a bundle reads outer variables through a chain of closures, and every read searched each environment on the way. See [resources.md](resources.md#cost-of-identifier-lookup).

## Subclassing built-in constructors

- A built-in object is a plain heap object plus a side table keyed by its id (`colls`, `weak.colls`, `date.times`, `promise.map`, the buffer and view kinds). So a subclass instance needs only the right prototype. No constructor makes a hidden "subclass" object.
- `construct_nt_inner` (Construct with a different `new.target`) puts `new.target` in `cls.native_nt` before it calls the native constructor. The constructor takes it first (`native_nt.take()`), so a nested construct inside an argument conversion cannot see it. `nt_proto` reads `prototype` of `new.target` before the object exists. `nt_patch` sets it after, for the constructors that make the object in a helper (`Promise`, typed arrays, `DataView`, `TextDecoder`). `Date` passes the prototype to `date_new`.
- A host constructor opts in with `add_ctor_nt` and reads `host_proto_from_new_target(default)`. A plain `new F` passes no `new.target`, so the host constructor gets `default` and runs no property read. This is exact because the `prototype` of a host constructor is read-only. The `default` must be that `prototype`. The read cost about 1% on rows that make events.
- `NewPromiseCapability` for a constructor other than `Promise` runs the constructor with an executor that keeps `resolve` and `reject` in a holder object. A side table `caps` maps the new promise to the two functions. The mark phase queues them once, at the end, for each reached promise in the table. A per-object check cost +0.8% on every GC row, so it is not there. The intrinsic `Promise` never uses the table. Its resolving pair is the old direct path, so a plain `Promise.resolve(x)` allocates no function.
- A custom capability in the combinators uses the pair id `u64::MAX`. The "already resolved" flag then lives in the user functions, not in `done_pairs`.
- `species_ctor` replaces the old gate for `then` and `finally`. The species constructor of an array, a typed array, an `ArrayBuffer` and a `Promise` is read once, with the V8 messages. A species that is the intrinsic constructor takes the old fast path.
- The brand of an error object is not stored. `Object.prototype.toString` tests an own `stack` and `Error.prototype` in the chain. Reason: no extra field for each error, and the tag matches V8 for the cases of the corpus. It is a heuristic, listed in progress.md.
- Not done: custom elements (no `customElements` yet); `Request`, `Response`, `Headers` and `URL` (each constructor must read `new.target`, one change for each); `extends Function` and `extends Symbol`. No target page needs them yet.
- Cost rule for this change: no `instructions:u` row worse than +0.5% against c66c8a5. The worst row is +0.41%. See [progress.md](progress.md#class-x-extends-built-in).

## Frame slots

- A function that has no inner function and no class keeps its variables in a frame, not in an environment. A frame is a run of bindings at the end of the bindings of environment 1, from `Interp::fp`. A call makes no environment. A block, a loop head, a `switch` or a `catch` inside the function makes none either. A `let` or `const` goes back to its TDZ each time its block is entered. V8 keeps such variables in registers and SpiderMonkey in frame slots.
- A function keeps its environment when it has an inner function or class, `arguments`, `this` (an arrow function may use it: it reads the one of its closure), `new.target`, `super` or `delete name`. It also keeps it when it is a class member, a constructor, an async function, a generator, a named function expression, has a parameter pattern or default, repeats a parameter name, or has more than 255 names. A generator or an async function must keep its variables across a suspension. This engine has no `eval` and no `with`, so no code can name a local by a string.
- Frame slots are for leaf functions only. A function with an inner function keeps its environment, because a closure reads the variables of its parent through the environment chain. Not done: a capture analysis that gives a frame to a function whose names no closure reads.
- A function with a loop or a call gets its frame at its first call: one call can be long, or recursive. Another function gets it at its second call. Its first call runs in an environment, and no site of it is resolved during that call (`hold`). So a function that runs once pays no layout. The cost is one state byte and one check for each call.
- Not done: no environment for a block of a function that has an inner function. The profile shows the environment arena at 0.5% to 8.6% of the rows (see [resources.md](resources.md#cost-of-frame-slots)). The cost of a call is binding setup and name access, and a block environment is not among the top ones.
- Rule status: two rows of the cost rule are over +0.5% with both allocators (`scripts_load few`), and three rows of `class_load` are at +0.50%. See [resources.md](resources.md#cost-of-frame-slots). The change was not merged for that reason.

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

Changed on 2026-11-05. Text is drawn from the outlines of Noto Sans Mono, the font whose numbers layout uses. `data/fonts/` holds Regular and Bold, each cut down to 22 KB (Basic Latin, Latin-1, common punctuation, bullets, boxes and arrows), with the OFL license. The files are built into the program, so no system font is read and the pixels do not depend on the machine. A letter that the files lack is not drawn. There is no fallback font yet. There is no bitmap font.

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

## The heap collects when it has grown by half

Problem: no collection ran inside one driver call. Typing 50 keys into the React form took the process to 214 MiB.

Decision: `Interp::gc_due` is true, at the end of a driver call and in each loader round, when the accounted heap is above `max(8 MiB, 1.5 x the heap left by the last collect)`. The numbers follow QuickJS (1.5 x) and SpiderMonkey (low-frequency growth of 150%). Sweep on this page with 30 keys (before the property room change, floor 1 or 4 MiB): factor 2.0 gave a peak RSS of 42.8 MiB and 11 collects, 1.5 gave 35.4 MiB and 19, 1.25 gave 31.2 MiB and 34, 1.1 gave 31.3 MiB and 34. Factor 1.25 saves 4 MiB and makes 79% more collects, so 1.5 is the knee. The floor of 8 MiB was not swept.

Cost: one key into the page costs +3.6% instructions (443.5 M to 459.4 M; the same under jemalloc). Each collect takes 8 to 11 ms. 27 collects ran in 50 keys, and a key takes 70 to 85 ms. The page falls from 214 MiB to 33 MiB at 50 keys, and it stays flat. The cost is accepted: a page that is typed into for minutes cannot grow without bound, and no `*_load` row is worse than +0.5% with jemalloc.

Not collected inside a call: the collect needs depth 0 and no statement frames, so a long synchronous script still keeps its garbage until it returns. The event cap and the loader rounds are its collect points.

## Template parsing follows Chrome 155 where it differs from the standard

The tree builder has the "in template" insertion mode. Chrome 155 differs from the standard in two cases, found with headless Chrome (`tools/fixtures/ui/tplmodes-cases.txt`). The builder follows Chrome, because a page is written against Chrome. No html5lib test covers these cases.

- The standard keeps a template "in template" after `base`, `basefont`, `bgsound`, `noframes` and `title`. Chrome does not: `<template><title>t</title><td>x` drops the `td`. The standard and Chrome agree for `link`, `meta`, `script`, `style` and `template`.
- In a table mode inside a template, the standard ignores a `<form>` start tag. Chrome inserts an empty `form` and does not set the form element pointer.

To follow the standard, add the five names to the first arm of `Builder::in_template` and ignore the form tag in the arm for `form` in `in_body`.

## Open questions

- A separate process for sensitive sites, for example banking.
- When to replace the bitmap font with TrueType.

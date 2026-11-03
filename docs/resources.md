# Resource use

This file records how much time and memory each part of the browser uses. Our priorities are speed, efficiency and low memory use. Update the tables when a module changes, and add a row for each new module.

## How to measure

Run `cargo run --release -p bare-bench`. It builds one page of 100,000 `<div>` and 100,000 `<span>` elements (5.2 MB), and one style sheet of 100,000 rules (6.9 MB), and runs each step five times.

- `ms` is the median time of the five runs.
- `peak MB` is the most heap memory alive at once during the step.
- `kept MB` is the heap memory that the result still holds, for example the DOM.
- A counting allocator measures the memory. It counts bytes that the program asks for, so it is lower than the RSS that `top` shows.

`cargo run --release -p bare-bench -- rss` runs a fresh process that only builds one DOM, and prints how much the real memory (RSS) of the process grew. The counting allocator does not see the overhead of `malloc` for each small allocation, so RSS is the higher and more honest number. Use RSS to compare with Chrome.

Machine: archMachine, release build, 2026-10-29.

## bare, by module

| Module | Step                         | ms   | peak MB | kept MB | Note                                   |
| ------ | ---------------------------- | ---- | ------- | ------- | -------------------------------------- |
| 1      | URL parse, 100,000 URLs      | 17   | 0.0     | 0.0     | about 170 ns for each URL              |
| 1      | URL join, 100,000 links      | 27   | 0.0     | 0.0     |                                        |
| 2      | Bytes to text                | 6    | 15.6    | 5.2     | one copy of the page                   |
| 2      | Tokenizer (streaming)        | 20   | 0.0     | 0.0     | counts 600,003 tokens, keeps none      |
| 2      | Tree builder (with tokenizer) | 45  | 23.1    | 17.8    | 400,006 nodes, 44 bytes each           |
| 2      | Tree builder, 20,000 nested divs | 2.2 | 1.3 | 0.7 | was quadratic: 10,000 levels took 151 ms. An open `<p>` counter fixed it |
| 3      | CSS tokenizer                | 39   | 0.0     | 0.0     | 6.9 MB, 2,500,000 tokens, 190 MB/s     |
| 3      | CSS parser (with tokenizer)  | 85   | 28.6    | 0.0     | 100,000 rules, 400,000 declarations    |
| 3      | CSS parser + values          | 123  | 31.2    | 0.0     | 400,000 declarations make 700,000 items of 12 bytes (8.4 MB if kept). Values cost about 90 ns each |
| 3      | Cascade: add 10,000 rules    | 11   | 7.1     | 5.9     | 10,000 rules, with `ua.css` (about 500 bytes for each rule: `Selector` has 2 `Vec`s) |
| 3      | Cascade: styles of 200,000 elements | 117 | 7.6 | 4.4 | 5,003 different styles: 22 bytes kept for each element. 0.6 µs for each element |
| 4      | Layout of 200,000 elements   | 32   | 14.4    | 14.4    | 200,002 fragments of 36 bytes, 0.16 µs for each element. The list gets room for one fragment for each DOM node (400,006) at the start, so it never grows. Half of that room stays empty on this page |

DNS (2026-10-29, real network, so the numbers move with the resolver):

| Step | Before | After | Why |
| --- | --- | --- | --- |
| Cold lookup (cache of the OS resolver flushed) | about 113 ms (2 round trips of 56 ms) | about 56 ms | The A and AAAA questions go out at the same time. The noise of the upstream resolver is large (55 ms to 2 s), so only a run with a steady network shows the gain |
| Lookup that hits our cache | 47 µs | 0.4 µs | The cache comes before the read of `/etc/hosts` |

HTTP body (2026-10-29, `bare-stress scale net-body`, a server on 127.0.0.1, so only our own time and memory count):

| Reply | Peak before | Peak after | Why |
| --- | --- | --- | --- |
| 100 MB, `Content-Length` | 235 MB, 83 ms | 100 MB, 49 ms | The body is not copied out of the read buffer. The buffer gets its full size at once, so it does not grow by doubling. The headers are read once, not after each read |
| 10 MB, chunked | 33.6 MB | 17.2 MB | The chunks join inside the read buffer |
| 10 MB, `Content-Length`, sent in 1 KB writes | 27 MB | 10 MB | The same two changes |
| Cache hit of a big body | one copy of the body | no copy | The cache gives out a shared `Arc<Response>` |
| 100 MB, read as a stream (`Client::get_stream`, 64 KB pieces) | 100 MB | 0.1 MB, 13.6 ms | The caller reads from the connection into its own buffer. Nothing keeps the body. A chunked body of 100 MB also takes 0.1 MB (760 ms: 1,000-byte chunks) |


The peak of an endless body (a server that never stops) is 268 MB in the counting allocator and about 128 MB of real memory: the list for the body doubles from 128 MB to 256 MB, and only the pages that are written count for the system. A stream (`get_stream`) of an endless body stops at the limit of 128 MB with 0.1 MB of memory.

Not measured:

- TCP, and HTTP on a real network. They wait for the network, so a CPU benchmark says little.
- The `bare` binary is 2.1 MB (release build).

History:

- 2026-10-29, first run: the tokenizer returned a `Vec` of all tokens (126 MB peak, 105 MB kept) and copied the page into a `Vec<char>`. The tree builder peaked at 135 MB for 67 MB kept.
- 2026-10-29, streaming tokenizer: the tokenizer reads the page text in place and gives one token at a time. The tree builder peak fell from 135 MB to 67 MB, and the tokenizer alone from 59 ms to 46 ms.
- 2026-10-29, small DOM: a `u32` `NodeId`, children as a linked list (no `Vec` for each node), and interned names. The tree builder peak fell from 67 MB to 37 MB, and the DOM kept from 67 MB to 30 MB (167 bytes to 74 bytes for each node). The spare room of the node list is given back at the end of `parse`.
- 2026-10-29, borrowed tokens: names, texts and attribute values are slices of the page, and the tokenizer scans runs of plain text without looking at each letter. The tokenizer went from 46 ms to 20 ms and the tree builder from 78 ms to 57 ms. Memory did not change: the strings were freed at once before.

- 2026-10-29, text buffer: all texts, attribute values, comments and doctypes live in one `String` of the `Document`, and all attributes in one `Vec`. A node holds a `Span` (8 bytes) and no `String`. A node is 36 bytes. The DOM kept fell from 30 MB to 18 MB, and the RSS of the DOM from 43 MB to 21 MB (112 bytes to 55 bytes for each node). The tree builder went from 57 ms to 45 ms.

What the numbers say:

- The CSS parser makes a tree of rules that borrows the text of the style sheet. For 100,000 rules it peaks at 28.6 MB (a rule is 48 bytes, a declaration 48 bytes, and each rule has a `Vec`). The tree is a temporary result: topics 3 to 5 turn it into a compact form and drop it. To verify: if the peak matters on a big style sheet, the parser can hand out each rule at once, like the HTML tokenizer hands out tokens.

- The arena is 36 bytes for each node (14 MB for 400,006 nodes). The rest is the attribute list (12 bytes for each attribute) and the text buffer.
- Real memory (RSS) for the DOM of this page: 21 MB, which is 55 bytes for each node. Chrome's DOM is about 22 MB for 200,005 elements (rough, see below), and that page has no text nodes between the tags. The two are close.
- The text buffer and the attribute list never shrink while the page lives. When JavaScript changes the DOM (module 9), we may need to clean them up.

## HTML in pieces

`encoding::Decoder` and `tree::Parser` read a page that arrives in pieces. The caller feeds each piece of bytes to the decoder, and the text to the parser. The tree is the same as for the whole page, wherever the pieces are cut.

- The tokenizer starts again from a `Mark`: the place after the last complete token. A token that is cut stays in the parser's buffer, and the tokenizer reads it again with the next piece. Only the unread part is kept.
- A very long cut token (a 10 MB script) would be read again for each piece. The parser waits until its buffer is twice as big as at the last stop, so the work is linear.
- The decoder chooses the encoding from the first 1024 bytes. The guess (UTF-8 or windows-1252) looks at these bytes only. `decode` uses the same code, so a page that is valid UTF-8 at the start and broken later is now UTF-8. Before, it was windows-1252.
- Check: a test cuts each test page at every one place, at every two places, and into single letters. The fuzzer (`fuzz html`) cuts each input at random places and compares the trees: 500,000 inputs, no difference.
- Speed: the tree build of the 5.2 MB page is the same as before (49.5 ms against 47.4 ms, inside the noise). `decode` went from 3.9 ms and 15.6 MB to 2.1 ms and 10.4 MB, because it no longer checks the page twice.

### `bare` reads the page in pieces

`bare` calls `get_stream`, reads the body in pieces of 64 KB, and sends each piece to `Decoder` and then to `Parser` (or to the screen, for `--source`). The page is never all in memory, as bytes or as text. The output is the same as before for the tree, `--source` and `--styles` (checked on a page with windows-1252, `\r\n`, a script and 20,000 divs).

| Case (`cargo run --release -p bare-stress -- scale net-page`) | In memory | In pieces |
| --- | --- | --- |
| 5 MB page, peak | 34.1 MB | 24.4 MB |
| 50 MB page, peak | 460.7 MB | 361.0 MB |

The 100 MB that the pieces save at 50 MB are the bytes of the body and the text of the page. The tree is the rest. The time for these cases is 437 ms and 190 ms for 5 MB. Part of the gap is noise: only the peak is a result.

Time to first output, for a server that sends 3,000 bytes in 3 seconds (`--source`): 2.9 s before, 1.1 s now. The first output waits for 1024 bytes, because the decoder needs them to choose the encoding. The tree is not ready earlier: the DOM is only printed at the end. The gain is for the work that comes later (style sheets and images can start while the page is read: module 4 and later).

## Profile of the tree build (module 2)

`cargo run --release -p bare-bench -- parse` builds the 5.2 MB page 40 times. Use it with `perf record -g` (build with `CARGO_PROFILE_RELEASE_DEBUG=true` to get names). Result: 46.7 ms for each build, 400,006 nodes.

| Share | Where |
| --- | --- |
| 23% | `Tokens::next` (the state machine). A third of it is the one `match` on the state: the CPU cannot predict that jump well |
| 12% | `Tokens::emit_tag` (mostly the copy of the 64-byte token into the queue) |
| 8% | `Document::intern` (the name table, FNV hash) |
| 7% | `Builder::in_body` |
| 5% | `Tokens::attr_name` |
| under 4% each | all the rest: `read`, `body_start`, `process`, `finish_value`, `insert_text`, `close_implied`, `start_tag`, `insert_element` |

No part has more than a quarter of the time. Two changes were tried and thrown away:

- Two slots instead of the `VecDeque` for the ready tokens: 7% slower (2.01 s against 1.87 s for 40 builds). Moving an `Option<Token>` costs more than the deque.
- `#[inline(always)]` on `peek`, `read`, `back`, `Pending::slice` and `start_tag`: 1.72 to 1.85 s against 1.87 s. This is inside the noise (about 0.1 s).

What is left needs a change of the design, for example to join the states of a tag (name, attributes, values) so that the `match` runs less often. It could give 5 to 10%. It is not done: the code would be harder to read, and the page already builds at 110 ns for each node.

## Profile of the CSS parser and the cascade (module 3)

`bare-bench -- css` parses the 6.9 MB sheet and its values 20 times. `bare-bench -- style` computes the styles of 200,000 elements 20 times. Build with `CARGO_PROFILE_RELEASE_DEBUG=true` for `perf record -g`.

- Sheet parser: `Parser::declaration_list` 17% (half of it is `peek`), the tokenizer (`next_spanned`, `name`, `numeric`, `skip_comments`, `peek3`) about 35%, `parse_declaration` 6%, number parsing (`f32::from_str`) 5%. No part is the cause of the cost: 6.9 MB take 92 ms, 75 MB/s, with 2.5 million tokens.
- Cascade: `RuleSet::compute` is 37% of itself, and nearly all of it is the loop over the candidate rules of an element (the ancestor filter test, then `matches`). In the bench, the rightmost part of a rule is `.eN`, and 13 classes share 5,000 rules, so each span has about 385 candidates. 100,000 spans make 38 million tests in 107 ms: 2.8 ns for each test. The loop is already tight. A faster result needs fewer candidates (a better index key), not a faster test, and a real page has far fewer rules for one class.

Nothing was changed for these two profiles.

## Chrome (Blink), the same page shape

Method: one fresh Chrome on a hyprcage screen for each page, with its own profile. The page is a local file. The script forces layout, waits 4 seconds, and reads the RSS of the page's renderer process (`ps`). One run for each page, so treat the numbers as rough.

| Page                                                            | Renderer RSS |
| --------------------------------------------------------------- | ------------ |
| Blank page                                                      | 152 MB       |
| 200,005 elements, no style sheet (browser defaults only)        | 504 MB       |
| Same, with 107 class rules                                      | 543 MB       |
| Same rules, but `display: none` on the body (no style, no layout) | 174 MB       |

What the numbers say (hypothesis: the split cannot be separated more finely with RSS):

- Rules add about 39 MB (543 minus 504) for 200,000 elements. That is about 195 bytes for each element.
- Style and layout together add about 330 MB (504 minus 174), about 1.6 KB for each element. Layout objects are most of it.
- The DOM alone adds about 22 MB (174 minus 152). It is a rough figure, because the baseline varies between runs.
- The first reading of the no-style page was 1.06 GB. A repeat gave 504 MB. We do not know why the first one was high. We use the repeat.
- Chrome counts RSS here. The `bare` table counts heap bytes. Do not compare them as equal.

## Load tests

Run `cargo run --release -p bare-stress -- scale [filter]`. It runs about 130 cases with bad input, each at 1x, 10x and 100x the size, each in its own process. It flags time or memory that grows faster than the input (exponent over 1.5), a crash, a run over 20 seconds, and more than 2 GB of memory. `-- fuzz <html|css|url> [iterations] [seed]` breaks good seeds at random and looks for panics, hangs (5 seconds), memory blowups, a broken DOM tree, and a URL that is not stable. A failing input goes to `crash-*.txt`. The parsers of DNS and HTTP are private, so their fuzz tests are in the `net` crate (`random_packets_never_crash`, `random_replies_never_crash`).

Run on 2026-10-29, archMachine. 450,000 fuzz inputs (3 targets, 3 seeds) found one panic. The scaling run found these problems, and all are fixed unless the list says otherwise:

| Where | Input | Before | After |
| --- | --- | --- | --- |
| CSS selector | `:nth-child(2n` + a wide character | panic (a slice inside a character) | no panic |
| CSS selector | 50,000 classes on one element, and 50,000 rules on them | 13 s (the class list was read for each test) | 70 ms, 500,000 take 0.9 s. `Document::has_class` makes a set of the classes of an element when its `class` attribute is 128 bytes or more. Memory: the names of that list, only for such an element |
| CSS selector | `:nth-child` and `:nth-of-type` on 50,000 siblings | 5 s (the siblings were counted for each element), 5,000 took 50 ms | 4.7 ms, 500,000 take 46 ms. `Document::sibling_place` makes all the places of a parent in one pass, the first time. Memory: 8 bytes for each node, only for a page that uses `:nth-*` (10 MB at 500,000 siblings) |
| HTML tokenizer | one tag with 20,000 attributes | 311 ms (a scan for each name), 200,000 took over 20 s | 5 ms, 200,000 take 84 ms |
| HTML tree builder | 20,000 nested `<i class=cN>` (all different) | 1.2 s, 20 s at 80,000 | 39 ms, 200,000 take 375 ms |
| CSS custom properties | 20,000 on one element | 613 ms, 200,000 over 20 s | 14 ms, 200,000 take 133 ms |
| CSS `var()` chain of 5,000 | | 73 ms, 50,000 took 7.4 s | 3 ms, 50,000 take 32 ms |
| CSS `@layer` | 20,000 different names | 369 ms, 200,000 over 20 s | 27 ms, 200,000 take 286 ms |
| HTTP | chunked body of 10 MB | 412 ms (it read the whole reply again after each read) | 82 ms |
| HTTP | a server that sends headers for ever | ran until killed | error after 256 KB |
| HTTP | a body that never ends | 2 GB, then killed | error after 128 MB |
| HTTP | cache of 5,000 replies of 1 MB | 2 GB, then killed | 69 MB (the cache empties at 64 MB) |

Not fixed:

- The cap of 256 on the Noah's Ark scan changes the tree only for a page with more than 256 formatting elements open between two identical ones.
- The three timeouts of `net-silent-server`, `net-drip-server` and `net-huge-content-length` are the wait of the client (30 s for each read, 120 s for the whole reply), not a bug. They are longer than the 20 s of the test.
- The 100x runs of `css-rules`, `css-selector-list` and `html-decode-bytes` stop at 2 GB because the input has 130 MB to 500 MB. Memory grows in line with the input.

### Layout (module 4)

`bare-stress scale boxes-` walks the box tree of pages of 20,000 to 2,000,000 items (2026-11-03). All cases are linear (time exponent 0.9 to 1.2):

| Case | n | ms |
| --- | ---: | ---: |
| `<div>x</div>` repeated | 2,000,000 | 75 |
| 200,000 nested `<div>` | 200,000 | 4 |
| `<p>x</p>` with spaces between | 2,000,000 | 101 |
| One run of 2,000,000 inline elements | 2,000,000 | 80 |
| `display: none` blocks | 2,000,000 | 106 |
| List items | 2,000,000 | 137 |

About 40 ns for each box. The cascade costs about 600 ns for each element, so the box rules add little. They keep no memory.

`bare-stress scale text-` cuts one text into pieces and measures them (2026-11-03). Linear, and no memory at all:

| Text | Size | ms | Speed |
| --- | ---: | ---: | --- |
| `hello world ` repeated | 120 MB | 189 | 635 MB/s, was 363 ms before the two changes below |
| `a ` repeated (one piece for each byte) | 200 MB | 855 | 4.3 ns for each piece, was 1,139 ms |
| Chinese words | 100 MB | 232 | was 541 ms |
| Words with accents (`e` + U+0301) | 130 MB | 506 | the accent table is a binary search |
| `ß` in uppercase | 70 MB | 562 | a case change for each letter |

Two changes from the measurement:
- A word is scanned once. The first version scanned it to find its end, then again to check that it is ASCII.
- `width_of` tests the wide letters before the table of accents, so Chinese text skips the binary search.

`bare-stress scale block-` lays out pages of blocks (2026-11-03). Linear:

| Page | n | ms | peak MB |
| --- | ---: | ---: | ---: |
| `<div>` with a height and margins | 2,000,000 | 153 | 72 |
| Empty `<div>` with margins (all collapse through) | 2,000,000 | 149 | 72 |
| `<div>` with `calc()`, `%`, `auto` margins, `max-width` | 2,000,000 | 150 | 72 |
| `<li><div>` (a marker for each) | 2,000,000 | 444 | 288 |
| 200,000 nested `<div>` (layout stops at 400 levels) | 200,000 | 0.3 | 0 |

About 75 ns for each block. A fragment is 36 bytes: the peak is the fragment list. It started at 160 ms and 75.5 MB: the list now gets room for one fragment for each DOM node at the start, so it does not grow by copies.

`bare-stress scale inline-` lays out inline content (2026-11-03). Linear:

| Page | n | ms | peak MB | Before the fixes |
| --- | ---: | ---: | ---: | --- |
| One paragraph of 2,000,000 words, 300px wide | 2,000,000 | 61 | 14 | |
| `<b>a</b> <i>b</i>` repeated | 2,000,000 | 756 | 432 | |
| `<b>a</b>` repeated: one unit with no break | 2,000,000 | 308 | 245 | 480 ms, 983 MB |
| 200,000 nested `<span>`, one line | 200,000 | 14 | 8 | 5,431 ms: quadratic |
| 10,000 nested `<span>` over 10,000 lines | 10,000 | 1.3 | 0.4 | over the 2 GB limit |
| `a<br>` repeated | 2,000,000 | 181 | 144 | |
| Inline-blocks with two words | 2,000,000 | 843 | 216 | 1,105 ms, 298 MB |
| Block inside an inline, repeated | 2,000,000 | 539 | 216 | 645 ms, 432 MB |

Fixes from these numbers:
- Culled inline boxes: a box with nothing to paint, no edges and the font of its parent makes no fragment and no line item.
- The open boxes are a stack. The first version searched the list of boxes on the line for each end tag (quadratic).
- A line item is 20 bytes: an inline-block's fragments are in a `Box`.
- Intrinsic widths are kept only for an inline-block inside another one, and the fragment lists of inline-blocks are used again.

`bare-stress fuzz layout` lays out broken pages with random CSS and checks that the fragments make a tree and that no place is NaN. It found two bugs: a tab in `font-size: 0` (a division by 0) and lengths like `2e45px` (infinity in `f32`). Lengths now stop at 33,554,431 px and font sizes at 10,000 px, as in Chrome. After the fix: 900,000 inputs (seeds 1 to 3), no failure.

### Layout against Chrome (module 4)

Method: a test page with 33 boxes and `font-family: 'Noto Sans Mono'` on all elements, served on 127.0.0.1. Chrome on a hyprcage screen, window 800 px wide. Chrome shows a 15 px scrollbar, so its page is 785 px wide; `bare --layout --width=785` gives the same width. A script reads `getBoundingClientRect` and `getClientRects` of each element with an `id`.

Result: all 33 boxes and the 6 line pieces of the two spans over three lines have the same place and size, to 0.01 px. The only difference is the letter width: 9.6 px here and 9.6016 px in Chrome (the font gives 1229/2048 em). A span that starts after 6 letters is at 65.6 px here and 65.61 px in Chrome.

One difference of shape, not of place: Chrome gives a span that holds a block three client rects (the line before, the block, the line after). `bare` gives two span fragments and the block fragment.

### Layout of real pages against Chrome (module 4)

Method: a script downloads each page and its style sheets, takes out `<script>`, `<noscript>`, `<iframe>` and `<template>`, gives each element an `id`, and adds `* { font-family: 'Noto Sans Mono' !important }`. The pages are served on 127.0.0.1. Chrome on a hyprcage screen, 800 px window, reads the box of each element with an `id` over the DevTools port. bare runs with `--layout --width=` the page width that Chrome had (785 with a scrollbar). The union of the fragments of an element is its box. A "clean" element has no flex, grid, table, float, `position`, `overflow`, form control or image in it or above it, and no float or positioned box beside it. For clean elements only the size is compared, because a box above them can move them.

| Page | Elements in both | Same box (0.5 px) | Clean elements | Same size (0.5 px) | Why the rest differ |
| --- | --- | --- | --- | --- | --- |
| example.com | 2 | 2 | 2 | 2 | |
| info.cern.ch (no doctype) | 22 | 0 | 22 | 22 | Quirks mode: Chrome drops the top margin of the first child of `body`. With a doctype added: 22 of 22 |
| danluu.com (doctype added) | 211 | 54 | 0 | | Flex (`li { display: flex }`) |
| news.ycombinator.com (doctype added) | 329 | 0 | 0 | | Tables |
| gnu.org, free software page | 248 | 1 | 0 | | Floats, form controls |
| en.wikipedia.org, "Web browser" | 1,199 | 1 | 99 | 95 | Flex header, floats (the infobox), `::before` text in the reference list |

Bugs that this check found, both fixed with a test first:
- `x-a::before, span.mw-linkback-text { display: none }` was thrown away, because bare dropped every list with a pseudo-element. Chrome keeps it: a known pseudo-element is valid and matches nothing. The hidden back-link numbers made Wikipedia reference items one line taller.
- `<input type=hidden>` was shown. Chrome's UA style sheet has `input[type=hidden i] { display: none }`.

"No box in bare" counts are high (2,426 on Wikipedia). Most are culled inline boxes (a link with no edges and no background makes no fragment, by design), and the children of flex items.


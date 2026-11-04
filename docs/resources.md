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

`bare-stress scale floats-` (2026-11-03). Linear after the change to steps (`docs/decisions.md`, "Floats"):

| Page | n | ms | peak MB | Before the change |
| --- | ---: | ---: | ---: | --- |
| Floats of 10 by 10 that stack in rows | 200,000 | 43 | 7.3 | 20,000 took 243 ms, 200,000 timed out (20 s) |
| One tall float and a long paragraph | 2,000,000 words | 67 | 16.5 | 64 ms |
| A float and a paragraph, repeated | 200,000 | 213 | 49.5 | 20,000 took 839 ms, 200,000 timed out |
| A float and a block with `clear: both`, repeated | 200,000 | 163 | 27.9 | 20,000 took 842 ms |
| Floats in the middle of one paragraph | 200,000 | 116 | 44.0 | 20,000 took 311 ms |
| A float and a `flow-root` block, repeated | 200,000 | 171 | 27.9 | 20,000 took 771 ms |

`bare-stress fuzz layout` with floats, `clear` and `flow-root` in its words found one panic at once: a float with a negative margin box height (negative margins) made an empty range with its end before its start. Such a float now takes no room. After the fix: 900,000 inputs (seeds 1 to 3), no failure.

`bare-stress scale overflow-` (2026-11-03). All linear:

| Page | n | ms | peak MB | Note |
| --- | ---: | ---: | ---: | --- |
| `auto` boxes of 10 px that overflow, each with a paragraph | 200,000 | 204 | 45.6 | 108 ms when they fit: the second pass costs one more layout |
| One `auto` box of 400 px with a log of lines | 2,000,000 | 749 | 144 | 1,403 ms with two full passes (`docs/decisions.md`, "Overflow") |
| `auto` boxes, one inside the other, each overflowing | 400 levels (the depth limit) | 0.2 | 0.1 | the boxes inside keep their first answer |
| One `overflow-x: auto` box with a `nowrap` line | 200,000 words | 5 | 0.0 | |

`bare-stress fuzz layout` with the overflow words and a seed with nested `auto` and `scroll` boxes: 18,000 inputs (seeds 1 to 6), no failure.

`bare-stress scale position-` (2026-11-03). All linear:

| Page | n | ms | peak MB |
| --- | ---: | ---: | ---: |
| One relative block with an absolute box after each paragraph | 200,000 | 106 | 46.4 |
| A relative block with an absolute box inside, repeated | 200,000 | 150 | 61.6 |
| Relative spans in one paragraph | 200,000 | 94 | 39.6 |
| Absolute boxes that shrink to fit, each in a relative block | 200,000 | 143 | 46.4 |
| Nested relative blocks with an absolute or a `fixed` box at the bottom | 400 levels (the depth limit) | 0.2 | 1.1 |

`bare-stress fuzz layout` with the position words and a seed with relative, absolute, fixed and sticky boxes: 21,000 inputs (seeds 1 to 7) in release, 1,500 in debug (which checks for overflow of integers), no failure.

`bare-stress scale flex-` (2026-11-03). All linear. The `flex-ref-` cases are the same pages with blocks, to compare:

| Page | n | ms | peak MB | Note |
| --- | ---: | ---: | ---: | --- |
| One row with many items | 200,000 | 127 | 60.8 | blocks: 63 ms, 16 MB. The intrinsic widths are one more walk over the content |
| One row that wraps, with `gap` | 200,000 | 107 | 48.5 | |
| Rows of three stretched cards | 200,000 rows | 386 | 71.2 | 488 ms with a second layout for each card (`docs/decisions.md`, "Flex") |
| The same with a percent height inside each card | 200,000 rows | 400 | 63.2 | these cards need the second layout |
| One column with `flex: 1` items and a fixed height | 200,000 | 106 | 48.5 | |
| `flex: 1` items with 500 different `max-width`s (the freeze loop) | 200,000 | 75 | 41.3 | |
| 30 nested rows around one long text | 200,000 words | 9.3 | 0.4 | blocks: 5 ms. 79 ms and 9 MB before the cache and the list change |
| Nested rows or columns | 400 levels (the depth limit) | 0.4 | 0.8 | |

`bare-stress fuzz layout` with the flex words and a seed with wrapped rows, `order`, `auto` margins and nested columns: 21,000 inputs (seeds 1 to 7) in release, 9,000 in debug. The debug run found one panic: an item that is laid out again counted the static positions of its first layout as still waiting, but a positioned box inside it had taken them. Fixed: each item counts how many its layout left waiting. After the fix, no failure.

`bare-stress scale table-` (2026-11-03). All linear (time exponent 1.0 at 10x and 100x). Each case times the layout only. The page is parsed and styled before the clock starts.

| Page | n | ms | peak MB |
| --- | ---: | ---: | ---: |
| One table, rows of 3 cells (auto layout) | 2,000,000 rows (6,000,000 cells) | 2,971 | 1,285 |
| The same with `table-layout: fixed; width: 600px` | 2,000,000 rows | 2,484 | 1,285 |
| The same with `border-collapse: collapse` and a 1 px border on each cell | 1,000,000 rows | 1,580 | 642 |
| Rows with `rowspan=3` cells | 500,000 groups of 3 rows | 1,712 | 753 |
| Rows with percent widths | 1,000,000 rows | 1,433 | 642 |
| Rows with `colspan=1..n` (a chain of spans) | 100,000 rows | 231 | 63 |
| Many small tables of 4 cells | 200,000 tables | 490 | 96 |
| One row with many cells | 500,000 cells (the table keeps 16,384 columns) | 19 | 38 |
| Tables inside tables | 400 levels (the depth limit) | 0.6 | 2.3 |

That is about 0.5 µs for a cell in auto layout, and about 210 bytes for a cell, with the DOM and the styles. A profile of the 400,000-row page (`perf`) is flat: `table::layout` is 3.5% of the time. The rest is the tokenizer, the tree builder, the cascade and the generic block layout of the cells. So no change was made for speed: the first version was linear and each cell is laid out once.

`bare-stress fuzz layout` with the table words and a seed with captions, groups, `col`, spans, hint attributes and collapsed borders: 18,000 inputs (seeds 1 to 6) and 30,000 (seed 7) in release, no failure.

Tables against Chrome (2026-11-03): test pages of 8 to 10 tables each, `font-family: 'Noto Sans Mono'` on all elements, the window 800 px wide, `grab2.mjs` for the rectangles of Chrome. Page 1: auto, fixed and percent widths, `colspan` and `rowspan`, `table-layout: fixed`, `vertical-align`, `border-spacing`, table `height`, a floated table: 34 boxes, 34 within 0.5 px. Page 2: `thead` and `tfoot` out of order, a caption below, `colgroup` widths, `margin: auto`, hint attributes (`border`, `cellpadding`, `cellspacing`, `width`, `align`), nested tables, `inline-table`, an empty cell, loose `display: table-cell` boxes with text: 39 boxes, 39 within 0.5 px (`col` elements and a culled span are not boxes here). Page 3, collapsed borders (1 px, 2 px, 4 px, 7 px, `hidden`, spans): 22 boxes, 22 within 0.5 px. A page with a caption: within 0.03 px (the letter width, see above). An odd collapsed border is a half pixel in Chrome, so the first cell starts at 8.5; the first version of bare used a whole pixel and was 0.5 px off in 5 boxes.

A review page of 12 more tables (hint attributes, collapsed borders, `rowspan`, fixed layout, captions and groups, `vertical-align`, `inline-table`, `border-spacing`, a table in a flex row, `col width`, `nowrap`) found one bug, fixed: a cell with a `width` kept its text on one line as the most width of its column, so the column was too wide. Chrome and Firefox use the larger of the width and the longest word. After the fix, all 59 boxes of the two review pages are the same as in Chrome, to 0.5 px.

Real pages with tables built: Hacker News (doctype added, the table gets `margin: auto` because `<center>` is not built): 329 boxes, 290 within 0.5 px (88%). The other 39 are the vote arrows: `<center><a><div></div></a></center>` is a block in an inline, and bare gives it a line that is 9 px too short (not a table problem). Wikipedia, "Web browser": 362 boxes of table type (the infobox, the figures that use `display: table`, the references), 241 with the same size. The rest wait for images (a figure has the size of its image) and for flex and grid, which are not built.

### Layout against Chrome (module 4)

Method: a test page with 33 boxes and `font-family: 'Noto Sans Mono'` on all elements, served on 127.0.0.1. Chrome on a hyprcage screen, window 800 px wide. Chrome shows a 15 px scrollbar, so its page is 785 px wide; `bare --layout --width=785` gives the same width. A script reads `getBoundingClientRect` and `getClientRects` of each element with an `id`.

Result: all 33 boxes and the 6 line pieces of the two spans over three lines have the same place and size, to 0.01 px. The only difference is the letter width: 9.6 px here and 9.6016 px in Chrome (the font gives 1229/2048 em). A span that starts after 6 letters is at 65.6 px here and 65.61 px in Chrome.

One difference of shape, not of place: Chrome gives a span that holds a block three client rects (the line before, the block, the line after). `bare` gives two span fragments and the block fragment.

Floats (2026-11-03): a second test page has 18 cases: text around left and right floats, floats in the middle of a line that fit and that do not, floats that stack and go down, `clear`, `flow-root` beside a float (auto and fixed width), a BFC that contains its floats, a float in a block with margins, a shrink-to-fit float, `text-align: center` next to a float, a right float wider than its block, percent widths. All 51 boxes and all 39 text lines are the same as in Chrome, to 0.01 px. The check found two bugs, both fixed: a right float wider than its block kept its left edge (Chrome keeps its right edge), and a `flow-root` with a fixed width used its over-constrained right margin in the fit check, so it went below the float.

Overflow (2026-11-03): a third test page has 16 cases: `hidden` and `clip` with margins inside, `scroll`, `overflow-x: scroll`, `auto` with `max-height`, a `nowrap` line, end padding, nested `auto` boxes, a scroll container beside a float, an inline-block that scrolls, shrink-to-fit floats with `scroll` and `auto`, lists and floats inside `auto` boxes, `box-sizing: border-box`. All 31 boxes are the same as in Chrome, to 0.5 px. Chrome's scrollbars are 15 px on this screen, also under device emulation.

Position (2026-11-03): a fourth test page has 15 cases: relative blocks and inline boxes, absolute boxes with each mix of offsets, `auto` margins that center, percents with `box-sizing: border-box`, `float` on an absolute box, absolute inside absolute, a relative inline-block as containing block, the window as containing block, `fixed`, `sticky`, and `bottom: 0` in a block with `height: auto`. All 32 boxes are the same as in Chrome, to 0.5 px. A fifth page found two bugs, both fixed: an absolute box in a centered line did not follow `text-align`, and a relative inline box was not a containing block. It also found an older inline bug, fixed after: a space at the end of a line inside a span with padding or a background still counted in the width of the span. The start or end of an inline box after the space no longer stops the space from hanging. Now all 14 boxes of that page are the same as in Chrome.

Flex (2026-11-03): two test pages have 65 cases: grow, shrink, `flex: 1 1 0` with and without `min-width: 0`, all `justify-content` values (also with negative free space), auto margins, `align-items` and `align-self`, `baseline` with padding, `wrap` and `wrap-reverse`, `align-content` with a height, `row-reverse` and `column-reverse`, columns that grow, shrink and wrap, `gap` in px and %, `order`, anonymous items, min and max sizes in the freeze loop, `flex-basis` in px and %, `box-sizing`, `inline-flex` and a floated flex container that shrink to fit, a percent height in a stretched item, a nested column, an absolute child, a list. All 217 boxes are the same as in Chrome, to 0.5 px. The second page found two bugs, both fixed: a column that wraps stretched its items to the container and not to their line, and a column item with a `height` took that height as its automatic minimum, not its content. The real pages of the next section do not change: Wikipedia uses flex, but its flex boxes are not "clean".

`bare-stress scale grid-` (2026-11-03). All linear (time exponent 1.0 at 10x and 100x). Each case is a whole page: parse, cascade and layout.

| Page | n | ms | peak MB |
| --- | ---: | ---: | ---: |
| `repeat(4, 1fr)`, a gap, 3 words in each item | 500,000 items | 330 | 142 |
| `repeat(auto-fill, minmax(80px, 1fr))` | 500,000 | 270 | 216 |
| `repeat(6, auto)` (all items measured) | 500,000 | 286 | 142 |
| `dense` with items that span 3 of 5 columns | 500,000 | 483 | 373 |
| The same, sparse | 500,000 | 453 | 373 |
| `grid-auto-flow: column` with 4 rows | 500,000 | 316 | 216 |
| One item in each explicit row (`grid-row: n`, up to the limit of 10,000 tracks) | 500,000 | 326 | 142 |
| Items that span 2 of 4 `auto` columns | 500,000 | 908 | 503 |
| Many grids of 4 items | 200,000 grids | 648 | 72 |
| Grids in grids, 3 levels | 200,000 | 1,718 | 165 |
| Rows of 3 stretched cards, with a percent height inside | 500,000 | 605 | 309 |
| `inline-grid` in a line of text | 200,000 | 617 | 48 |
| Absolute children | 200,000 | 147 | 73 |

The explicit rows first took 752 ms and 1,059 MB. The cause: rows past 10,000 go in the last row, and there each item took the next column with no limit. The columns went past 10,000, and the bit set of used cells grew to about 490,000 columns for each of the 10,000 rows. Now an item past the last track goes in the last track (standard 9), and the same page takes 326 ms and 142 MB.

`bare-stress fuzz layout` with the grid words and a seed with areas, `repeat(auto-fit, minmax())`, `fit-content()`, `order`, `dense`, an absolute child and an inline grid: 18,000 inputs (seeds 1 to 6) after the merge with flex and tables, and 90,000 more (seeds 11 to 13). 0 failures.

Grid against Chrome (2026-11-03, window 800 by 800, Noto Sans Mono on both): two pages with 21 grids and 109 boxes. Page 1: fixed, `fr`, `auto` and `minmax()` tracks, `auto-fill`, `auto-fit`, `dense`, areas, `fit-content()`, `min-content`, `max-content`, `space-evenly`, `space-between`, an `inline-grid`, `grid-auto-flow: column` with `center`: 58 of 58 boxes within 0.5 px. Page 2: `auto-fit` with `justify-content: center`, percent gaps, `auto` margins, a `min-height`, `grid-auto-rows: minmax()`, a stretched child with a percent height, negative lines and implicit tracks before the grid, `order`, spans over `auto` tracks, `overflow: hidden` items, `minmax(0, 1fr)`, `place-content`, borders, a run of text and an absolute child: 49 of 51 within 0.5 px. The 2 others are a percent `row-gap` in a grid with no height (known gap, 2.2 px).

### Layout of real pages against Chrome (module 4)

Method: a script downloads each page and its style sheets, takes out `<script>`, `<noscript>`, `<iframe>` and `<template>`, gives each element an `id`, and adds `* { font-family: 'Noto Sans Mono' !important }`. The pages are served on 127.0.0.1. Chrome on a hyprcage screen, 800 px window, reads the box of each element with an `id` over the DevTools port. bare runs with `--layout --width=` the page width that Chrome had (785 with a scrollbar). The union of the fragments of an element is its box. A "clean" element has no flex, grid, table, float, `position`, `overflow`, form control or image in it or above it, and no float or positioned box beside it. For clean elements only the size is compared, because a box above them can move them.

| Page | Elements in both | Same box (0.5 px) | Clean elements | Same size (0.5 px) | Why the rest differ |
| --- | --- | --- | --- | --- | --- |
| example.com | 2 | 2 | 2 | 2 | |
| info.cern.ch (no doctype) | 22 | 0 | 22 | 22 | Quirks mode: Chrome drops the top margin of the first child of `body`. With a doctype added: 22 of 22 |
| danluu.com (doctype added) | 211 | 54 | 0 | | Flex (`li { display: flex }`) |
| news.ycombinator.com (doctype added) | 329 | 0 | 0 | | Tables (see below for the result with tables built) |
| gnu.org, free software page | 248 | 1 | 0 | | Floats, form controls |
| en.wikipedia.org, "Web browser" | 1,199 | 1 | 99 | 95 | Flex header, floats (the infobox), `::before` text in the reference list |

With floats built (topic 6), floats and `flow-root` count as clean: Wikipedia then has 133 clean elements, and 121 have the same size as in Chrome. The rest are next to floats whose own size is wrong: the infobox is a floated `<table>` (tables are not built), and a floated `<figure>` holds an `<img>` with no size (images are not built), so it shrinks to fit its long caption and takes the whole line.

Bugs that this check found, both fixed with a test first:
- `x-a::before, span.mw-linkback-text { display: none }` was thrown away, because bare dropped every list with a pseudo-element. Chrome keeps it: a known pseudo-element is valid and matches nothing. The hidden back-link numbers made Wikipedia reference items one line taller.
- `<input type=hidden>` was shown. Chrome's UA style sheet has `input[type=hidden i] { display: none }`.

"No box in bare" counts are high (2,426 on Wikipedia). Most are culled inline boxes (a link with no edges and no background makes no fragment, by design), and the children of flex items.


## Staging network inspection

2026-11-04, archMachine. This measures the Python inspection tool, not the browser engine. Each local parse ran in a fresh process with a 20 s parent timeout. The input is `tools/fixtures/staging/login.html` repeated N times. Parse time excludes input construction. Peak RSS includes the interpreter, input, parsed results and output preparation. Python reports RSS in KiB on Linux.

| Repeated pages | Input bytes | References | Parse seconds | Peak RSS KiB |
| --- | --- | --- | --- | --- |
| 100 | 868,700 | 2,400 | 0.0381 | 23,528 |
| 1,000 | 8,687,000 | 24,000 | 0.3563 | 46,148 |
| 10,000 | 86,870,000 | 240,000 | 3.4485 | 275,668 |

The parser stores one dictionary per reference. This tool is for inspection only. These numbers do not establish Rust resource-loader performance. Keep the existing Rust streaming approach for the engine.

Parent review added a decoded-body cap and a direct failure check for the browser command. Offline tests cover compressed-body limits, request limits across retries, connection reuse and omission of `Accept-Encoding`. A single verified HTTPS request through the inspection tool returned `/login` with status 200, 8,687 identity bytes and the saved SHA-256. `bare` still rejects HTTPS. The expected failing check is `sh tools/staging_https_check.sh`.

## HTTPS URL prerequisite

2026-11-04, archMachine, release. Scheme-aware URL parsing and HTTPS plaintext guards add no measured heap cost in the delegated before/after runs. The delegate measured the existing 1x/10x/100x URL cases before and after the change. All runs had zero flags. Individual times varied by about 10%; these runs do not establish a speedup.

Parent verification: `cargo test --workspace --quiet` passed (484 tests). `cargo run --release -q -p bare-stress -- scale url-` had zero flags. At the largest join-dots size (2,000,000), time was 71.1 ms and peak counted heap was 110.7 MB. Peak counted heap is not RSS. No optimization was added without a measured cause.

The delegate ran URL fuzzing with 200,000 inputs (seed 1), 1,000,000 (seed 7), and 3,000,000 (seed 42), with zero failures. Parent review checked the guards before cache lookup and socket reuse. Real TLS and HTTPS throughput remain untested.

## TLS hash foundations

2026-11-04, archMachine, release. `bare-tls` implements streaming SHA-256, HMAC-SHA256 and HKDF-SHA256. The portable compressor keeps a 16-word message schedule and unrolls eight rounds at a time. The x86-64 path uses SHA instructions after a runtime check of every required CPU feature. HMAC keeps keyed hash states so HKDF does not hash its pad blocks again for each output block.

Parent ran `sh crates/tls/examples/run_scales.sh`. Each case is a separate process with a 512 MiB virtual-memory limit and a 300 s timeout. The driver reuses a fixed buffer. Times below are elapsed times, not CPU times.

| Case | Scale | Work | Seconds | Peak RSS KiB | Allocations in timed loop |
| --- | --- | --- | --- | --- | --- |
| SHA-256 | 1x | 64 MiB | 0.040 | 2,392 | 0 |
| SHA-256 | 10x | 640 MiB | 0.400 | 2,376 | 0 |
| SHA-256 | 100x | 6,400 MiB | 3.874 | 2,392 | 0 |
| HMAC-SHA256 | 1x | 64 MiB | 0.040 | 2,384 | 0 |
| HMAC-SHA256 | 10x | 640 MiB | 0.406 | 2,348 | 0 |
| HMAC-SHA256 | 100x | 6,400 MiB | 3.646 | 2,368 | 0 |
| HKDF-Expand | 1x | 2,000 expansions of 8,160 bytes | 0.097 | 2,376 | 0 |
| HKDF-Expand | 10x | 20,000 expansions of 8,160 bytes | 0.885 | 2,380 | 0 |
| HKDF-Expand | 100x | 200,000 expansions of 8,160 bytes | 7.906 | 2,388 | 0 |
| Small HMAC | 100x | 10,000,000 messages of 64 bytes | 2.667 | 2,364 | 1 (64-byte driver buffer) |

Parent also built the driver with `--features force-portable` and hashed 6,400 MiB. It took 18.211 s, used 2,380 KiB peak RSS and allocated nothing in the loop. Its digest matched the accelerated run. A separate accelerated 6,400 MiB run took 3.284 s in the timed hash loop. Linux child CPU accounting reported 3.3297 s user and 0.0113 s system time, including process startup. The portable path remains available without SHA instructions. These are single runs under concurrent machine load; timings do not establish a precise speedup ratio.

Checks: all 17 crypto tests passed in debug and release, with and without `force-portable`. The tests include standard vectors, padding and key-length edges, incremental updates, and HKDF output limits. The merged workspace passed 501 tests before this PR. `cargo clippy -p bare-tls --all-targets --all-features -- -D warnings` passed. No non-x86 hardware was tested. No TLS handshake or certificate check exists in this crate yet.

## AES record protection

2026-11-04, archMachine, release. AES-128-GCM uses caller-owned buffers. Open authenticates the ciphertext before decrypting. Failed authentication leaves the buffer unchanged. Portable AES computes its S-box without secret-indexed tables. The x86-64 path requires AES, PCLMULQDQ, SSSE3 and SSE4.1, checked at run time. `force-portable` also disables this path.

Parent built `tools/crypto/aes_gcm_load.rs` with `rustc --edition=2024 -C opt-level=3`. Each scale ran in a new process with a 512 MiB address-space limit and a 45 s timeout. Records are 16 KiB with 5-byte AAD. Every record uses a distinct sequence-derived nonce, completes seal/open comparison, and contributes to the output. Every 4,096th record includes a rejected modification. Linux clock ticks were checked as 100 per second for the driver's CPU accounting.

| Backend | Scale | Input MiB | Seal MB/s | Open MB/s | Wall seconds | CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Accelerated | 1x | 256 | 5,004.0 | 4,999.6 | 0.12 | 0.11 | 2,464 | 0 |
| Accelerated | 10x | 2,560 | 4,989.0 | 4,982.1 | 1.18 | 1.17 | 2,464 | 0 |
| Accelerated | 100x | 25,600 | 5,023.0 | 5,021.0 | 11.71 | 11.67 | 2,492 | 0 |
| Portable | 1x | 1 | 12.7 | 12.6 | 0.17 | 0.16 | 2,472 | 0 |
| Portable | 10x | 10 | 12.6 | 12.6 | 1.66 | 1.65 | 2,464 | 0 |
| Portable | 100x | 100 | 12.6 | 12.6 | 16.61 | 16.54 | 2,456 | 0 |

The delegated implementation measured the GHASH stage as the accelerated bottleneck. Three-product Karatsuba multiplication and eight-block aggregated reduction improved it. Portable measurements identified the S-box as the cost; a shorter inverse chain and linear squaring improved it. Parent verified the final loads above. Parent did not reproduce the earlier implementation's timings. Portable throughput remains much lower; no non-x86 machine was tested.

Parent checks: 510 workspace tests passed. All 26 crypto tests pass in debug and release with default and forced-portable builds. NIST CAVP vectors, bit/byte changes, empty inputs, length boundaries, and backend agreement pass. Clippy passes with default and all features. These tests do not establish constant-time machine code or complete TLS correctness. TLS framing, per-key usage limits and sequence exhaustion remain separate work.

## SHA 384 certificate hashes

2026-11-04, archMachine, release. Streaming SHA-384 uses a 16-word schedule ring and fixed-size state and buffer. It copies partial blocks only. Full blocks are read from the caller's slice. This hash is needed for the observed certificate signatures; the chosen TLS cipher still uses SHA-256 for its key schedule.

Parent built `tools/crypto/sha384_load.rs` with `rustc --edition=2024 -C opt-level=3`. Each scale ran in a separate process with a 512 MiB address-space limit and a 30 s timeout. The input uses 16,421-byte chunks with a changing chunk counter, so partial-block handling runs repeatedly. Parent independently regenerated and hashed each complete stream with Python hashlib. All digests matched, including the stream larger than 4 GiB.

| Scale | Bytes | MB/s | Wall seconds | CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- | --- |
| 1x | 67,108,864 | 547.1 | 0.12 | 0.12 | 2,348 | 0 |
| 10x | 671,088,640 | 549.5 | 1.22 | 1.21 | 2,404 | 0 |
| 100x | 6,710,886,400 | 548.3 | 12.24 | 12.19 | 2,400 | 0 |

The delegate tried a separate unroll for rounds 0 to 15. Its measured gain was about 1%, within noise, so the change was removed. The final implementation keeps the simpler loop. Parent did not reproduce that discarded variant. No CPU-specific path was added.

Checks: all 517 workspace tests passed. All 33 crypto tests passed in release with default and forced-portable builds. SHA-384 adds seven tests for standard vectors, padding boundaries, deterministic oracle inputs, incremental splits, cloning and length arithmetic. Clippy passed with all features. No 32-bit, big-endian or non-x86 hardware was tested.

## X25519 and OS random input

2026-11-04, archMachine, release. X25519 uses five 51-bit limbs and fixed ladder steps. A dedicated square uses 15 products instead of 25. Addition and subtraction defer carries within documented limb bounds. The delegated before/after runs identified field arithmetic as the main cost and reported lower per-call time after these changes. Parent verified the final implementation with the runs below; the earlier variant was not measured again.

Parent built `tools/crypto/x25519_load.rs` with `rustc --edition=2024 -C opt-level=3`. Each load process had a 512 MiB address-space limit and a 45 s timeout. Each exchange computes two public keys and two shared secrets. Both secrets must agree. The next iteration uses the preceding results. Input data is deterministic test data, not real credentials.

| Scale | Exchanges | X25519 calls | Wall seconds | CPU seconds | Microseconds per call | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1x | 2,000 | 8,000 | 0.267 | 0.26 | 33.38 | 2,492 | 0 |
| 10x | 20,000 | 80,000 | 2.688 | 2.68 | 33.60 | 2,508 | 0 |
| 100x | 200,000 | 800,000 | 26.463 | 26.34 | 33.08 | 2,504 | 0 |

Parent reran `oracle 100000` against the installed Python cryptography/OpenSSL implementation. Results: 97,332 identical outputs, 2,668 all-zero outputs rejected by OpenSSL, and no differences. The RFC 7748 million-iteration check also passed separately in release (32.50 s). It remains ignored in normal workspace tests because of its duration.

Linux random input uses `getrandom` with flags 0. This waits for initialized kernel random input and needs no device file. Reference: [Linux getrandom manual](https://man7.org/linux/man-pages/man2/getrandom.2.html). Short reads and EINTR pass through `Read::read_exact`; errors clear the full output. Other Unix platforms use `/dev/urandom`; unsupported platforms return an error. No source failure falls back to another generator.

Parent ran `random 1`, `random 10`, and `random 100` in separate processes with a 512 MiB limit and a 30 s timeout. Each call fills 32 bytes, which are consumed without logging their contents.

| Scale | Calls | Wall seconds | CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- |
| 1x | 100,000 | 0.007 | 0.00 | 2,572 | 0 |
| 10x | 1,000,000 | 0.069 | 0.06 | 2,580 | 0 |
| 100x | 10,000,000 | 0.699 | 0.69 | 2,528 | 0 |

CPU accounting has 10 ms resolution on this machine; a printed 0.00 s is not zero CPU use. Tests passed: 532 workspace tests, 48 crypto tests in release with default and forced-portable builds, and the separate million-iteration vector. Clippy passes with all features. A regression test covers clearing partial random output on EOF and hard errors. No TLS handshake, non-Linux source, unavailable-getrandom failure, cross-target assembly or timing harness was tested. Secret stack copies are not comprehensively erased.

## DER and certificate parsing

2026-11-04, archMachine, release. `der` reads bounded TLVs as borrowed slices. `x509` parses the fields needed by the staging chain without copying certificate data or building a recursive tree. Certificate size is capped at 64 KiB and extension count at 64. Duplicate extensions are checked against a fixed stack array of prior OID slices. The delegated profile identified rescanning encoded extensions as the main cost; the array preserves the check and avoids reparsing. Parent did not reproduce the discarded implementation.

Parent review added regression tests and fixes for forbidden end-of-contents values and canonical ordering of multi-valued name sets. Both tests failed before their fixes. OpenSSL expectations were regenerated and did not change. Parent ran the load driver with a 512 MiB address-space limit and a 30 s timeout per process. Each round parses five public certificates and five mutated copies. An accepted mutation is syntactically parseable; it is not a trusted certificate.

| Scale | Parser calls | Input MiB | Wall seconds | CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- | --- |
| 1x | 200,000 | 150 | 0.07 | 0.06 | 2,444 | 0 |
| 10x | 2,000,000 | 1,509 | 0.67 | 0.66 | 2,428 | 0 |
| 100x | 20,000,000 | 15,094 | 6.77 | 6.75 | 2,444 | 0 |

Reproduce with `rustc --edition=2024 -C opt-level=3 tools/crypto/x509_load.rs -o <out>` and `<out> 1|10|100`. All 563 workspace tests pass. All 79 normal crypto tests pass in release with default and forced-portable builds. Clippy passes with all features. Prefix truncation, malformed lengths/extensions, random mutations, borrow boundaries and fixed-date cases are covered. The long X25519 test remains a separate ignored test.

This measures parsing only. X509 name values and some unused GeneralName forms remain opaque. The caller must enforce critical-extension handling, leaf/issuer policy, hostname, validity, path length and signature verification. RSA verification and compressed EC keys are unsupported. Only the five fixture certificates and their mutations were tested; no other target hardware was tested.

## ECDSA verification

2026-11-04, archMachine, release. P-256 and P-384 share fixed-size Montgomery arithmetic. Verification validates the public key and signature scalar ranges. It rejects an infinite result. High-s signatures remain valid. All arithmetic is variable-time and must only handle public verification inputs.

The delegated implementation replaced a basic joint scalar multiplication with signed-window digits, a shared generator table, mixed affine additions and a projective comparison that avoids a final inversion. The delegate measured lower verification time after these changes and kept the shared field code after several unsuccessful arithmetic optimizations. Parent reviewed the implementation and reproduced the final checks; the discarded variants were not measured again.

Parent generated 10,000 fresh oracle cases with `oracle_gen.py 1000`, then interleaved the two curves so every scale includes both. Cases include valid/high-s signatures and changed digests, signatures and keys. The verifier compared every answer with OpenSSL's answer. Each process had a 512 MiB address-space limit and a 60 s timeout. Parent used base 2,000 rather than the driver's default 20,000. The delegate separately ran 2,000,000 verifications over a 10,000-case input file; that is not two million distinct random inputs.

| Scale | Verifications | P-256 accepted microseconds | P-384 accepted microseconds | Wall seconds | CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1x | 2,000 | 95.0 | 300.9 | 0.32 | 0.32 | 9,696 | 0 |
| 10x | 20,000 | 94.1 | 299.0 | 3.15 | 3.14 | 9,736 | 0 |
| 100x | 200,000 | 94.2 | 298.8 | 31.47 | 31.35 | 9,672 | 0 |

The driver holds the oracle cases before measurement. Per-curve times count accepted signatures only; invalid keys can fail early. Tables are shared through `OnceLock`. Parent added a concurrent first-initialization check using fresh tables and four threads for each curve. It verifies a single shared result and compares table contents with the curve's existing table.

Checks: 573 workspace tests pass, including 656 public signature vectors and 24 public-key vectors inside the ECDSA tests. All 89 normal crypto tests pass in release with default and forced-portable builds. Clippy passes with all features. The long X25519 check remains separate. No non-x86 target or independent exact stack-peak measurement was tested. ECDSA is not a certificate-path validator.

## TLS protocol foundations

2026-11-04, archMachine, release. The record layer owns fixed read-ahead and write buffers. Read and write keys have separate sequences. Authentication precedes plaintext access. Errors fail the affected direction. The layer enforces record and per-key usage limits. The handshake caller must enforce message order, an overall deadline and the compatibility-CCS window.

The key schedule uses typed traffic secrets and stack buffers. Transcript snapshots clone the fixed hash state. Hello encoding writes into the caller's buffer. Parent review added a failing test for invalid DNS labels in SNI, then tightened label validation. This preserves the existing hello vectors.

Parent ran each load scale in a fresh process with a 512 MiB address-space cap and a 60 s timeout. Hello work includes two X25519 operations, hello parsing, transcript hashing and key derivation. It excludes sockets and certificate validation. Record memory runs seal and open 16 KiB records. Socket runs use local Unix sockets and two threads.

| Case | 1x | 10x | 100x | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- |
| Hello/schedule, 1k / 10k / 100k iterations | 79.2 us/iteration | 80.8 us/iteration | 78.7 us/iteration | 2,484–2,500 | 0 |
| Record memory, 64 / 640 / 6,400 MiB | 3.97 GB/s seal, 3.91 GB/s open | 3.95 / 3.90 GB/s | 3.99 / 3.94 GB/s | 2,764–2,772 | 0 |
| Record socket, same sizes | 2.91 GB/s | 2.92 GB/s | 2.87 GB/s | 2,948–3,004 | 19 total setup allocations |
| PEM, 200 / 2k / 20k bundle decodes | 928 MB/s | 901 MB/s | 874 MB/s | 2,740–2,784 | 7 per decode |

The 100x record memory run took 3.39 s wall and 3.38 s CPU. The socket run took 2.34 s wall and 4.48 s CPU across two threads. The 100x PEM run processed 3.7 GB in 4.24 s wall and 4.21 s CPU. These final measurements do not establish a before/after gain. The delegated PEM implementation replaced a per-character state machine with a quartet decode fast path. Parent reproduced the final output, not the discarded implementation.

Trust loading caps the input file at 4 MiB, the certificate count at 1,024 and each decoded certificate at 16 KiB. The decoder reserves storage from the file size; this is an overall file bound, not a 16 KiB allocation bound. Limit cases reject excess input. Parent checked all 121 OS roots and the independently loaded ISRG Root X2 fixture. Their hashes match. Hostname matching makes no heap allocations; a parent 1M-call run measured 37 ns/call for four SANs and 449 ns/call for a 1,000-SAN miss.

Reproduce with the standalone `hello_load.rs`, `record_load.rs` and `trust_load.rs` drivers under `tools/crypto`. Parent's record oracle passed 3,000 Rust-to-OpenSSL records and 2,999 OpenSSL-to-Rust records before close_notify. All 636 workspace tests pass. All 152 normal crypto tests pass in release with default and forced-portable builds. The ignored OS-store check passes separately. Clippy passes with all features. The long X25519 vector was checked in the earlier primitive integration.

No connected TLS client, certificate-path validation, staging HTTPS, HRR, resumption, KeyUpdate message handling, wildcard hostname or non-Linux trust source was tested in this batch. Unknown critical certificate extensions remain the validator's responsibility. This code remains unaudited.

## HTTP request methods and origin state

2026-11-04, archMachine, release. Requests borrow header and body slices. Header validation runs before cache access, DNS or socket use. The wire head reserves capacity once. Bodies up to 16 KiB join that buffer; larger bodies write directly from the caller's slice. POST has no automatic retry. Cache and idle-pool identities include the scheme.

Parent review found a stale-cache bug after successful POST. A regression test failed because a later GET returned the old cached body. The fix invalidates the target when the response head has a 2xx or 3xx status. Tests cover buffered and streamed replies, redirects and failed POST. Reference: [RFC 9111 section 4.4](https://www.rfc-editor.org/rfc/rfc9111.html#section-4.4). Existing authenticated-request bypass checks remain separate from successful-POST invalidation.

The load server consumes request bodies. It scans only new head bytes, so a large request head does not cause repeated full-buffer scans. Parent changed the new load cases to require successful replies and expected body bytes. Request errors now fail these cases instead of producing misleading timings.

Parent ran `bare-stress scale net-post`. Each case and scale uses a fresh process. The harness limits each run to 20 s and 2 GiB of counted heap. Peak heap excludes the prebuilt input. It is not RSS.

| Case | Input at 1x / 10x / 100x | Milliseconds at each scale | Extra peak heap MB |
| --- | --- | --- | --- |
| POST body | 1 / 10 / 100 MB | 0.5 / 2.7 / 34.1 | 0.1 / 0.1 / 0.1 |
| Small POST, keep-alive | 1k / 10k / 100k requests | 12.1 / 120.7 / 1190.6 | 0.1 / 0.1 / 0.1 |
| POST to 303 to GET | 500 / 5k / 50k chains | 11.9 / 115.7 / 1239.1 | 0.1 / 0.1 / 0.1 |
| Custom headers | 10k / 100k / 1M headers | 1.6 / 14.4 / 135.6 | 1.5 / 13.2 / 125.4 |

All scales pass with zero growth flags. Parent did not reproduce the delegate's before/after GET comparison. Final measurements show linear time and constant extra heap for large bodies. No additional optimization was justified by these runs.

Checks: 661 workspace tests pass, including 81 network tests. Header fuzzing runs inside the network suite. The separate URL fuzz run uses 300,000 inputs and seed 5. Strict clippy reports pre-existing findings in DNS, HTTP response helpers, HTML and Unicode. The net/URL targets pass strict clippy with only the three pre-existing net lint classes allowed. Parent fixed the new test's single-match lint.

No HTTPS or staging requests were tested. Full-duplex early-response handling while a server keeps the connection open and stops reading a large upload is unsupported. The existing write timeout bounds that failure. POST never retries after a write. A cross-origin 307 or 308 preserves the body; credential headers are stripped. The HTTP cache has no header variants, so custom-header requests bypass it.

## Certificate path validation

2026-11-04, archMachine, release. The validator checks SAN hostname, validity, CA and key-use rules, path length and each used certificate signature. A path ends only at an independently loaded store entry. The peer's unused RSA cross-sign does not need verification. Unsupported name and policy constraints fail closed. Search uses at most eight peer certificates, 64 KiB of peer DER, four intermediates and eight signature checks. The search allocates no heap memory.

A subject-only DER prefilter avoids full parsing of unrelated roots. A matching root still needs a full parse, policy checks and signature verification. The delegate measured the discarded full-parse scan as slower on a 1,024-anchor store. Parent reproduced the final code, not that comparison. The remaining measured cost is ECDSA verification.

Parent ran the driver at base 100, with 150 anchors and the real anchor last. Each process had a 512 MiB address-space cap and a 40 s timeout. The delegate separately ran base 2,000 up to 200,000 checks per mode. Those checks reuse a bounded fixture set; they are not distinct random chains.

| Case | Calls 1x / 10x / 100x | Microseconds per call | 100x wall / CPU seconds | Peak RSS KiB | Loop allocations |
| --- | --- | --- | --- | --- | --- |
| Valid staging chain | 100 / 1k / 10k | 1422 / 1239 / 1163 | 11.63 / 11.42 | 2,648–2,652 | 0 |
| Invalid chain, host or time | 100 / 1k / 10k | 291 / 272 / 257 | 2.57 / 2.55 | 2,620–2,652 | 0 |
| CertificateVerify rejection | 100 / 1k / 10k | 68 / 67 / 67 | 0.67 / 0.67 | 2,608–2,628 | 0 |

Every expected acceptance and rejection matched. Parent tightened the CertificateVerify load driver to fail if its rejection workload ever accepts. This mode measures rejection only. Positive CertificateVerify tests use OpenSSL-signed synthetic keys and exact TLS server context bytes.

Reproduce: build `tools/crypto/verify_load.rs` with `rustc --edition=2024 -C opt-level=3`, then run `<out> valid|invalid|cv 1|10|100 100 150 last`. Tests cover 34 synthetic chains, impostor roots, peer-only roots, time edges, limits and changed signed/signature bytes. The stored OpenSSL oracle accepts 12 cases this conservative implementation rejects; tests list each difference explicitly. Parent added and ran an ignored environment check against the real OS root bundle and the public staging chain at a fixed fixture time. No live TLS socket is involved.

All 674 workspace tests pass. All 13 normal validator tests pass in release and forced-portable builds. Crypto clippy passes with all features. No revocation, certificate transparency, RSA, Ed25519, wildcard names, DN normalization, leaf-as-anchor path, non-x86 host or concurrent path validation was tested. Anchor validity and CA rules are deliberately strict. These limits can reject otherwise valid sites. HTTPS remains guarded until the connected client and transport are integrated.

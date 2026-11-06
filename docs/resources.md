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

## Script and stylesheet resources

2026-11-04, archMachine, release. `resources::discover` retains document order, script type and async/defer metadata. Repeated script occurrences share a fetch target. Inline source borrows the DOM text when it has one text child. The loader uses streamed HTTP replies, so it does not fill the HTTP cache with another body copy. UTF-8 bytes become the retained string without another full copy. Other encodings can allocate during decoding.

Defaults: 4,096 resource references, 128 distinct fetches, 8 MiB per resource, 32 MiB retained decoded text and 60 s across downloads. The byte caps apply to decoded text; temporary buffers and the DOM are additional memory. Tests accept exact caps and reject one extra byte. Blocking DNS, connect and read calls can exceed an elapsed-time checkpoint by one call. The first resolvable base href is used. This is a documented departure from using the first base href regardless of validity. Missing or unsupported MIME types and non-identity encodings fail closed.

Parent reproduced the release harness with a 512 MiB address-space cap and a 30 s timeout per process. Each row uses the median of three loads. The harness raises the default caps for its large cases and asserts request counts, retained bytes and successful results. The server runs in the measured process. Heap is the extra counted live heap during loading; RSS includes parsing and server buffers.

| Case | 1x / 10x / 100x | Load milliseconds | Extra peak heap KiB | RSS HWM KiB |
| --- | --- | --- | --- | --- |
| Small resources | 50 / 500 / 5,000 files, 2 KiB each | 1.20 / 8.42 / 89.99 | 111 / 1,078 / 10,707 | 4,024 / 4,868 / 16,644 |
| Declared length | 1 / 10 / 100 MiB | 0.74 / 6.21 / 59.10 | 1,025 / 10,241 / 102,401 | 4,920 / 14,136 / 105,836 |
| Chunked | 1 / 10 / 100 MiB | 2.10 / 9.23 / 90.63 | 2,057 / 16,393 / 131,081 | 5,608 / 21,956 / 105,980 |
| Duplicate references | 1k / 10k / 100k items, ten URLs | 0.51 / 0.69 / 0.57 | 3 / 3 / 3 | 4,084 / 5,364 / 21,812 |

Duplicate discovery took 0.13 / 0.76 / 7.83 ms. The implementation caches repeated raw URL references before resolution. The delegate measured this change as faster than resolving every occurrence. Parent reproduced the final implementation only. Chunked bodies grow geometrically, so counted capacity can exceed retained text. Parent made no further runtime optimization: these final runs show linear discovery and resource-size costs.

Reproduce: `cargo run --release -p bare --example resources_load -- small|large|largechunk|dups 1|10|100 3`. All 699 workspace tests pass, including 25 resource tests. App clippy passes with its one pre-existing needless-borrow lint allowed. Parent clarified that the HTTPS failure test only checks error propagation; it does not prove that no socket opens. The delegate reported one earlier failed workspace run whose output was discarded. Its cause is unknown; three later delegated runs and the parent run passed.

The saved staging login fixture gives 24 items and 17 targets. These are 15 async external scripts, six inline scripts, one skipped nomodule script and two stylesheets. Tests reject the known 404 HTML chunk shape before using its body. This does not cover runtime-generated chunks, execute scripts or establish a live staging fetch. Module scripts retain metadata only. Inline styles, preloads, media evaluation, parallel fetches, resource redirects and quoted semicolons in MIME parameters were not tested. HTTPS remains the next integration gate.

## Verified HTTPS transport and staging gate

2026-11-04, archMachine, release. `client::connect` uses the real clock, OS random and the bounded path validator. It checks the peer before client Finished and application access. HTTP holds the whole TLS stream in its pool, so read-ahead and record sequences survive reuse. OS roots load once per client on first HTTPS use. An explicit independent-root constructor supports private-CA tests. No public certificate bypass exists.

Parent review found two client issues. On baseline `b626b35`, a buffered transport could wait for the server before flushing ClientHello. A flood of valid session tickets could keep one read from returning to HTTP's elapsed-time checks. Both regression tests fail on that baseline and pass after the fixes. The handshake now flushes its flights. Flush errors fail the stream. One application read processes at most 64 records before returning data or failing. This conservative limit can reject peers that fragment control messages across many records.

Parent ran all load scales in fresh processes with a 512 MiB address-space cap and a 40 s timeout. Each case uses independent synthetic roots and loopback OpenSSL/Python TLS peers. Private test keys stay in private RAM directories and are deleted. Every body, count and checksum is checked. CPU numbers cover the client process; wall time includes the peer. These are final measurements, not a speed-up comparison.

| Case | 1x / 10x / 100x | Wall seconds | Client CPU seconds | Peak RSS KiB |
| --- | --- | --- | --- | --- |
| New TLS connection per GET | 20 / 200 / 2,000 | 0.020 / 0.194 / 1.972 | 0.013 / 0.123 / 1.274 | 3,864–3,900 |
| Reused GET | 200 / 2k / 20k | 0.027 / 0.237 / 2.256 | 0.005 / 0.036 / 0.356 | 3,836–3,856 |
| Reused 1 KiB POST | 100 / 1k / 10k | 0.013 / 0.149 / 1.382 | 0.002 / 0.021 / 0.202 | 3,836–3,856 |
| GET through 302 | 100 / 1k / 10k | 0.020 / 0.219 / 2.100 | 0.002 / 0.032 / 0.296 | 3,836–3,856 |
| Streamed Content-Length | 16 / 160 / 1,600 MiB | 0.017 / 0.159 / 1.589 | 0.015 / 0.148 / 1.491 | 3,904–3,968 |
| Streamed chunked | same sizes | 0.017 / 0.171 / 1.809 | 0.016 / 0.161 / 1.690 | 3,964–3,984 |
| Borrowed 8 MiB POST body | 1 / 10 / 100 requests | 0.043 / 0.387 / 3.835 | 0.006 / 0.051 / 0.458 | 12,008–12,036 |

New-connection GETs make about 46 allocations per request; reused GETs make about 24. The 100x Content-Length run makes 677 allocations across 1,600 MiB. The POST driver's 8 MiB source buffer explains its higher RSS. The delegated Nagle fix belongs to the Python peer harness. Parent did not measure a client optimization gain and made no further throughput changes.

Reproduce: `cargo run --release -p bare-net --example https_load -- hs|ka|big|post 1|10|100`. The 748 workspace tests pass, including 80 net unit tests, 12 HTTP-over-TLS integration tests, 195 normal TLS unit tests and eight real-validator OpenSSL tests. Release net/TLS tests also pass with `bare-tls/force-portable`. Crypto clippy passes with all features. Tests cover independent roots, wrong root/host/time, peer-only roots, split reads, ALPN, credential stripping, POST no-retry, truncation, altered handshake data and Finished. Four long/environment/fixture-print tests stay ignored in the normal TLS suite.

Public staging checks use `bare` itself. `sh tools/staging_https_check.sh` prints `ok /login sha256 0bc6a3d32df8098c47b0de2d683ebcbeb5653d67717c39aa67c2bf2bbc12f773`. `bare --resources` reports 17 successful downloads and 575,754 bytes. Parent separately parsed the fixture's script/stylesheet references, ran `bare --source` for each distinct target and compared SHA-256 after removing only the CLI's final newline. All 17 match `tools/fixtures/staging/manifest-2026-11-04.json`. No live load or authenticated POST was run.

Remaining limits: no HRR, TLS 1.2, resumption, KeyUpdate handling, wildcard SANs or IP-literal HTTPS. Key limits fail closed. Dropping a stream does not send close_notify. Early replies during a failed large TLS upload can surface as an error; POST is still never retried. Blocking application I/O uses socket timeouts. No non-Linux host or long-term leak measurement was tested. Runtime-generated chunks, JavaScript execution and login E2E remain open.

## JavaScript lexical foundation

2026-11-04, archMachine, release. `bare-js::lex` borrows source text and emits 12-byte tokens with byte spans, newline metadata and escape facts. Source size is capped at 64 MiB. The caller supplies division, regexp or template-continuation goals. Decoders produce UTF-16 code units only on request. The lexer keeps no parser stack or per-token String.

Parent review reproduced a panic when a public string token ended inside a UTF-8 character. The regression failed before the fix. Decoders now check source ranges and literal delimiters before slicing. Numeric decoding checks radix prefixes and digits. The same regression covers huge offsets, template boundaries and short numeric spans. Tokens carry no source identity; callers must pair them with the correct source.

Parent compared all 15 cached scripts with installed Acorn 8.15.0. Classes, byte spans and newline flags match. The oracle supplies the lexical goals. Every literal decoder is exercised, but the bundle comparison does not compare all decoded values or establish a real parser's goal selection. Separate unit tests check decoded values and errors. No target script was executed.

Parent ran `tools/js_lex_load.sh 3`, then repeated the small-token and repeated-literal cases with five samples after other checks finished. Each scale is a fresh process. The harness caps address space at 1 GiB and runtime at 120 s. Counts, spans, checksums and malformed-input results are asserted. The table uses later five-sample medians for small/repeat and initial three-sample medians for other cases.

| Case | Bytes at 1x / 10x / 100x | Median milliseconds | 100x peak RSS KiB | Timed-loop allocations |
| --- | --- | --- | --- | --- |
| Small tokens | about 0.5 / 5 / 50 MiB | 3.85 / 40.74 / 405.74 | 53,600 | 0 |
| Repeated literals | same sizes | 2.88 / 15.40 / 157.91 | 53,636 | 0 |
| Comments | same sizes | 0.27 / 2.40 / 26.03 | 53,616 | 0 |
| Strings, scan | same sizes | 0.48 / 4.39 / 53.61 | 53,652 | 0 |
| Strings, decode | same sizes | 1.24 / 12.34 / 125.18 | 53,632 | 0 |
| Malformed cases | 116,501 / 1,165,081 / 11,650,841 short sources plus one large bad source | 5.80 / 59.25 / 603.87 | 104,824 | 0 |

The 100x small case has 28,973,805 tokens; the repeated case has 11,121,250. Sources are held before timing. The decode buffer is reused. The malformed case keeps additional constructed input, which explains its higher RSS. Initial concurrent 100x medians were 742 ms for small and 431 ms for repeat; later medians were 406 and 158 ms without a code change. This variation does not establish an optimization gain.

The delegate measured gains from jumping between block-comment stars and widening ASCII directly to UTF-16. Parent reproduced the final code only. Punctuator, identifier-loop and inline experiments gave no reliable gain in the delegated runs and were reverted. No further optimization was justified by the parent checks.

Checks: 774 workspace tests pass; 26 lexer tests pass in debug and release; crate clippy is clean. Reproduce target comparisons with `ACORN=file:///path/to/acorn.mjs tools/js_lex_check.sh <bundle-dir>`. The small checked-in sample is under `tools/fixtures/js`. The oracle parses source and never executes it.

Not implemented or tested: parser integration, executable code, browser execution, non-ASCII identifiers, regexp pattern grammar, regexp v flag and BigInt values. The lexer reports legacy octal and bad template escapes; the parser must enforce strict-mode and tagged-template rules. Scanning the target files does not establish React/Next.js compatibility or login E2E.

## Managed values and heap

2026-11-04, archMachine, release. Values hold typed generation handles. Strings use UTF-16 units and weak interning. Object storage keeps descriptors, prototype links, dense array elements and sparse properties. The collector marks an explicit root set with a work list, then sweeps unreachable cycles. It does not recurse through the Rust stack. Freed generations cannot become valid again; exhausted slots retire.

Parent review added four failing regressions. Invalid constructor limits could panic. Index 2^32-1 could overflow array length. Stale keys could escape through key conversion. Creating the property hash index could exceed the account byte cap. The fixes return typed errors and charge index creation before mutation. `Heap::new` is now fallible. An allocation-count check also failed because an oversized rejected string was copied before its byte check. Borrowed UTF-16 and UTF-8 strings that exceed the total budget now reject with zero allocations in that check. The live payload cap is not a cap on all temporary buffers or allocator capacity.

Parent built `tools/js/heap_load.rs` with release rustc and ran each case and scale in a fresh process, with a 1 GiB address-space cap and 45 s timeout. Each case checks semantics, counts and checksums. The results below are one parent sample per scale; they do not establish a speed-up over the delegate's discarded implementations.

| Workload | 1x / 10x / 100x | Wall seconds | Main measured cost | Peak RSS KiB |
| --- | --- | --- | --- | --- |
| Lookup/update | 2M / 20M / 200M operations | 0.057 / 0.558 / 5.585 | 28 / 28 / 28 ns/op | 2,648 / 2,688 / 2,648 |
| Small objects | 10k / 100k / 1M objects | 0.004 / 0.039 / 0.404 | 94 / 96 / 101 ns per build/read operation | 5,544 / 33,568 / 312,172 |
| Many properties | 10k / 100k / 1M keys | 0.004 / 0.048 / 0.915 | define 79 / 81 / 146 ns/key; read 19 / 28 / 73 | 5,360 / 30,876 / 294,348 |
| String interning | 2k / 20k / 200k distinct strings | 0.015 / 0.303 / 6.466 | hit 28 / 58 / 126 ns | 3,068 / 7,148 / 49,264 |
| Fixed-live-set churn | 20 / 200 / 2,000 rounds, 10k temporary objects per round | 0.060 / 0.644 / 6.866 | 299 / 322 / 343 ns per allocation/collection operation | 6,624 / 6,572 / 6,768 |

At 100x, small objects account for 248 bytes each. After collection their live payload returns to baseline, but RSS remains 98,140 KiB because slot metadata and allocator capacity remain. Slots must preserve generations; keeping every freed Object-sized slot is a remaining representation cost, not a requirement. The churn case keeps 1,001 live objects, 11,001 object slots and 256,094 accounted bytes every round. Its RSS grows 224 KiB after round one. Average collection pause is 343 us and maximum is 2,035 us in the 100x parent run. Larger property and string working sets have higher per-operation costs; parent did not isolate their cause.

The delegate measured faster property-key hashing and four-unit string hashing. Parent reproduced final results only. String-key validation stayed in place after a small measured removal gain. The current fast hashes are not DoS-resistant. Heap storage bounds do not bound hash-collision work; evaluator execution limits remain a later gate.

Reproduce: `rustc --edition=2024 -C opt-level=3 tools/js/heap_load.rs -o <out>`, then `<out> run lookup|small|bigprops|strings|churn 1|10|100`. `<out> caps` checks rejected-string allocations. Parent ran the Node oracle with seed 17 and 20,000 conversion cases: 20,000 number-to-string, 20,000 string-to-number, 19 other conversions, 361 equality cases and 84,011 operation-result lines matched. Only small generated programs ran in Node; no target bundle ran.

Checks: 810 workspace tests pass on the final rerun. All 62 assembled JS tests pass in debug and release; crate clippy is clean. The first workspace run failed on an existing HTTPS same-origin redirect timeout. That test then passed alone and all 12 HTTPS tests passed in ten consecutive suite runs. The cause remains unknown; these repeats do not prove it is fixed. Parent did not change HTTP behavior in this heap integration.

Remaining limits: no function objects, accessor calls, object ToPrimitive, Proxy, Symbol.for, BigInt, for-in enumeration or automatic GC. Cross-heap handles are forbidden by contract and not detected. All managed handles held in Rust locals must be rooted before collection. Byte accounting excludes allocator slack, and individual string conversion can use temporary storage. No browser execution, closure lifetime, multi-heap isolation, hard allocator-OOM recovery or login E2E was tested.


## HTTPS harness races

The original same-origin HTTPS redirect timeout was not reproduced. Its cause remains unknown. The delegate observed two other failures under parallel suite load: another listener could pass a TCP readiness probe, and closing OpenSSL stdin before killing it could send close_notify during a hard-reset test. The harness now matches the listening socket inode to the child file descriptors through Linux /proc, retries failed starts, and kills before closing stdin. Startup failures kill and reap the child and drain its bounded stderr. Timeout diagnostics include the caller, child state, stderr and client result. Client formatting stops after 300 characters; stderr storage stops at 4 KiB while the reader continues draining.

The parent regression formatted 100,000 characters before truncation on the child baseline and failed. It passes with bounded formatting. Tests cover rejection of an occupied port and all four timeout diagnostic fields. The final parent HTTPS suite passed 40 times, with eight suite processes at once: 600 tests, zero failures, 10.97 seconds. Each suite had a 60-second timeout. These repetitions do not establish that the original timeout is fixed.

The delegate reported 368 suite runs, exceeding its 200-run cap. Its total elapsed time was estimated, not measured. The parent used the explicit 40-run cap above. No runtime HTTP or TLS behavior changed. Other OpenSSL versions, non-Linux systems and live staging were not tested in this change.

Final checks: 813 workspace tests passed. The final 15-test HTTPS suite passed after lint-only cleanup. Harness clippy has no findings; the three existing net library warnings remain.


## JSON parsing

`bare_js::json::{parse,parse_utf8}` reads JSON into the existing managed heap. It uses an explicit frame stack, data descriptors and caller-supplied object and array prototypes. Null prototypes are the API default. Duplicate keys retain their first position and last value. `__proto__` is an own data property. Strings preserve UTF-16 units, including lone surrogates. Number grammar is strict; negative zero, overflow and subnormal values are retained. Long number tokens use 800 significant digits and a sticky digit before the float parser to avoid a reproduced huge-mantissa result error.

Defaults: 128 Mi units of source, 1,024 open containers, 16 Mi values and 128 Mi decoded text units. UTF-8 input uses its byte length for the source check before conversion. Depth is clamped to 1,048,576 even if the caller raises the limit. Source, depth, node and text failures are distinct from syntax and heap errors. Error offsets count UTF-16 units except invalid UTF-8, which counts bytes. Heap handles from another heap remain forbidden by the existing heap contract.

Parent review found that the UTF-8 entry converted input before validating prototype handles. The allocation-counter regression failed with three allocations for a rejected stale handle. The fixed entry rejects that handle with zero allocations. The parser never invokes setters or collects roots. Failed parses can leave unreachable allocations until the caller collects with its own roots; successful results must also be rooted before collection.

Parent checked 20,759 generated inputs against Node with seed 17: 15,363 accepted, 5,396 rejected and zero mismatches. The comparison covers acceptance, number bits, UTF-16 strings, own-property order and data descriptors. It runs generated texts only. All 75 assembled JS tests pass, and crate clippy is clean. The workspace run passed 826 tests.

The parent ran all five workloads at 1x, 10x and 100x in separate release processes with a 1 GiB address-space cap and 45-second timeout. All checksums passed. Values below are one sample per scale on archMachine; RSS includes the generated source and verification walk. These measurements do not establish a parser speed-up.

| Workload | Values at 1x / 10x / 100x | Parse seconds | Peak RSS KiB |
| --- | --- | --- | --- |
| Records | 45,001 / 450,001 / 4,500,001 | 0.004 / 0.043 / 0.483 | 8,004 / 57,080 / 548,084 |
| Numbers | 100,001 / 1,000,001 / 10,000,001 | 0.007 / 0.070 / 0.685 | 7,296 / 49,052 / 386,744 |
| Strings | 5,001 / 50,001 / 500,001 | 0.001 / 0.013 / 0.165 | 5,304 / 33,596 / 336,688 |
| Wide object and array | 25,003 / 250,003 / 2,500,003 | 0.001 / 0.014 / 0.184 | 4,144 / 17,516 / 171,220 |
| Deep arrays and objects | 2,003 / 20,003 / 200,003 | 0.000 / 0.003 / 0.030 | 3,164 / 9,272 / 72,444 |

The first parent 10-million-number run aborted under the 1 GiB address-space cap. The harness materialized every array key and pending value during verification. Replacing that with an array cursor let the same workload pass under the same cap. The parser was unchanged. The final harness uses the 1 GiB cap by default. Parsing allocates about 0.56 times per record value, once per distinct string and 38 times in the whole 10-million-number case. Existing heap property storage accounts for most allocations.

Adversarial checks passed under 1 GiB and 45 seconds: a 100-million-opening-bracket flood stopped at depth 1,024; an override stopped at the hard depth cap; node, decoded-text and heap-byte limits fired; long-number tokens and one million duplicate keys completed. Peak RSS for the whole cap process was 504,628 KiB. Truncation checks did not panic. Reproduce with `rustc --edition=2024 -C opt-level=3 tools/js/json_load.rs -o <out>`, then `<out> preflight`, `<out> caps`, `<out> all` or `<out> oracle <dir> 17 20000`.

Not tested: reviver callbacks, stringify, evaluator integration, target bundle execution, browser behavior, Windows, macOS, hard allocator OOM recovery or exhaustive float rounding. Default caps bound parser work and storage; they do not recover from an allocator abort.


## Regexp matching

`bare_js::regexp` compiles UTF-16 patterns into a bounded backtracking instruction list. Matching is iterative and returns UTF-16 spans for the whole match and captures. Supported flags are g, i, m, s and y. The caller supplies lastIndex; sticky matching uses it exactly, other matching searches forward from it. The global flag is metadata only. A later wrapper must coerce and update lastIndex, construct result arrays and advance empty matches in String operations.

Unsupported flags v and d, lookbehind, named groups and quantified lookahead return explicit errors. Non-Unicode Annex B escapes, backreferences, positive and negative lookahead are included. Case folding uses Rust Unicode data. The Node comparison checks all 65,536 code units and 1,144 multi-member classes. Three known Unicode-version differences remain at U+A7CF, U+A7D3 and U+A7D5; no other folding differences were found.

Compile caps: 65,536 pattern units, group depth 128, 131,072 instructions, 4,096 captures and about four million class-folding units. Default execution caps: 50 million work units, 16 Mi input units and four million backtrack frames. A limit error is distinct from no match. Cancellation is checked at entry and periodically during work. Register initialization is charged in bulk; scans, comparisons, resets and stack traversal also consume budget. Heap allocator failure is not recovered.

Parent review found two budget bypasses. With a zero budget, searching for a in bbbb returned no match; a 1,000-unit backreference also completed outside the budget. Both regressions fail on child baseline 28911aa and pass after the fixes. Short calls now check an already-set cancellation flag. The Rust-string entry also stops conversion after the pattern cap: rejecting an 8 MiB input allocates 131,074 bytes, against 39,146,856 cumulative allocated bytes on the baseline. This measures conversion allocations, not source storage or RSS.

All 57 public literals compile. Parent compared them with Node on 9,006 controlled inputs; every literal has positive and negative probes. Seeded generated corpora 11, 12 and 13 contain 20,244, 20,245 and 20,244 cases. One case initially reached the new 50-million-work budget; it returned no match like Node with a 500-million-work semantic-check budget. The oracle harness now uses that larger budget; production defaults are unchanged. Generated cases exercise backreferences and lookahead beyond the target inventory. The committed 846-case oracle fixture also passes. No target bundle executes in Node.

Parent ran release loads in separate processes under 1 GiB and 120 seconds per mode and scale. All Node-derived span checksums and expected error counts passed. The figures below are one sample per scale on archMachine; they do not establish a parent-measured speed-up.

| Workload | Passes at 1x / 10x / 100x | Wall milliseconds | Peak RSS KiB at 100x |
| --- | --- | --- | --- |
| Short matches | 20k / 200k / 2M | 37.8 / 373.9 / 3,736.8 | 2,460 |
| Captures | 5k / 50k / 500k | 17.3 / 174.6 / 1,708.6 | 2,516 |
| Near miss | 1 / 10 / 100 | 12.7 / 128.7 / 1,293.0 | 2,472 |
| Budget failures | 100 / 1k / 10k | 55.9 / 558.2 / 5,369.1 | 2,796 |
| Compile | 200 / 2k / 20k | 4.2 / 38.3 / 370.8 | 2,536 |

At 100x the short case returns 34 million matches, with about one allocation per returned span vector. Near misses and budget failures allocate no memory per execution after scratch setup. The largest retained scratch is 196,644 bytes. Near misses still backtrack quadratically within the explicit work budget. The delegate's first-unit and anchor skips remain; parent did not reproduce the discarded pre-optimization version.

Reproduce with `tools/js/regexp_check.sh <existing-output-dir>` and `tools/js/regexp_load.sh <existing-output-dir>`. The check script requires Node's pathological-pattern probe to exit with timeout status 124; another failure is not counted as expected. It saves the canonicalization output before checking it, so a producer failure cannot hide in a shell pipeline.

Not tested: JS RegExp/String wrappers, dynamic patterns constructed by the target, browser execution, Unicode-mode syntax, newer-Unicode folding agreement or non-Linux targets. This component does not establish full ECMA-262 compatibility.

Final checks: 840 workspace tests and all 89 assembled JS tests in debug and release passed. Crate clippy is clean. The full regexp check script passed with zero mismatches in all three generated corpora and the required Node timeout status.


## JavaScript parser

`bare_js::parse::parse_script` returns an arena AST that borrows the source. Typed 32-bit IDs refer to nodes and lists. Literal decoding is on demand. The parser chooses division, regexp and template lexical goals. It preserves parentheses, optional chains, directives, strictness, function kinds and binding patterns. Statement parsing is recursive descent; binary expressions use precedence parsing. Early-error checks cover declarations, labels, strict mode, super, private names and function/class rules in the tested corpus.

Limits default to 16 MiB of source, 16 Mi nodes plus list entries, 1 Mi entries per list and depth 256. Parent review found that callers could remove the recursion bound with max_depth=u32::MAX. The new regression accepted 500 nested parentheses on the child baseline; the fixed parser returns a depth-limit error. Depth now has a hard ceiling of 256. At that ceiling the recursive implementation requires an embedding stack of at least 8 MiB in debug or 2 MiB in release. Smaller stacks are unsupported; a depth counter alone cannot protect an arbitrarily small host stack.

Parent compared all 2,087 committed cases against Acorn 8.15: 910 rejected by both, 31 explicitly unsupported and normalized trees with spans equal for accepted cases. The four excluded Acorn divergences also fail Node's independent syntax check. All 15 cached staging bundles match the normalized Acorn trees byte for byte with parser-selected lexical goals. They total 469,525 source bytes and 128,506 nodes. One release pass took 8,264 us and 5,172 KiB peak RSS. This establishes parsing, not execution or browser API coverage. The oracle script saves each producer's output before filtering it, so a producer crash cannot hide in a pipeline.

Parent ran five workloads at 1x, 10x and 100x under 1 GiB of address space and 120 seconds per process. Valid runs assert node counts and iterative tree-walk checksums. Each timing is the median of five parses in that process; other work ran concurrently. The figures are final measurements, not a before/after optimization claim.

| Workload | Source bytes at 1x / 10x / 100x | Median microseconds | Peak RSS KiB |
| --- | --- | --- | --- |
| Statements | 41,780 / 457,780 / 4,977,780 | 1,315 / 15,869 / 175,621 | 3,212 / 10,408 / 78,384 |
| Flat chains | 118,899 / 1,288,899 / 13,888,899 | 2,047 / 20,316 / 144,791 | 3,700 / 13,288 / 111,564 |
| Function and class bodies | 54,960 / 557,560 / 5,655,560 | 1,792 / 22,324 / 100,292 | 3,284 / 10,600 / 81,980 |

Malformed workloads check two exact errors at each scale. Deep workloads check four shapes up to 500,000 nested input elements, each returning a depth-limit error on a 2 MiB release thread. Their 100x source totals 12,500,002 bytes and peak RSS is 22,208 KiB. The 100x valid cases retain 1.2M statement nodes, 4M chain nodes and 1.52M body nodes. Allocations per parse are 140, 26 and 40,183 respectively. Tree bytes exclude arena capacity and scope scratch space. Cover-grammar conversion can leave unreachable expression nodes in the arenas; Stats includes them.

Checks: all 116 assembled JS tests passed in debug and release, including all cached bundles through BARE_JS_BUNDLES. Crate clippy is clean. The full workspace run passed 867 tests. Reproduce with `ACORN=<acorn.mjs-url> tools/js/parse_check.sh <bundle-dir>` and `tools/js/parse_load.sh <output-file>`. The latter now uses the parent-tested 1 GiB cap.

Remaining gates: no scope-reference resolution, interpreter or browser execution. Generators, modules, import(), static class blocks, non-ASCII identifiers and HTML-like comments are unsupported. Regexp flags are scanned but pattern grammar is not validated by the parser; REGEXP_PATTERN_VALIDATED remains false even though the standalone matcher now exists. No test262 run or near-16-MiB source-boundary load was done. Full language conformance and arbitrary embedding stack sizes are not claimed.


## Heap payload capacity

The heap now keeps each handle's generation and dense position in stable 8-byte slot metadata. Live payloads and backlinks occupy separate dense vectors. Sweep drops dead values in allocation order, compacts survivors and updates their metadata. Handles retain their slot and generation; reclaimed slots increment their generation, and exhausted generations stay retired. The string interner is rebuilt from the moved payloads. Stats now reports object payload capacity, metadata capacity and total slab-vector capacity.

After sweep, trimming frees payload, backlink and mark capacity only if at least 1 MiB can be released. It keeps room for twice the live count or the live count plus allocations since the previous sweep, whichever is larger. Steady churn therefore keeps capacity. A first collection after a burst can retain its capacity; a later collection without more allocation releases it. Metadata, free lists and the gray work list remain allocated. Property buffers in live objects are not compacted.

Parent compared baseline 3bbfaf7 with the new representation in 18 interleaved release processes: three repeats of each version at 100x for small objects, lookup and churn. Each process had a 1 GiB address-space cap and 45-second timeout. The original and new harnesses run the same workloads; the new harness adds capacity output. All checksums passed. Total elapsed time was 72.39 seconds. Other work ran concurrently; the first small-object pair was slower in both versions. Ranges below include all three samples.

| 100x measure | Baseline | Dense payloads |
| --- | --- | --- |
| Million-object peak RSS, KiB | 311,792–312,212 | 315,900–316,032 |
| RSS after live then dead collection, KiB | 98,164–98,208 | 19,676–19,720 |
| Small-object workload, ns/op | 98–156 | 107–173 |
| Dead collection, ms | 28.7–56.2 | 36.8–60.9 |
| Lookup/update, ns/op | 28.4–29.8 | 29.1–32.4 |
| Churn, ns/op | 269–289 | 278–303 |
| Average churn collection, us | 252–295 | 290–385 |
| Churn peak RSS, KiB | 6,732–6,804 | 6,912–7,136 |

The retained-memory reduction is about 80% in this burst workload. Peak RSS rises about 1.3%. The quieter small-object pairs cost about 9% more overall; all samples are shown above because the run was not isolated. The change trades CPU and peak memory for lower retained payload capacity. The million-object case keeps 1,000,001 handle slots while payload capacity falls from 1,048,576 to zero. Accounted bytes remain 248 per object in that case. Symbol accounting changes from 16 to 20 bytes per live symbol; byte limits continue to cover live payload estimates, not all allocator capacity.

Churn runs keep 1,001 live objects, 11,001 object slots and 256,094 accounted bytes throughout 20 million allocations. Payload capacity stays 16,384. RSS growth after round one is 128 KiB, against 224 KiB on the baseline. No handle revival or live-count drift occurred. Parent also ran one-million-object workloads at 0%, 1%, 10%, 50%, 90% and 100% scattered survivors; remapping, reads and slot reuse passed in each case. Small survivor fractions still retain property-buffer pages between live objects. Full-survivor workloads gain no memory reduction.

All 122 assembled JS tests pass in debug and release, crate clippy is clean, and 873 workspace tests pass. A separate mutation that omitted survivor remapping failed six tests. The Node oracle with seed 17 matched 20,000 number-to-string cases, 20,000 string-to-number cases, 19 other conversions, 361 equality cases and 84,011 operation-result lines. The six new unit tests cover moved objects and every traced edge kind, slot reuse, retirement, scattered live fractions, burst release and stable churn capacity.

Reproduce with `rustc --edition=2024 -C opt-level=3 tools/js/heap_load.rs -o <out>` and `<out> run small|lookup|churn 100`. `LIVE_PCT=10 <out> run frac 100` checks scattered survivors. Every invocation should use a separate process and the caps above. Earlier foundation tables describe the older slot representation.

Not tested by the parent in this change: other allocators/platforms, hard OOM, multiple heaps, browser/evaluator workloads, a new full big-properties or strings performance matrix, or capacity release from metadata/free/gray vectors. The delegate's wider matrix was partly noisy and is not used to claim a CPU improvement. This change does not add automatic collection or alter the explicit-root contract.


## JavaScript evaluator

`bare_js::eval::Interp` walks the parser AST and owns a managed heap. It first checks every reachable syntax node, then runs the script once. Host code can call a function value later. The tested subset includes primitive operators and conversions, lexical scope and TDZ, var and function hoisting, ordinary functions and arrows, closures, arguments, defaults/rest, object/array patterns, loops and labels, exceptions and finally, optional chains, construction and descriptor-aware getters/setters. Builtins cover a small Object, Function, Array, Error and primitive-conversion set. This is not full language or staging compatibility.

Classes, async functions, await, tagged templates, regexp/BigInt literals, super, new.target, private names, with and for-await are rejected during setup. Missing globals such as Promise, JSON and Map throw ReferenceError. Some unsupported builtin operations return Unsupported at run time, including wrapper construction, Function construction and Object(primitive). Parser-level unsupported syntax remains separate.

Defaults bound each top-level run/call to 100 million counted steps, 100 call levels, 1 Mi environments and 1 Mi tracked functions. Calls are clamped to 100 levels even if the caller raises the limit. Recursive setup and execution share a hard ceiling of 512 frames. Long flat parser trees can therefore return TraversalDepth during evaluator setup. Run on a thread with at least 8 MiB in release or 64 MiB in debug; arbitrary smaller embedding stacks are unsupported. Parent verified recursive calls and a 20,001-term flat tree stop with limits on an 8 MiB release thread.

Parent regressions found that sparse Array.forEach and Array.join loops could run without consuming steps, and that setup recursion lacked a hard bound. Those tests failed on the submitted behavior and pass with the fixes. Native traversals now charge steps; temporary join/template append growth checks string-length and byte bounds before expansion. Parent also fixed generic Array.pop ignoring non-configurable deletion and array-like indices at and above 2^32-1. Large indices become ordinary string property keys, and ToLength uses the safe-integer bound. All these limits remain distinct from catchable JavaScript exceptions. Join no longer copies its separator buffer on entry.

Collection stays explicit and never runs within an active call. Environments and function metadata live outside the heap; roots includes their bindings, this values, closures, bound arguments, builtin objects and cached keys. Interp.collect first finds reachable environments/functions, then collects the heap. Host-held results or thrown values must be passed as extra roots. Host code must not inject handles from another heap; ownership does not detect foreign-handle collisions. Tests keep a live closure after collecting more than 2,000 dead closures and validate direct heap collection with the exposed roots.

Parent matched all 202 committed tiny programs against Node vm in an empty context. Five are limit cases and three throw objects. Only generated tiny programs execute under Node; no staging bundle executes. All 141 assembled JS tests pass in debug and release. The 892-test workspace run passes and crate clippy is clean. The final unit/oracle run also passes after removing the separator copy.

Parent ran eight load cases at 1x, 10x and 100x in fresh release processes, each with a 1 GiB address-space cap and 45-second timeout. Every checksum passed. Loads ran alongside other work. Results below are final measurements, not a before/after speed claim.

| 100x workload | Run milliseconds | Peak RSS MiB | Counted steps |
| --- | --- | --- | --- |
| Loop | 8,642.8 | 3.3 | 320,000,011 |
| Function calls | 5,553.6 | 3.3 | 210,000,012 |
| Fibonacci | 219.0 | 3.5 | 12,341,484 |
| Retained closures | 1,102.5 | 535.6 | 20,500,022 |
| Objects | 2,949.6 | 93.2 | 66,006,021 |
| Synthetic webpack queue | 34.4 | 20.7 | 675,034 |
| Throws | 1,953.7 | 301.1 | 46,000,014 |
| Concatenation | 85.9 | 194.7 | 80,012 |

The load harness raises the step cap for its workload sizes. The call workload allocates 33 times in the run; it reuses argument vectors. Retaining 500,000 closures keeps 500,001 environments and about one million heap objects. Ordinary functions eagerly own prototype objects. Repeated string concatenation remains quadratic in copied/interned text; 10,000 concatenations retain about 191 MiB of accounted heap data until collection. Throws likewise accumulate until collection. These are measured remaining costs. The synthetic webpack case tests queue/callback shapes only.

Known semantic deviations: parameter and body bindings share an environment, so default parameters can see body vars and have no TDZ. Arguments is unmapped and not iterable. Primitive sloppy this is not boxed. String.prototype and function toString are absent. Array/string iteration lacks user-defined iterator protocols. Script completion uses a simplified rule rather than full UpdateEmpty. Cyclic Array.join currently ends at an execution limit instead of producing the usual empty cycle element. Global declaration descriptor checks differ from Node vm's host behavior and are not part of that oracle.

Reproduce with `tools/js/eval_check.sh` and `tools/js/eval_load.sh <output-file>`. Not tested: real staging execution, DOM/network integration, async/Promise or class behavior, mapped arguments, full iterator semantics, full UpdateEmpty, automatic collection during execution, hard allocator OOM recovery, Windows or macOS. Internal evaluator vectors and allocator slack are outside the heap payload-byte accounting.


## Long decimal conversion

Numeric literals and Number(string) previously returned infinity for a million-digit mantissa with a cancelling exponent. Node returns 1 for the reproduced input. JSON already had a long-number reduction. All three now use the same private number helper: ordinary tokens take Rust's float parser; long tokens retain 800 significant digits, a sticky digit and an adjusted bounded exponent. Caller-specific grammar stays separate. The lexer strips separators; string conversion handles signs and whitespace; JSON remains strict.

Parent review found a malformed long public Token could panic in exponent reduction. The added regression failed on eed7959 and passes after long-input grammar validation. Long conversion now reads UTF-16 units and separator-filtered bytes directly. Parent also caught the submitted JSON path copying the whole token before reduction. Its allocation-counter test failed with 1,002,061 allocated bytes for one million digits. Final literal, string, JSON and separated-literal paths each allocate 2,050 bytes of conversion scratch on that input, excluding source storage. Short paths preserve their existing behavior.

The Node oracle with seed 17 supplies 630 exact-bit comparisons: 201 literals, 328 strings and 101 JSON numbers. All match, including normal/subnormal halfway points, overflow, signs, separators and cancelling exponents. The independent JSON oracle matches 3,759 generated texts at seed 19. The heap oracle matches 20,000 number-to-string, 20,000 string-to-number, 19 other conversion and 361 equality cases plus 84,036 operation-result lines. Malformed-token tests check that panics and junk acceptance do not occur. All 146 assembled JS tests pass in debug/release, 897 workspace tests pass and crate clippy is clean.

Parent ran three long-number shapes (cancellation, halfway tail and leading zeros) through three paths at 1x, 10x and 100x. Each invocation has a 1 GiB address-space cap and 45-second timeout. Sizes are one, ten and one hundred million digits. The three lexer 100x cases correctly stop at its 64 MiB source cap; they are cap checks, not completed conversions. Every other result matches the independently specified float bits. Figures below are one parent sample per scale on archMachine and include input conversion in the harness.

| Cancellation path | Wall seconds at 1x / 10x / 100x | Peak RSS KiB at 100x |
| --- | --- | --- |
| Literal | 0.002 / 0.019 / source cap | not converted |
| Number(string) | 0.003 / 0.032 / 0.276 | 295,036 |
| JSON | 0.004 / 0.036 / 0.335 | 294,948 |

The final short-token sample was 16.7 ns per literal with no allocation and 41.9 ns per string conversion with one existing short-string allocation. Parent did not repeat a before/after short-token comparison, so these figures establish no speed-up. The delegate measured about 3% extra short-string cost and 5% extra JSON-number cost from sharing the helper; that cost was not eliminated. Large-input scratch is bounded, but source and harness allocations still scale with input size.

Reproduce: build `tools/js/decimal_load.rs` with release rustc, run `preflight`, `all`, `bench` or `check <oracle-dir>`. Generate independent cases with `node tools/js/decimal_oracle.mjs <dir> 17 400`. The standalone JSON and heap harnesses include the shared private module.

Not tested: exhaustive decimal-to-f64 correctness, source-cap removal, non-Linux targets or hard allocator OOM. The lexer source cap is unchanged. This change does not add BigInt, runtime builtin bindings or staging execution.


## Promise jobs

The evaluator now provides Promise construction, resolve/reject, then/catch/finally and static resolve/reject. Thenable calls become jobs. Resolver pairs share a once flag, self-resolution rejects with TypeError, and a thrown handler rejects its derived promise. Jobs run FIFO only when the host calls drain_microtasks after a script/task. Run does not drain automatically. The host can inspect pending jobs, promise status and unhandled rejections.

Defaults cap queued jobs at 262,144, tracked promises and pending reactions at 1 Mi each, and jobs per checkpoint at 1 Mi. Drains also share the evaluator step budget across all jobs. Parent review found fanout settlement could queue reactions without charging steps. Its regression failed on d8a3e50 and now consumes work before changing promise state. Queue capacity is also checked before settlement. Identity counters now fail at exhaustion instead of wrapping; the injected-exhaustion regression failed with a debug overflow before the fix.

An ordinary JavaScript throw rejects a promise. A resource limit, Unsupported or heap fault inside a running job ends the checkpoint and drops that job. Parent review found the next empty drain could then report success. A new regression failed on that behavior; later drains now repeat the failed-job error. Treat that interpreter as failed. The jobs-per-drain cap is different: it stops before starting the next job, preserves the queue and is resumable. Running a checkpoint inside an active call remains refused.

Promise state and resolver captures live outside the heap. Roots includes intrinsics, queued arguments/handlers, settled values, reaction handlers, derived promises and resolver targets. Collection traces reachable promise metadata and prunes unreachable entries and completed resolver-pair flags. Tests cover collection before a drain, captured closures, a promise reachable only through its resolver, unreferenced cycles and recycled heap slots. Promises are not collected automatically while scripts run.

All 62 Promise programs match Node vm with explicit pre/post checkpoints; the existing 202 evaluator cases still match. Parent added regressions for fanout work, counter exhaustion and repeated drains after a job failure. All 163 assembled JS tests pass in debug and release; 914 workspace tests pass and crate clippy is clean. Node runs only tiny generated programs, never target bundles.

Parent ran seven workloads at 1x, 10x and 100x: 21 fresh release processes, each capped at 1 GiB address space and 45 seconds. All checksums pass. At 100x each case uses n=100,000; finally schedules 400,001 jobs. Other work ran concurrently. Values below are one sample and establish no speed improvement.

| 100x workload | Run ms | Drain ms | Collect ms | Peak RSS MiB |
| --- | --- | --- | --- | --- |
| Chain | 120.0 | 29.5 | 10.6 | 103 |
| Fanout | 105.9 | 38.5 | 6.3 | 97 |
| Thenable | 219.0 | 178.2 | 49.8 | 293 |
| Finally | 253.6 | 514.5 | 69.6 | 355 |
| Reject | 453.7 | 90.1 | 49.9 | 219 |
| Constructor | 703.8 | 62.2 | 33.8 | 338 |
| Captured closures | 441.8 | 143.5 | 52.3 | 222 |

The constructor case allocates 1,000,053 managed objects and leaves 53 after collection. Native allocator counts were not measured; the figures report managed allocations and RSS. The delegate measured a saving from omitting resolver name/length properties but rejected it because those properties are observable. Parent retained them and did not reproduce that discarded variant.

Reproduce with tools/js/promise_check.sh and tools/js/promise_load.sh with an output path. At this foundation stage, async/await, Promise combinators, withResolvers, subclass/species construction and toStringTag were unbuilt. Async/await, symbols and combinators are integrated in later sections. Static methods on another constructor are unsupported. Without Symbol, then validates constructor but uses the intrinsic default. Unhandled-rejection reporting is poll-only. No timers, network, staging execution, other-platform behavior or hard allocator OOM recovery was tested. A real nested host callback checkpoint was not exercised; the active-call guard is tested through its entry path.


## Shared-realm scripts

Interp.realm creates a heap/global/lexical realm. Interp.run_script parses, checks and runs another script in it. Existing new/run/call APIs remain. Program can own its source through Cow and into_owned; the evaluator keeps owned programs behind Rc. Each script has separate node caches and scope tables. Closures carry their script id. Calls and construction select the callee's code and restore the caller on success, throw and limit. Host source strings can be dropped after run_script returns.

Defaults retain at most 4,096 script slots (including the empty realm slot), 64 MiB of source and 8 Mi nodes plus list entries. The lexer also caps each individual script at 64 MiB even if the host raises the total budget. Code and caches stay until realm teardown; there is no per-script reclamation. These are component budgets, not a hard total RSS limit. Parent reduced the submitted default node budget from 16 Mi to 8 Mi to lower potential retained code cost.

Parent review found new cloned a rejected program before checking the script budget. The preflight allocation test failed with 620,392 copied bytes, then passed with zero after checking limits first. The per-script lexer cap was also reported as SyntaxError when the total source budget was raised. An isolated c13efd9 baseline fails the new regression; the fixed path reports Limit(ScriptSource) before parse. Empty realms now install Promise. The cross-script checkpoint regression failed with Promise missing before that hook was added.

Global declaration checks run before definitions. Tests cover lexical redeclarations, lexical shadowing of configurable builtins, function declarations over configurable accessors and rejected declarations over restricted properties. A lexical whose initializer throws remains in TDZ. Parse/preflight rejection drops its code and rolls back new name metadata. Runtime and global-declaration errors can retain checked code; runtime failures must retain code for closures created before the throw. Parent added a regression that calls such a closure after collection.

Parked-script literal caches are roots. Promise jobs hold function Values whose code provenance is resolved by call_fn. Parent queues callbacks in two scripts, collects before the checkpoint, then verifies both callbacks and their source literals. Other tests cover colliding ExprId/FuncId values, getters, methods, host callbacks, constructor provenance, dropped host strings, cross-script recursion and state restoration on every exit.

All 180 assembled JS tests pass in debug and release; 931 workspace tests pass and crate clippy is clean. All 202 evaluator and 62 Promise Node cases still match. Parser comparison preserves all 2,087 syntax cases and normalized trees for all 15 cached staging bundles. No target bundle executes in these checks.

Parent ran five release load cases at 1x, 10x and 100x in fresh processes under 1 GiB and 45 seconds each. All 15 checksums pass. The load tool raises script/count budgets for cases above the defaults. These are one-sample final results with concurrent work; no speed-up is claimed.

| 100x workload | Run ms | Peak RSS MiB | Allocations |
| --- | --- | --- | --- |
| Many small scripts | 87.0 | 53.3 | 743,461 |
| Five large scripts | 145.1 | 95.3 | 450,959 |
| Cross-script calls | 868.7 | 3.5 | 162 |
| Host calls between scripts | 1,286.6 | 6.2 | 9,985,573 |
| Rejected scripts | 219.8 | 31.7 | 3,201,910 |

Many-small retains 20,003 script slots, 380,014 nodes and 1.09 MiB source including its post-collection checks. Few-large retains about 1.2M nodes and 3.37 MiB source. The host case retains 2,004 scripts while making two million calls. Count refusals do no parse. Rejected syntax creates error objects until explicit collection; this accounts for much of that case's RSS. The delegate measured arena shrinking during ownership conversion; parent retained it but did not rebuild the discarded pre-shrink version.

Reproduce with tools/js/scripts_load.sh and the scripts_load preflight mode. Not tested: per-script reclamation, actual webpack bootstrap across the deployed resource list, DOM/document loading, modules, classes/async/builtin branch integration or hard allocator OOM recovery. Unsupported spans still lack a script id. Existing evaluator semantic gates remain in force.


## JavaScript JSON, regexp and String bindings

Shared realms now bind JSON.parse, JSON.stringify, RegExp construction and literals, exec/test, and the observed String operations. These include UTF-16 search and slicing, split, match, replace, concatenation, trimming and case conversion. Replacement callbacks run after global matching. Regexp objects carry a real lastIndex descriptor. Compiled literals use both script and node ids. Collection prunes dead regexp metadata and clears the 256-entry constructor cache. Literal code stays under the realm code budgets.

JSON.stringify uses an explicit frame stack. It calls toJSON and replacer functions through the evaluator. Replacer arrays, indentation, getters, inherited array values, holes, cycles, non-finite numbers and lone surrogates are covered by tiny Node comparisons. It detects cycles only along the current path, so repeated acyclic references are valid. JSON.parse keeps the evaluator prototypes. A callable reviver runs the ES2022 InternalizeJSONProperty walk with an explicit frame stack (eval_json_revive.rs). It reads key lists and array lengths once per container, reads each value just before its visit, deletes on undefined and uses CreateDataProperty otherwise. It reuses json.max_depth and json.max_nodes as distinct Json limits and charges steps per visit. It passes two arguments: the newer source-text context argument is absent. This is an ES2022 compatibility limit, not an explicit Unsupported error. Check with tools/js/revive_check.sh and tools/js/revive_load.sh.

Parent review added failing regressions before five fixes. Successful long string searches now charge comparison work, with scans checked in bounded chunks. Whitespace trimming charges its scan even when the result is empty. Regexp.toString reads source and flags through ordinary property access. Unicode case conversion checks the actual output size. Stringify omits undefined object properties before writing their key, so an omitted long key does not falsely exhaust the output cap. A separate test checks regexp literals from two scripts across collection and a rejected script slot.

Default binding caps are 50M regexp work units per attempt, 16 Mi input units, 1 Mi backtrack frames, 4 Mi capture slots per global operation and 1 Mi split items. JSON.parse caps source and decoded text at 16 Mi units, nodes at 4 Mi and nesting at 1,024. Stringify caps output at 16 Mi units, depth at 1,024 and visited nodes and keys at 4 Mi each. Limits are distinct evaluator errors and cannot be caught by JavaScript. The heap does not collect during a call. Object key enumeration allocates its list before the stringify key cap; that list remains bounded by the heap budget. Case mapping uses bounded input but creates a temporary Unicode segment before checking its final encoded size.

Parent ran 90 binding and 57 stringify Node cases. The 202 evaluator and 62 Promise cases still match. Node executes only tiny independent programs. All 213 assembled JS tests pass in debug and release; 964 workspace tests pass and crate clippy is clean. The added collection regression compares function count with the initial intrinsic count instead of increasing a fixed threshold for each new builtin.

The final load matrices ran 42 fresh release processes: eight binding workloads and six stringify workloads at 1x, 10x and 100x. Each process had a 1 GiB address-space cap and a 45-second timeout. All checksums and expected limits passed. The following final samples ran with concurrent work. They establish no speed improvement.

| Binding workload at 100x | Run ms | Peak RSS MiB |
| --- | --- | --- |
| Parse 8.48M JSON units | 294.4 | 164.5 |
| Scan 200k matches | 672.7 | 152.1 |
| Replace | 530.7 | 64.8 |
| Split | 223.7 | 127.1 |
| One million String-operation iterations | 3,767.7 | 294.9 |
| One million RegExp constructions | 2,687.4 | 501.8 |
| Global empty matches | 273.1 | 71.4 |
| Explicit-collection rounds | 965.8 | 4.2 |

| Stringify workload at 100x | Serialize ms | Peak RSS MiB |
| --- | --- | --- |
| 100k shallow objects | 154.6 | 171.4 |
| Depth 1,000, 20 repetitions | 85.8 | 11.3 |
| 50k callback items | 93.3 | 41.9 |
| Five million escaped input units | 10.0 | 30.3 |
| 500 explicit-collection rounds | 9.5 | 3.6 |
| Expanding acyclic graph, expected Nodes limit | 512.7 | 23.4 |

Stringify collection leaves 90 live objects before and after all three scales. Binding collection reclaims 750,000 objects at 100x and leaves 88 live objects. Large no-collection cases retain dead objects and interned strings until the host collects. Native allocator counts were not measured. The delegate retained source/flags caching after finding no measurable gain; parent does not claim an optimization from it.

Reproduce with tools/js/builtins_check.sh, tools/js/stringify_check.sh, tools/js/builtins_load.sh and tools/js/stringify_load.sh. Pass an output path to each load script. Missing features include wrappers, revivers, custom regexp exec, Symbol protocols, species copies, several String methods and Unicode regexp modes. Invalid literal patterns still report Unsupported during evaluator preflight; the parser itself does not validate patterns. Regexp flag getters do not yet implement generic overridden-flag reads. Rust Unicode tables can differ from the Node version. Not tested: target bundle execution, staging login, other platforms, hard allocator OOM or combined class/async/collection branches.


## DOM and event host bridge

The bare library now connects the native Document to a shared JavaScript realm. Host provides call, roots, trace and prune hooks. Host functions use the evaluator call-depth and traversal guards. Host scopes share one execution budget across callbacks and Promise checkpoints. The host is cloned before callbacks so no host borrow must span JavaScript execution. With a host installed, collect through Interp.collect; passing Interp.roots directly to Heap.collect does not trace opaque host edges.

Browser exposes script execution, explicit collection, selectors, wrapper lookup and driver input/event dispatch. Wrappers keep stable identity while reachable. The document wrapper is a root. Parent and child wrappers and listeners form traced edges. Detached wrappers survive when JavaScript holds them and are pruned otherwise. Native input state persists with native nodes. Generation handles prevent a recycled JS slot from inheriting an old wrapper or event.

Document adds detached element/text/comment/doctype creation and attribute/text mutation. New nodes and clones invalidate the sibling cache. Attribute changes invalidate cached class sets. Old text and attribute slots can remain in the native arena. Native nodes never leave that arena. This is a retention limit, not a claim that detached native nodes are collected.

The bridge supports tree mutation, attributes, real CSS queries, input value/checked state and capture/target/bubble events. Listener throws become bounded reports; resource errors stop dispatch. Reports retain at most the configured count and at most 4,096 message bytes each. Zero report capacity stores none and counts drops. Driver events are untrusted and have no default actions. A driver checkpoint runs between its listeners; dispatch called from JavaScript leaves microtasks for the later checkpoint.

Parent review resolved the host and builtin hooks as a union. A stale test assumed String.charCodeAt was absent; it now checks the surrogate code unit. Added regressions reproduced clone attribute-cap bypass, UTF-8 byte-cap bypass, zero report-cap bypass, oversized reports, insufficient sibling-selector work accounting, rejected document-root replacement, initial-document cap bypass and uncharged DOM string copies. The clone preflight also checks copied dirty input bytes. CustomEvent now retains its prototype when detail is explicitly undefined; Node confirms that this dictionary value defaults to null. Combined listeners from two scripts use Promise jobs, JSON and distinct regexp literals correctly after collection.

Default native caps are 1 Mi nodes, 64 Mi text bytes, 2 Mi attribute slots and 65,536 names. Wrapper and listener counts are 65,536, with 1,024 listeners per target. Events cap at 16,384. Result arrays cap at 65,536, inserted tree depth at 512, selectors at 1,024 bytes and crossing strings at 16 Mi UTF-8 bytes. Names, input strings and native text have separate component budgets. Host metadata has count and per-string limits, not one aggregate RSS limit. Initial node/text/attribute/name counts are checked before realm setup. Clone attribute and dirty-input budgets are checked before native node allocation.

Selector matching uses a conservative work charge. Descendant searches use tree depth. Sibling searches include document width, including skipped non-elements. Lists add their charges. More than two descendant/later-sibling combinators remain refused. This can reject a wide query before the unmetered CSS matcher runs. It is not precise instruction accounting. String conversion and copies charge by size; textContent checks capacity before appending each text. Input receiver validation no longer copies its current value just to check the tag.

All 1,009 workspace tests pass. The changed crates pass release tests: 36 browser-library tests, 25 CLI tests, 102 HTML tests and 218 JS tests. JS clippy with warnings denied is clean. The wider clippy run reports existing warnings in HTML, CSS, Unicode, net and main. The new event oracle matches 15 tiny Node cases. Existing binding 90, stringify 57, evaluator 202 and Promise 62 cases still match. Node has no DOM tree in this oracle; tree expectations remain handwritten. Node capture-removal, passive and re-dispatch-name differences remain covered by separate Rust expectations.

Parent ran the three load workloads at 1x, 10x and 100x in fresh release processes under 1 GiB address space and 45 seconds each. All nine checksums pass. The final sample ran with concurrent work and establishes no speed improvement.

| Workload | Scale | Run ms | CPU ms | Peak RSS MiB | Allocations |
| --- | --- | --- | --- | --- | --- |
| Mutation | 1x | 6.3 | 0 | 4.7 | 29,264 |
| Mutation | 10x | 61.0 | 60 | 5.7 | 225,794 |
| Mutation | 100x | 618.3 | 600 | 14.6 | 2,472,770 |
| Events | 1x | 4.0 | 0 | 4.3 | 18,192 |
| Events | 10x | 35.3 | 30 | 4.4 | 179,835 |
| Events | 100x | 355.6 | 350 | 5.2 | 1,796,238 |
| Lookup | 1x | 6.7 | 0 | 4.4 | 16,539 |
| Lookup | 10x | 63.6 | 60 | 4.5 | 159,963 |
| Lookup | 100x | 619.8 | 610 | 4.9 | 1,594,116 |

CPU uses 10 ms ticks, so zero does not mean no CPU work. Mutation at 100x retains 200,005 native nodes and 1,005 wrappers; collection frees 99,000 JS objects across the run. Event at 100x retains 9 native nodes, 8 wrappers and 3 listeners and frees 150,003 JS objects. Its accounted live bytes rise from 0.2 to 0.9 MiB. Added code counters show 13 to 1,003 retained scripts and 149 to 6,089 syntax/list nodes. This confirms retained code growth while DOM metadata stays flat; it is not an allocation breakdown proving the source of every retained byte. Lookup at 100x retains 405 wrappers and 503 scripts. Collection time is included in run time.

Reproduce with tools/js/browser_check.sh and tools/js/browser_load.sh with an output path. Not tested: a real browser DOM comparison, staging execution, focus, default actions, form validation, live collections, full namespaces, lone-surrogate storage, native arena reclamation, hard allocator OOM or cross-platform allocator behavior. Lists are snapshot arrays. The CLI still does not execute page scripts. The delegate's original commit used a hook override; parent integration used the normal configured git hooks. No hook or identity configuration was changed.


## Byte buffers and UTF-8 codecs

The evaluator now binds ArrayBuffer, Uint8Array, TextEncoder and UTF-8 TextDecoder. Buffers own Vec bytes. Views store a checked buffer handle, offset and length. A subarray shares bytes; slice copies them. Uint8Array.set handles overlapping ranges. Integer-indexed properties use virtual entries instead of one heap property per byte. The embedding helpers can move a Vec into a buffer/view without copying and borrow its bytes. Pointer-identity and stale-generation tests cover that contract.

Buffers have single and aggregate backing-byte caps of 256 MiB by default. Backing bytes also count against the heap budget. Parent review reproduced an accounting fault: object and payload could each fit while their sum exceeded max_bytes. The combined check now runs before allocation. Empty copies validate the range instead of bypassing bounds. View construction and copying check offsets and lengths. Failed multi-step constructors can leave unreachable intermediate objects until explicit collection.

A view marks its backing buffer. The evaluator collector uses stored_keys instead of own_keys, so it does not build virtual byte-index lists while tracing. The delegate measured high memory and latency with that earlier tracing path; parent retained the fix but did not rebuild the discarded version. A new DOM fixture keeps a view on an attached wrapper, a closure on its buffer and a listener in another script. After collection, a Promise job decodes the bytes and calls the buffer closure correctly.

Real enumeration still lists view indices. Parent review added a regression where an exhausted step budget previously reached key-list allocation first. Object.keys, object spread, for-in and stringify now charge the virtual range before creating that list. The heap also checks the temporary key-list byte cost. Static builtin counts in older tests now use the initial intrinsic count plus the intended test allowance.

The UTF-8 encoder preserves complete code points in encodeInto and replaces lone surrogates. Decoder state carries at most three pending bytes. It handles streaming boundaries, fatal errors and BOM policy. Parent reproduced false output-limit failures from a pessimistic input-length-plus-four estimate. Decode now checks actual UTF-16 growth before every append. This accepts ASCII, multibyte text and surrogate pairs at the exact output cap. A stripped BOM fits a zero-unit low-level output cap. Output-limit errors remain resource errors. The new bounded decoder does not add a second decode pass. Decoder label scans now consume steps.

Native byte passes charge work. The defaults are one step per 64 copied bytes, one per 16 encoded/decoded bytes and one per element read from JavaScript. Buffer counts and lengths are component budgets, not a hard RSS bound. Vec capacity slack, metadata and temporary interpreter allocations can add memory beyond accounted payload bytes.

All 1,043 workspace tests pass. Release tests pass for 251 JS tests and 37 browser-library tests. JS clippy with warnings denied is clean. The 79 tiny Node binary cases match. Existing builtin 90, stringify 57, evaluator 202, Promise 62 and browser-event 15 cases also match. Node ran no target bundle. Separate Rust tests cover the declared WebIDL argument-order differences from the Node implementation. The pure decoder tests compare generated byte sequences and split points with the standard library.

Parent ran five release workloads at 1x, 10x and 100x, each in a fresh process under 1 GiB address space and 45 seconds. All 15 results match Rust-side checks. The following final samples ran with concurrent work. They establish no speed improvement.

| Workload at 100x | Data | Run ms | Collect ms | Peak RSS MiB |
| --- | --- | --- | --- | --- |
| Copy, slice, set and shared views | 50 MiB input | 51.9 | 0.1 | 303 |
| Uint8Array scan | 10M reads | 5,946.5 | 0.1 | 22 |
| Plain-array scan baseline | 10M reads | 12,737.3 | 215.3 | 441 |
| UTF-8 round trip | 50 MiB | 175.0 | 0.1 | 209 |
| Streaming UTF-8 | 50 MiB in 1,000-byte chunks | 175.2 | 2.2 | 125 |

Copy retains four 50 MiB backing buffers after collection. Streaming retains its input buffer and reclaims 104,858 temporary objects. Decoder registers stay fixed; scripts and input strings still consume ordinary realm memory. Scan timings include evaluator property access. The plain-array case also creates its numeric array in JavaScript, so it is not an isolated comparison of single-read latency. CPU counters cover the process, including setup, at 10 ms resolution. Native allocator counts were not measured.

Reproduce with tools/js/binary_check.sh and tools/js/binary_load.sh with an output path. The public host helpers are set_binary_limits, new_array_buffer, new_uint8_array and bytes_of. Unsupported features include resizing, transfer, non-UTF-8 encodings, many typed-array methods and object-valued defineProperty conversion. SharedArrayBuffer, DataView and other typed families are absent. Symbol iterator/tag/species hooks and native subclassing still need the parallel branch adapters. Not tested: real fetch/Flight streams, staging execution, long-running soak, hard allocator OOM, big-endian or 32-bit targets.


## Browser task scheduler

The browser host now provides setTimeout, setInterval, cancellation, queueMicrotask, performance.now and MessageChannel. One queue orders tasks by due time and sequence. No OS timer, thread or server runs these tasks. The driver calls run_scheduled_tasks, advance_to or advance_time. A virtual clock supports deterministic fixtures. Event timestamps and performance.now use the same clock.

Timer ids are non-reused positive integers. Delays use ToInt32 and clamp negative values to zero. The implemented nested-timer policy clamps delays below 4 ms from level six. Clearing an interval in its callback prevents rescheduling. Arguments are capped before copying. String/eval callbacks are unsupported. Each task uses a host scope and checkpoint. Ordinary throws become reports and later tasks run. Other errors halt the pump permanently.

queueMicrotask uses the real Promise FIFO queue. Host.report_exception handles ordinary thrown callback values; its default drops them. Limits, Unsupported and heap faults poison later checkpoints. Parent review reproduced a failure after an externally drained microtask exhausted its budget: with no jobs left, a timer could run before the pump noticed the failed checkpoint. The new regression observes the unwanted timer side effect. The pump now always checks the checkpoint before taking a task, even when the job queue is empty.

Message ports reuse EventTarget dispatch. Primitive data waits until the receiving port starts, then each delivery is a task. Timers, queued arguments, port peers and queued messages are traced through Host collection. Unreachable port cycles and event metadata are pruned. Generation handles prevent slot reuse from reviving an old port. The tested close policy delivers previously queued messages and drops later posts. This has no real-browser comparison. Objects and transfer lists remain explicit gates. MessageEvent.ports is mutable, and channel ports are own properties; these are declared deviations.

Default caps are 65,536 timers, 256 extra arguments per timer, 16,384 ports, 65,536 queued messages and 16 MiB of queued message payload. The driver also caps tasks per pump. Id and sequence exhaustion fail. Events remain under the DOM event cap and need explicit collection between long pumps. A DrainJobs refusal halts this pump, although the VM drain itself remains resumable. A halted realm should be discarded by its owner.

Parent ran the changed-crate debug and release checks, the workspace suite and JS clippy with warnings denied. Node comparisons match 15 scheduler cases, 15 event cases and 62 Promise cases. Node runs only authored programs. Timer-id type, Node's minimum-delay clamp and undefined-message differences are excluded from the common corpus. Native fixtures cover the engine policy. No real-time precision or browser scheduling equivalence is claimed.

All nine release load cases pass with checked results, a virtual clock, 1 GiB address-space cap and 45 seconds per process. The final samples ran with concurrent work and establish no speed improvement.

| Workload at 100x | Tasks | Run ms | CPU ms | Peak RSS MiB | Allocations |
| --- | --- | --- | --- | --- | --- |
| Timers | 80,600 | 217.3 | 210 | 5.2 | 770,959 |
| Messages | 100,000 | 290.1 | 280 | 4.9 | 1,321,794 |
| Mixed | 200,000 | 680.0 | 670 | 6.1 | 2,514,882 |

After collection, all three cases have zero timers, tasks, queued messages and events. The message case retains two ports for its live channel; the others retain none. CPU uses 10 ms ticks. Reproduce with tools/js/scheduler_check.sh and tools/js/scheduler_load.sh with an output path.

Not tested: actual bundles, staging, browser event-loop comparison, real timer precision, other platforms or hard allocator OOM. requestAnimationFrame, requestIdleCallback, window.postMessage, BroadcastChannel, ErrorEvent/window.onerror, object structured clone and transfers are absent. Native constructor subclassing still needs the language integration. Navigation and network pumps remain separate pending work.


## Fetch and buffered response readers

The browser library now binds fetch, Headers, Request, Response and a buffered byte-reader subset. fetch validates and queues a request with a pending Promise. run_network_tasks performs actual bare-net I/O, settles the Promise and runs a checkpoint. Host-created Promises use the existing resolve/reject machinery; callers must settle them once and root them while pending.

Client.send_once returns one response without cache or automatic redirects. It retains request validation, verified TLS, pooling and the POST no-resend rule. Fetch checks the initial origin before DNS/socket work and checks every redirect before another request. Cross-origin requests are unsupported. Cross-origin and HTTPS-to-HTTP redirects reject without forwarding bodies or credentials. Managed request headers are filtered; response Set-Cookie is hidden. Diagnostics omit paths, queries, headers and bodies.

Bodies are fully buffered. getReader yields checked Uint8Array chunks from those bytes; this is not live socket streaming. text, json and arrayBuffer enforce single-consumer state. Clones share immutable bytes with separate consumption state. Pending Promises and response/header/reader edges participate in Host collection. Body accounting is conservative for shared clones. Objects count until explicit collection.

Parent resolved scheduler and fetch Host hooks as a union. Four new regressions failed before their fixes. Numeric record header names now survive conversion. Zero diagnostic capacity retains nothing and counts drops. The network pump checks even an empty failed checkpoint before I/O and preserves fatal pump errors. Abort checks the same state before settling queued requests. The maximum usize body cap no longer overflows its one-extra-byte read limit.

All 1,106 workspace tests pass. Changed-crate release checks and JS clippy with warnings denied pass. The suite includes real loopback HTTP and four private-CA HTTPS cases: POST/GET, wrong root/host, and redirect refusals. The 14 pure-object Node cases match; event15, scheduler15, binary79, Promise62 and evaluator202 cases still match. Node never executes app bundles. Real network fetch was not compared against Node.

All nine bounded release load cases pass under 1 GiB address space and45 seconds per process. Final samples ran with concurrent work; no speed improvement is claimed.

| Workload at 100x | Work | Run ms | CPU ms | Peak RSS MiB |
| --- | --- | --- | --- | --- |
| POST | 10,000 loopback requests, queue up to8 | 284.1 | 240 | 7.5 |
| Byte reader | 100 MiB through chunks | 37.7 | 30 | 9.8 |
| Objects | 100k Header/Request/Response/clone/reader rounds | 1,117.4 | 1,110 | 7.8 |

Each case leaves zero fetch objects, pending requests and retained/queued body bytes after collection. Object-case collection takes182.2 ms and is included in run time. Timings check Rust-side results; byte-reader checks sample each chunk at its first, middle and last byte. CPU uses10 ms ticks. Reproduce with tools/js/fetch_check.sh and tools/js/fetch_load.sh with an output path.

The document must configure networking. When navigation has a document URL, fetch uses it immediately; otherwise update_fetch_base_url supplies the fallback base. Queued requests retain their original origin. Cross-origin/CORS, cookies, Origin/Referer, signal-based abort, custom stream sources, BYOB, pipe operations, non-ASCII headers and Headers iterator protocols remain absent or gated. Headers.forEach uses a snapshot. getSetCookie returns an empty list, including on manually created headers. Network statusText is empty because the HTTP layer drops its reason phrase. In-progress blocking I/O cannot be interrupted; abort handles queued requests only. The client timeout can exceed a pump's desired wall budget. A fatal driver error requires discarding the realm. No staging, credentials, GUI, browser comparison or native subclass/Symbol integration was tested.


## Navigation and origin storage

The browser library binds URL, URLSearchParams, location, history, localStorage and sessionStorage. URL records reuse bare-url and keep fragments separately. A document URL must be configured explicitly. URL parsing refuses unsupported schemes, IDNA, user info, backslashes and unsupported path forms. No network or disk stores supply origins or tokens.

WebStorage keeps UTF-16 keys and values per origin. A shared store preserves a session across new Browser instances. new_session shares local data and starts empty session data. Quotas check before writes. This does not persist across process restarts. Named storage properties and storage events are absent.

History clones plain objects and arrays, including cycles, sparse arrays, undefined and lone surrogates. Native state and cached materializations participate in collection. Depth is capped at128 even when the caller raises its configured limit. State nodes, bytes, history entries and queues have separate caps. Full navigation produces a NavigationRequest for the loader. It does not load a document. The embedder must create a fresh Browser for a new document realm; setting its URL alone does not replace its JavaScript state.

Parent integration keeps scheduler, fetch and navigation Host hooks. The navigation pump checks a checkpoint before spending a queued task. Fatal errors halt later calls. Tasks run checkpoints. Fetch resolves relative requests against current history state immediately when networking is configured. Already queued requests retain their original origin.

Five regressions failed before their fixes: a failed empty checkpoint allowed an event, large history.go values used a clamp instead of signed32-bit wrapping, configured clone depth had no hard ceiling, Request used the old path after pushState, and URLSearchParams.set could exceed the total byte cap. A loopback fixture also combines history, session storage, fetch, JSON, a timer and collection in one realm. Its first two runs exposed wrong clock setup in the test; the corrected fixture uses a virtual clock.

All1,150 workspace tests and162 release tests for bare pass. Clippy reports only existing warnings. The Node URL oracle matches333 ungated cases and reports35 explicit gates. It does not establish history, location or storage behavior in a real browser. All12 bounded release load rows pass under1 GiB address space and45 seconds per process. Samples ran with concurrent builds; no speed improvement is claimed. At100x, URL takes1503 ms, history915 ms, storage481 ms and routing729 ms. Peak RSS ranges from6.3 to10.3 MiB. URL/params/iterator wrappers and pending queues return to zero after collection. History retains50 entries. Routing retains4.9 MiB for5003 scripts.

Reproduce with tools/js/navigation_check.sh and tools/js/navigation_load.sh with an output path. Not tested: Chrome, actual staging, authentication, storage at the full default quota, other platforms or hard OOM. baseURI does not read base elements. PopStateEvent and HashChangeEvent classes, onpopstate and storage named properties remain absent or gated. Iterable URLSearchParams is integrated below. Fragment event behavior has structural tests only. The CLI page loader and unified driver remain separate work.


## Unified language and byte protocols

The realm now combines Array/Object operations, Symbols, Map/Set, classes, private names and async/await. Shared script ownership applies to constructors, field initialization, methods and resumed activations. Native tasks and async resume jobs use the same FIFO Promise checkpoint. Resource faults stay uncatchable and preserve sticky checkpoint failure.

Array/Object operations include the observed traversal, copying, sorting and conversion methods. Array species, concat spreadability and String/RegExp hooks use the well-known Symbols. Map/Set use insertion-order entries and SameValueZero lookup. Array, String and collection iterators share the protocol. Custom species and several wrapper/subclass cases remain unsupported. Math.clz32, log, random and LN2 are integrated below with numeric radix conversion.

Async for-of saves the source, iterator kind, iterator, next method and position. Collection traces all of them while suspended. IteratorClose runs on ordinary abrupt exits. Limits, Unsupported and heap faults run no more JavaScript. Classes preserve script provenance, private brands, home environments and this across awaits. Await positions that the machine cannot execute are refused before script effects.

Uint8Array iterable construction gathers values before coercion through the shared protocol. Branded byte iterators use the shared array iterator with the view's own length. Byte constructors expose tags and default species. Custom species and native byte/Map/Set subclasses are refused explicitly. Heap.splice_dense refuses every byte-view prototype, including a zero-length view. Object.assign, values and entries charge virtual view indices before key allocation. Collection keeps stored-key traversal and the backing-buffer edge.

The parent added a synthetic loopback integration fixture. An async class method serializes a request, awaits verified-client fetch and JSON decoding, updates session storage and history, decodes bytes, then completes through a timer. It spans two scripts and collects while suspended and after completion. The first assertion incorrectly expected a Promise expression to return undefined; it was corrected to expect an object. This fixture does not establish actual staging login.

The child ran the existing class, async, interaction, collection, array and byte load matrices under caps. Its new adapter matrix has15 checksum-verified cases. A paired plain interpreter comparison showed a consistent1–3% slowdown that was not traced. The cause is unknown. No optimization or speed gain is claimed.

Not tested: actual target execution, staging login, GUI, browser differential behavior, other platforms or scales above100x. There is no automatic collection during script execution. Code lives until realm teardown. Custom species, native subclasses, several typed-array operations, unsupported await positions, modules and generators remain gates. Promise combinators are integrated below. MessageChannel payloads remain primitive-only.

Parent checks pass1,261 workspace tests,358 JS release tests and169 bare release tests. JS clippy with warnings denied is clean. All17 oracle groups match their supported cases: evaluator202, Promise62, builtins90, stringify57, Array/Object192, Symbol/collections38, seam13, class107, async94, interaction22, binary79, events15, scheduler15, byte adapters21, async adapters13, fetch14 and navigation333 with35 explicit gates. Node ran authored programs only. All15 target files pass parse/preflight without executing.

All15 parent adapter load rows pass under1 GiB and45 seconds per process. At100x, iterable byte construction takes154 ms and17 MiB RSS; a20M-step byte walk takes1313 ms and4 MiB. The50k suspended-iterator case takes255 ms before drain,168 ms to drain and44 ms for final collection, with216 MiB peak RSS. After final collection, activations, private slots, frames and async slots return to zero. Small differences in live-object baselines follow each test's retained definitions. Samples ran with concurrent checks; no speed comparison is claimed. Reproduce with tools/js/adapter_check.sh and tools/js/adapter_load.sh with an output path.


## Numeric builtins

Math.clz32 uses ToUint32 and leading_zeros. Math.log uses f64::ln, including the specified zero, negative, infinity and NaN cases. LN2 has a non-writable, non-enumerable, non-configurable descriptor. The platform logarithm can differ from Node; bit identity is not claimed.

Math.random holds one xorshift128+ state per realm. The first call reads16 bytes from /dev/urandom. Short reads, errors and an all-zero seed refuse initialization. There is no time or PID fallback. Hot calls allocate nothing in this path. This state is not cryptographic and supplies no TLS or credential randomness. Tests cover deterministic injected states, range, separate realms and rejected seed input.

Number.prototype.toString checks its receiver before radix coercion. Radixes2 through36 are accepted. Decimal output reuses the existing shortest converter. Non-decimal output uses a fixed2200-byte stack buffer. The intrinsic prototype is recognized as zero by this method. Heap string limits apply before interning, and output work consumes steps. Wrapper Number objects and other Number methods remain unsupported.

The radix function is a port of V8 12.4.254 DoubleToRadixCString. Its source, copyright and BSD-3-Clause terms are retained in crates/js/LICENSE-V8. The crate's SPDX expression reflects those terms. This adds no external crate dependency. Non-decimal output follows this implementation choice; it is not proof of all-engine equivalence.

Parent regressions failed before the fixes for intrinsic-prototype conversion and long-output work charging. The OS seed reader now has injected error, truncation and zero-input checks. All1,273 workspace tests pass. Release checks pass370 JS tests and169 bare tests. JS clippy with warnings denied is clean. The664-case radix fixture regenerates byte-for-byte from Node. Existing evaluator202, Array/Object192, binary79, adapter21, async-adapter13 and Promise62 oracle cases still match. Node ran authored programs only.

The load harness now asserts independent Node checksums for deterministic operations. Logarithm sums allow a relative1e-12 tolerance. Random calls assert range, with deterministic sequence and uniqueness checks in unit tests. All21 fresh-process load rows pass under1 GiB address space and45 seconds. At100x,100k random calls plus checks take83.7 ms and about4 MiB peak RSS.100k subnormal binary conversions take1.10 s and214 MiB. The heap retains intermediate strings until explicit collection; this load does not collect inside the loop. These are full-script samples with concurrent work and added validation, not isolated native timings or speed improvements.

Reproduce with tools/js/numeric_load.sh and cargo test -p bare-js numeric_tests. Not tested: actual staging execution, login, GUI, non-Linux platforms, hard allocator OOM or bit-identical logarithms across implementations. The exact bundle call sites were not recorded by this worker.


## Browser iterator protocols

Headers and URLSearchParams use the interpreter's existing iterator engine. The public HostIter API is opaque and valid only during one host call. It offers open, next, close and failure operations. The realm supplies its own Symbol.iterator key and IteratorPrototype. Deleting the Symbol global does not change those identities.

Both browser types expose keys, values, entries and Symbol.iterator. Their iterators are live and themselves iterable. Host trace edges keep the source object alive. Collection prunes unreachable metadata, including cycles and reused slots. An async Headers walk survives collection with its source global removed.

Constructor inputs with an iterator use pair sequences; other objects use records. Caps check count and bytes before list growth. Byte views with too many virtual keys fail before own_keys materializes the list. History cloning rejects a view even if native code changes its prototype. The child reproduced 773 MiB RSS without the record guard and 12 MiB with it; the parent did not repeat that large allocation.

Parent regressions failed for copying an existing Headers without its iterator, validating names before all sequence values were converted, closing an iterator after Headers conversion failure, and changing an existing header before rejecting excess bytes. All are fixed. Headers sequence errors no longer close iterators, following WebIDL conversion. URLSearchParams keeps its tested Node-compatible close behavior; this differs from WebIDL sequence conversion. The parent brief had requested closing for both and was corrected for Headers. An inner pair stops after three values and reports TypeError; full sequence consumption is not implemented for longer pairs.

All 1,288 workspace tests pass. Release checks pass 371 JS tests and 183 bare tests. JS clippy with warnings denied is clean. The 35 Fetch cases match Node. Navigation matches 353 ungated cases with 35 explicit gates. Existing evaluator 202 and Promise 62 cases match. Node runs authored programs only. The child saw one release connection-reuse test failure; its reruns passed. Parent debug and release runs passed. The intermittent race remains unresolved.

Eight bounded parent load processes pass under 1 GiB and 45 seconds. Fetch iteration at 100x takes 3104 ms, 3090 ms CPU and 9.3 MiB peak RSS. Params iteration takes 3388 ms, 3380 ms CPU and 9.0 MiB. Iterator metadata returns to zero after collection. Existing 100x object and URL cases pass too. These are single samples with concurrent checks, not speed improvements. Map initialization still allocates per iterator result and pair; no optimization was added. Reproduce with the fetch_load and navigation_load examples, case iterate at scales 1/10/100.

Not tested: staging, authentication, GUI, native iterator timing on other platforms or whole-workspace release. Headers.forEach remains a snapshot. Headers getSetCookie remains empty. Iterator toStringTag is absent. Same-origin fetch policy, buffered readers, URL syntax gates and unsupported stream APIs remain unchanged.


## Page scripts, sessions and theme APIs

The page driver runs classic scripts in a shared realm and pumps the existing fetch, scheduler and navigation queues. It checks budgets before downloads and each task. A failed checkpoint blocks further work even when its queue is empty. Navigation completes the current checkpoint, skips queued dynamic scripts from the old document and creates a fresh Browser. WebStorage survives by origin. Per-page time limits are not replaced by the session limit. URL fragments are preserved within the URL cap. XHTML is refused because there is no XML parser.

The loader uses a pre-parsed DOM. Blocking and defer scripts precede async scripts, which run in document order rather than network arrival order. Dynamic inline scripts run after the current task. Module scripts are unsupported. This is not parser-blocking HTML or complete browser scheduling. Stylesheets are not applied by this driver. Session execution does not resume interactive input after settling yet.

Resource downloads now use send_stream_once and check each redirect before opening the next connection. An HTTPS-to-HTTP resource redirect is refused. HTTPS documents also refuse direct plaintext script URLs. Diagnostics use fixed MIME, encoding and exception labels instead of arbitrary server headers or exception names. Document and resource body caps remain active. Blocking I/O and synchronous JS can overshoot wall-clock deadlines. The document parser runs before Browser node-cap validation; the document byte cap bounds input, not exact parser allocation.

The theme platform uses the existing media-query matcher and event engine. The default viewport is 1280 by 800 with light colour preference. Session configuration and CLI width can override the viewport before scripts. Media-query change listeners run checkpoints and participate in collection. classList reads and writes the native class attribute. Its iterators use the realm's Symbol identities. getElementsByTagName and getElementsByClassName return snapshot arrays. Native node and text arenas retain data until document teardown or their caps. Media lists with change listeners remain rooted within their cap. Live collections, style, dataset, innerHTML, full namespaces and browser comparisons remain open.

Twelve parent regressions failed before their fixes. They cover task budgets before I/O, empty failed checkpoints, idle timer and history batches, per-page time, fragment truncation, navigation checkpoints, untrusted diagnostics, XHTML, plaintext scripts and maximum-size caps. New positive tests cover viewport propagation, checked redirects and one-request streaming. All 1,351 workspace tests pass. Release checks pass 241 bare, 371 JS and 85 net unit tests plus the integration suites. The post-lint bare run passes. Clippy reports existing warnings only. Browser 15, scheduler 15, Fetch 35 and navigation 353 oracle cases match Node; 35 URL cases remain gated. Node ran authored cases only.

All 24 parent load processes pass at 1x, 10x and 100x under 1 GiB address space and 45 seconds per process. At 100x, blocking, dynamic and mixed loading take 52, 161 and 123 ms with 14.9, 15.7 and 17.5 MiB peak RSS. One hundred document transitions take 46 ms and 6 MiB; the heavy transition case takes 516 ms and 8 MiB. Media, class and query cases take 449, 606 and 242 ms with 7.9, 10.2 and 11.0 MiB. Checksums pass, pending queues drain and obsolete wrapper metadata is collected. These are single samples with concurrent checks, not speed improvements. Reproduce with tools/js/loader_load.sh, session_load.sh and platform_load.sh, each with an output path.

The cached bootstrap example serves the saved HTML and cached public scripts over loopback. It requires a rendered selector and zero script/resource failures before success. Current output is Page(Settled), 21 scripts, 15 downloads, 469,525 script bytes, one ReferenceError, missing WeakMap and no form. It exits 1. A separate live bare --run HTTPS check reaches the same script counts and one ReferenceError with no load errors. The live run does not identify that exception's API; the cached check does. Neither result is successful hydration or login. No credentials, authenticated API calls, GUI or browser comparison were used. Reproduce the cached gate with cargo run --release -p bare --example bootstrap_check -- BUNDLE_DIRECTORY.


## Weak collections

WeakMap and WeakSet support object keys, iterable constructors, branding, get/set/add/has/delete and toStringTag. Constructors use the existing iterator protocol and look up the adder. Primitive writes throw TypeError; primitive reads and deletes are absent. Registered Symbol writes throw TypeError. Non-registered Symbol writes, including well-known symbols, are Unsupported. Native subclass construction is Unsupported. WeakRef and FinalizationRegistry are absent.

Each collection has a generation-checked object-key table. Collection and entry caps check before growth. The interpreter's mark loop implements ephemerons: a reached map releases values for already marked keys and indexes other values by their pending key. Marking a key releases its waiting values. Each entry is visited a bounded number of times. Unreachable collections and entries are pruned before the final heap sweep. Values do not make an otherwise unreachable key live. Existing closure, class, async, buffer and host tracing participates in the same fixed point.

The public roots method conservatively lists stored keys and values for direct heap collection. It does not itself implement weak semantics. Use Interp::collect for weak collection and opaque host edges. That method computes reachability first and builds heap roots after pruning. Intrinsic constructors, prototypes and adders remain rooted after global deletion. Collection is explicit; no collection occurs during script execution.

All 1,365 workspace tests pass. JS debug and release pass 385 tests. The parent added a cross-script closure and typed-array backing-buffer cycle check. It verifies live values survive and the cycle is reclaimed after its key disappears. Tests cover host edges, forward and reverse ephemeron chains, private environments, async state, caps and reused slots. The child changed two collector hooks in scratch copies; the tests rejected both mutations. Tests were written alongside the implementation, not first against an empty implementation. Parent clippy initially found four test-initializer warnings; those are fixed and warnings-denied clippy passes.

Five authored weak-collection oracle programs and 38 existing collection programs match Node. They cover multiple operations per program. Node does not run fetched scripts. All 21 parent load rows pass under 1 GiB address space and 45 seconds per process. At 100x, 100k object entries take 192 ms to build/read and 49 ms for collection, at 45 MiB peak RSS. Forward and reverse 100k-link chains take 326/338 ms plus 235/218 ms collection at 88/75 MiB. Visits are about 4n/2n across the staged collections. Closure values take 451 ms plus 370 ms collection and 216 MiB. One million dead entries between explicit checkpoints take 596 ms plus 137 ms collection and 305 MiB. These are single samples under concurrent checks, not speed improvements. Reproduce with tools/js/weak_check.sh and tools/js/weak_load.sh with an output path.

The cached login check gets past WeakMap but still has one TypeError and no form. The source of React's failing vendor-prefix check reads document.createElement("div").style, which bare does not implement. This is not successful hydration. No live network, authentication, GUI or real-browser comparison was tested for this change. Heap-wide fuzz with weak entries and exotic objects, weak Symbol storage and native subclassing remain untested or unsupported.


## Promise combinators

Promise.all, allSettled, race and any reuse the intrinsic Promise queue and shared iterator engine. Each call reads the constructor resolve once, invokes then for each resolved input and preserves input order. A shared result resolver prevents repeated settlement. allSettled shares one called flag between its two callbacks per item. Empty all, allSettled and any settle immediately; their reactions still run as jobs. Empty race stays pending. Iteration errors reject the result. Ordinary resolve/then errors close the iterator; next errors do not. Resource errors run no close or rejection code.

Promise.any rejects with AggregateError and its ordered, non-enumerable errors array. The global AggregateError constructor supports an optional message and cause. It reads cause before the errors iterable. Custom Promise constructors and AggregateError subclasses remain explicit Unsupported gates. Existing Error constructors are unchanged.

Aggregate records hold values, remaining counts and called flags. Element functions name records by never-reused identifiers. Marking a live element function traces its result promise and values. Unreachable callback cycles drop their records, and live records preserve their resolver-once flags. Caps check record count, total slots and items per call before growth. Steps, queue, Promise, function and heap caps still apply. The public roots method remains conservative; use Interp::collect for precise metadata pruning.

Parent regressions failed before fixes for repeated tracing and ignored cause. A suspended 256-item aggregate traced 65,792 references before the fix and 257 after it. A per-collection visited-record set now traces each aggregate once. The constructor now defines cause with the correct descriptor and order. Three added Node cases cover inherited undefined cause, descriptor attributes and a throwing getter that stops error iteration.

The workspace run passes 1,379 tests before the final added browser integration test. JS debug and release pass 399 tests. The added browser test collects a pending cross-script aggregate whose result callback mutates a WeakMap-owned DOM node; its checks and the bare suite pass. Warnings-denied JS clippy passes after removing a single-element test loop left by the old gate checks. All 52 aggregate and 62 existing Promise oracle programs match Node. Node runs authored programs only.

The parent load harness now collects before draining jobs and asserts no aggregate metadata remains after final collection. All 21 runs pass at 1x/10x/100x under 1 GiB address space and 45 seconds per process. At 100k items, all takes 337 ms to set up, 143 ms for suspended collection, 109 ms to drain and 61 ms for final collection, with 168 MiB peak RSS. The thenable case takes 274/203/155/115 ms for those phases and 268 MiB. Race needs no aggregate record and peaks at 123 MiB. These are single samples with concurrent checks, not isolated speed improvements. Reproduce with the promise_all_load example and promise_oracle.mjs plus the promise_check example on promise-combinator-cases.txt.

No staging execution, authenticated login, GUI or real-browser comparison was tested for this change. The verified bootstrap blocker remains the missing element.style API. Automatic collection during execution, arbitrary Promise constructors, native subclass construction and full error stack behavior remain outside this change.


## Script-made readable streams

Default ReadableStream sources support start, pull and cancel, with a chunk-count highWaterMark strategy. Controllers implement enqueue, close, error and desiredSize. Default readers implement read, cancel and releaseLock. Pending reads are genuine Promises. Pull callbacks run one at a time, with pull-again state and queue backpressure. Source and controller state is held by generation-checked owners and traced through Host. The existing response.body facade still reads a fully buffered network response; no live socket streaming is added.

Host hooks bind callbacks without consulting script-visible bind and attach intrinsic Promise reactions. Start returns any, so the constructor resolves a fresh Promise with its result before attaching reactions. Pull and cancel use their Promise callback conversion. No RefCell borrow is held while user JS runs. Controllers, readers, chunks, callback captures and pending reads survive collection only while reachable. Source callbacks are cleared on completion or cancellation. A controller exposed by a throwing start remains usable until it becomes unreachable; failed constructions are not discarded while script still holds them.

StreamLimits bounds live streams and readers at 4,096 each, queued chunks at 16,384 per stream, queued buffer/view bytes at 64 MiB total, pending reads at 1,024 per reader and lifetime source callbacks at 4,194,304. Other queued values count as chunks, with their storage bounded by the heap. Enqueue work is charged by chunk and buffer size. Resource errors and Unsupported poison subsequent stream-layer operations. Gates include byte streams, BYOB, pipes, tee, reader.closed, custom size functions, stream bodies in Response and native subclassing. A default stream with no pull callback starts no unobservable pull cycle.

Five parent regressions failed before fixes. They cover null dictionaries and invalid modes, escaped controllers, start-result assimilation timing, sticky pipe gates and receiver branding before a BYOB gate. Three additional Node programs cover conversion, controller lifetime and assimilation; a fourth covers branding and a mode getter read once. All 42 stream and 35 existing Fetch oracle programs match Node. Node runs authored programs only. Parent tests also combine a timer source, pending Promise.all reads, WeakMap-owned controllers and DOM mutation across collection. A separate bootstrap example regression changed an invalid selector from a panic with exit 101 to a fixed diagnostic and exit 1.

The workspace passes 1,398 tests before the final receiver-brand test. The final bare suite passes 261 tests plus four HTTPS integrations. JS release passes 399 tests. The final 19 stream tests pass in debug and release. Clippy reports existing warnings only. The first workspace attempt failed an old gate test that ran independent Unsupported cases in one realm; it now uses a fresh realm for each case because stream gates are sticky. No runtime check was removed.

All 12 final release load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds per process. At 100x, 500k flowing chunks take 3.25 seconds wall and 3.23 seconds CPU, with 11.4 MiB peak RSS. Thirty thousand streams take 582 ms and 8.4 MiB; 300k pending reads take 1452 ms and 9.4 MiB; churn takes 289 ms and 7.5 MiB. Queue, read and byte checks pass. The harness now uses a temporary file for default output and checks that 12 successful rows exist. The first matrix, run during concurrent builds, took 6.2 seconds for flow. Two interleaved 10x comparisons took 320–340 ms CPU on the child baseline and 310–380 ms on the integrated build; no consistent timing change is established. Reproduce with tools/js/streams_check.sh and tools/js/streams_load.sh.

Not tested: real Next/Flight consumption, live fetch feeding script sources, a long soak, hard allocator OOM, other architectures or a real browser. No staging credentials, authentication or GUI were used. The cached app still lacks its style API on this branch. The completed style work has not yet been reviewed here.


## Bounded console records

The console namespace supplies the three observed methods: log, warn and error. Methods are writable, configurable, receiver-independent host functions. Saved, bound and patched methods record their calls. Each record holds a sequence, level and raw Values. Debug shows counts only. Nothing is automatically printed or persisted. Retained arguments are Host roots. Taking records transfers root responsibility to the caller, who must pass live Values to Browser::collect.

ConsoleLimits defaults to 64 arguments per call, 1,024 kept records and a 64 KiB format bound. The sink drops the oldest record when full and counts drops. Zero keeps nothing. Argument count is checked before capture, and capture charges one step plus argument count. The driver formatter bounds UTF-8 output bytes and UTF-16 text input units. It estimates exact lossy UTF-8 size before copying, charges scans and parses numeric prefixes from borrowed UTF-16. Symbol descriptions follow the same limits. Formatting returns text only to the caller.

Capture does not run getters, toString, valueOf or Symbol.toPrimitive. The optional data-only formatter is not the Console Standard Formatter: object substitutions use fixed tags rather than observable JS coercions. It supports primitive substitutions and a local percent-escape convention. It has no grouping, style rendering or object inspection. Other console methods remain absent. This is a scoped sink, not a devtools console or a claim of browser formatting equivalence.

Three parent regressions failed before fixes for unbounded numeric formatting input, uncharged Symbol descriptions and integer-prefix conversion. A cross-feature test verifies that a logged weak key keeps its stream controller and pending aggregate alive, then draining the record permits collection. All 1,419 workspace tests pass. Release passes 281 bare tests, four HTTPS integrations and 399 JS tests. All 20 console tests pass. Clippy reports existing warnings only. Four authored numeric-prefix expectations match Node parseInt. There is no Node or browser Console oracle, and no fetched script ran in Node.

The parent checked every literal console occurrence in the 15 cached files. It found only error, warn and log member operations, plus the saved/patched error shape and an apply receiver. This is source-text evidence, not a runtime audit of dynamically generated code. The reduced missing-console check previously returned undefined and TypeError. The implemented saved/patch shapes pass. The real app has not been rerun with pending style corrections, and no app-startup claim is made.

All 12 release load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds. At 100x, 300k calls take 219 ms wall, 210 ms CPU and 6.5 MiB peak RSS. Retaining 50k object records through collection takes 1805 ms and 63.4 MiB. Formatting 30k lines takes 69 ms and 11.1 MiB. Patched calls take 245 ms and 43.4 MiB. Counts, drops and independent checksums pass. These are single samples under concurrent builds, not speed improvements. Reproduce with tools/js/console_load.sh.

Not tested: real browser comparison, full app execution with the corrected styles, staging authentication, GUI, long soaks or non-Linux platforms. Console arguments can contain sensitive values; the sink intentionally retains them only in process. Callers must not publish formatter output or raw records without their own data policy.


## Reflection and construction

Object.getOwnPropertyNames returns own string keys, including non-enumerable ones. Reflect.ownKeys also includes symbols. Both use native key order and charge virtual byte-view indices before materializing them. Primitive strings expose UTF-16 indices and length without a wrapper object. Reflect.ownKeys requires an object. Other primitives have no own names; null and undefined throw TypeError.

Reflect.construct validates its target and a present newTarget before reading the array-like argument list. Explicit undefined as newTarget is an error. The list uses ToLength, the existing one-million-argument cap and a step precharge before allocation or index getters. Function.apply shares that list helper. Construction uses the existing new.target, class, bound-function and cross-script dispatcher. Async functions are no longer classified as constructors.

Parent regressions failed before fixes for uncharged bound-chain validation, native fallback prototypes and observable allocation order. Bound validation now ticks per link before the argument-list getter can run. Array and Error constructors read the prototype before their argument processing. RegExp reads IsRegExp first, then the prototype, then string conversions. Native objects are allocated with that prototype directly. A missing object prototype falls back to the target's intrinsic Array, error-type or RegExp prototype, rather than Object.prototype. Existing call/frame guards remain active.

Source evidence is static. In the cached files, Reflect.ownKeys appears at byte 77046 of 2117-562f130d3e4e3a40.js in a Proxy-related trap. Reflect.construct appears in fd9d1056-41ce205d8e0c6ad6.js at 7153 (guard), 7176 and 7217 (two call forms). Object.getOwnPropertyNames appears at byte 1101 of webpack-4af263207fca128f.js. The parent byte scan corrects the child's duplicate ownKeys entry and the webpack offset. These positions do not prove runtime reachability. Reduced authored React and webpack shapes are in the oracle corpus.

All 1,437 workspace tests pass. Release passes 417 JS tests, 281 bare tests and four HTTPS integrations. JS clippy with warnings denied is clean. All 23 reflection Node cases match. Existing class 107, builtins 90, evaluator 202, async 94, Promise 62 and weak-collection five cases still match. Node runs authored programs only. Cross-script new.target and collection are covered. Parent regressions also check prototype-getter errors before Array length errors.

All 21 parent load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds. At 100x, function/class construction takes 216/254 ms and 69 MiB peak RSS. Key enumeration takes 122 ms and 49 MiB. The view/string case takes 518 ms and 96 MiB. Nested and cross-script construction take 178/204 ms and 69 MiB. The 1000-argument-list case takes 29 ms and 5 MiB. Checksums pass, followed by explicit collection. These are single samples with concurrent builds, not speed improvements. Reproduce with tools/js/reflect_check.sh and tools/js/reflect_load.sh.

The Reflect object exposes only ownKeys and construct. Proxy and the rest of Reflect are absent. String/Number/Boolean construction has no wrapper objects and remains Unsupported. Native constructors without newTarget plumbing, including host constructors, collections, bytes, Promise and AggregateError, refuse differing newTarget. Function own keys omit legacy arguments/caller properties. Class field new.target remains a gate. Full bundle execution, staging authentication, GUI, other platforms and nested maximum-size argument lists were not tested. Batch 1 later added the rest of Reflect (see [Batch 1 built-ins](#batch-1-built-ins-array-string-object-reflect)).


## Native inline style declarations

HTMLElement.style returns a stable CSSStyleDeclaration over the native style attribute. The forwarding setter, cssText, property accessors and get/set/remove methods reuse the existing CSS declaration and value parsers. Supported shorthands track ownership of longhands. Reads preserve authored units where possible. Partial writes preserve unaffected longhands and unknown or engine-invalid declarations. Those opaque declarations stay hidden from CSSOM reads. Wrapper and declaration edges participate in Host collection.

StyleLimits defaults to 65,536 objects, 256 KiB per block/value and 8,192 raw declarations. Scans consume steps before parsing. The capped parser retains at most the cap and detects overflow with one discarded declaration. Serialization checks each append. Reconstructed shorthand reads check their output length before joining. Parent regressions failed before fixes for reconstructed output growth and the parser's previous cap-plus-one retention. Identifier escaping also checks remaining output space before allocation.

The original surrogate defect did not reproduce. All 13 tested string entry points already refuse lone surrogates without a write. That test passed before the follow-up changes. Eight follow-up regressions failed before fixes for shorthand ownership, priority, preservation and caps. Native cascade tests verify that accepted mutations change actual layout values. No browser or Node CSSOM comparison was run.

The final workspace passes 1,468 tests. Release passes 308 bare tests, four HTTPS integrations and 188 CSS tests. Clippy reports existing warnings only. All 12 release load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds per process. At 100x, 200k set/get operations take 748 ms wall and 10.8 MiB peak RSS. Detached-object churn takes 749 ms and 14.7 MiB; attribute reflection takes 610 ms and 10.7 MiB; shorthand updates take 2.12 seconds and 18.8 MiB. Checksums and native cascade checks pass. These are single samples during concurrent builds, not speed improvements. Reproduce with tools/js/style_load.sh.

The cached login executes 21 scripts and fetches 15 files totaling 469,525 bytes. It reports zero script throws and load errors, then stops at Unsupported("TypedArray.prototype.indexOf"). No form exists. The check exits 1. The vendor-prefix style probe now receives a real declaration object. No credentials, live staging requests or authentication were used in this check.

Known deviations: implied border width can read 3px rather than medium, and implied font weight can read 400 rather than normal. Some complex shorthand reads return an empty string after partial edits. Item order can move an edited longhand after its group. Partial mutation of an unresolved var() shorthand is Unsupported. Authored values are not browser-canonical serializations. There are no indexed declaration properties, SVG style, computed style or layout reads. Native text storage grows on each write and is bounded by the existing DOM cap. Real-browser comparison, GUI, full hydration and authentication were not tested.


## DOM focus foundation

HTMLElement.focus and blur update native focus state. Document.activeElement returns that element, or the body/document element fallback. FocusEvent carries a traced relatedTarget. Driver calls share the same event dispatch and checkpoint paths. Events are synthetic and isTrusted remains false. Ordinary dispatch of a FocusEvent does not change focus.

Focus changes dispatch blur, focusout, focus and focusin. Only focusout and focusin bubble. Events are composed and not cancelable. Reentrant listener calls supersede the previous transition. Parent regressions failed before fixes for a stale focusout after a blur listener moved focus, a relatedTarget getter accepting ordinary Events, and activeElement skipping receiver validation while focused. Native event branding now rejects prototype-only imitations. The document receiver is checked before focus state is read.

Focusability checks document connection, hidden/inert ancestors, closed details, disabled controls/fieldsets, native control types, links, tabindex and contenteditable. Fixup runs when focus is read, without events. CSS visibility and display do not participate. Focus is cleared before blur listeners run. This is an explicit local reentrancy policy, not verified browser equivalence. preventScroll is read but there is no scrolling; focusVisible is Unsupported. Transition nesting is capped at 16 and ancestor walks consume steps. Event storage uses the existing event cap and explicit collection.

No browser or Node DOM comparison was run. The child observed eight failing JS tests before implementation. Parent regressions also failed before their fixes. Selection, caret, Tab traversal, click focus, autofocus, shadow trees, window focus, change-on-blur, CSS focus selectors and FocusEvent.view remain absent. No actual staging bundle, GUI or authentication was tested.

The workspace passes 1,480 tests before the final document-receiver regression. The final bare suite passes 321 library tests and four HTTPS integrations in debug and release. All 13 focus tests pass. All nine capped release load rows pass with independent checksums. At 100x, 100k focus moves take 845 ms wall and 7.2 MiB peak RSS; synthetic dispatch takes 646 ms and 7.4 MiB; detached-node churn takes 1.25 seconds and 10.8 MiB. The churn case retains five wrappers and zero events after collection. Native nodes grow to 100,045 because the existing arena does not free nodes. These are single samples under concurrent builds, not speed comparisons. Reproduce with tools/js/focus_load.sh. The runner now verifies all nine successful rows and preserves output when stdout is a pipe.


## Byte-view index search

Uint8Array.indexOf validates the view first, returns early for an empty view and converts fromIndex once. Strict equality matches numeric byte values, including negative zero, without coercing the search value. The scan borrows the view after fromIndex conversion, so mutations from valueOf are visible. Each started 64-byte chunk costs one step before it is read. A match stops the scan early. There is no copied search buffer or allocation in the scan.

The child observed four failing tests and the new Node cases returning Unsupported before implementation. The parent reviewed the direct step-counter update against the existing saturating counter and limit check. Four new Rust tests cover matching, exact work charges, fatal limits and saved-method collection. All 93 binary Node cases and both adapter corpora (21 and 13 cases) match. Node ran authored programs only. The workspace passes 1,485 tests. Release JS passes 421 tests, with one existing ignored test.

The final cached bootstrap loads 15 files totaling 469,525 bytes and executes 21 scripts. It reports zero script throws and load errors, then stops at Unsupported("JSON.parse reviver"). No form exists and the check exits 1. This local loopback check uses the cached bundles, not staging authentication.

The load runner now checks all expected rows and uses temporary output for piped stdout. The harness reports successful checksums and uses an 8 MiB release VM stack, or 64 MiB in debug. Detached/resizable buffers, fuzzing, comparison with Array.indexOf, real staging, GUI and authentication were not tested. The harness retains its own input copy, so peak RSS includes that copy. Other typed-array method gates remain in place.

All 18 parent release load rows pass under 1 GiB address space and 45 seconds per process. The index search case uses 2/20/200 MiB at 1x/10x/100x. It takes 2.6/26/256 ms wall, with 7/44/404 MiB peak RSS. The 100x process uses 520 ms CPU including setup and collection. Its hit, miss and tail checks pass with 7,526,448 charged steps. These are single samples during concurrent builds, not a measured speed improvement. JS clippy with warnings denied is clean. Reproduce with tools/js/binary_check.sh, tools/js/adapter_check.sh and tools/js/binary_load.sh.


## Native namespaces and foreign content

The native document keeps sparse element namespace and prefix metadata. Missing metadata means HTML. Moves and clones preserve identity. Element namespaceURI, prefix, localName, tagName and nodeName read that identity. SVG has an SVGElement wrapper brand; MathML uses Element. createElement remains HTML. createElementNS supports HTML, SVG and MathML within explicit name/namespace restrictions. Attribute NS methods distinguish null, XML, XMLNS and XLink attributes. Null-namespace or arbitrary-namespace elements remain Unsupported. Name validation is a restricted subset, not complete current DOM name validation.

The HTML tree builder makes foreign elements with SVG name/attribute adjustments and integration points. Tokens::for_tree uses builder-selected raw-text/RCDATA and foreign CDATA context after each token. Standalone tokenization retains its previous tag-name behavior. Foreign CDATA preserves text without entity decoding. Formatting recovery respects foreign scope borders and special elements. Tests cover every cut point and pairs of cut points on selected mixed-namespace pages, character-by-character feeds and seeded junk. Fragment parsing is absent.

Parent regressions failed before fixes for HTML style/focus methods accepting SVG receivers, NUL removal from foreign text and plaintext not closing an open paragraph. Native namespace checks now reject borrowed HTML methods on foreign elements. The focus driver leaves SVG elements unfocused. HTML createElement('svg') still has HTML identity and can use HTML style. Foreign NUL becomes U+FFFD; standalone HTML tokenization still drops NUL. Plaintext closes an open paragraph. A separate regression confirms that SVG and MathML start tags keep a paragraph open; that check already passed and required no change. The child reported this as a remaining defect, but the final source has no close_p call in that branch.

Dynamic SVG script insertion and src mutation queue no script execution or fetch. The parent loader regression passed without a runtime fix because both discovery and dynamic insertion already check native identity. This check covers script inertness only; no SVG scripting implementation is claimed. The load runner now runs all five cases, checks 15 successful rows, preserves piped output and uses a real deadline instead of the shell-specific SECONDS variable.

Known gaps: no fragments, CSS namespace selectors, foreign mixed-case type-selector matching, SVG layout/drawing, SVG style, MathMLElement brand or Attr nodes. Native nodes, names, namespace metadata and text remain until document teardown, bounded by existing caps. No Chrome/browser comparison, GUI, application execution, authentication or non-Linux validation was done for this integration. Handwritten tests follow selected HTML and DOM rules, not a full browser conformance suite.

The parent workspace passes 1,532 tests. Release passes 339 bare tests, four HTTPS integrations and 131 HTML tests. Namespace tests and the dynamic SVG-script regression pass in debug and release. All 15 capped release load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds per process. At 100x, SVG construction takes 2.59 seconds wall and 46.2 MiB peak RSS. Foreign parsing takes 208 ms to parse and 512 ms to read, with 60.6 MiB peak RSS and 60,006 wrappers. Checksums pass. These are single samples during concurrent builds. No before/after throughput comparison was made. Reproduce with tools/js/browser_load.sh.


## JSON reviver traversal

JSON.parse accepts a callable reviver after parsing into managed values. An explicit stack performs the ES2022 InternalizeJSONProperty walk. It snapshots object keys and array lengths, reads values just before visits, calls the reviver with holder this and string key, deletes undefined results and defines plain replacement properties. Refused deletes/definitions do not throw. The synthetic root holder uses the empty key and its callback result is returned. Callbacks run through the existing function dispatcher and can re-enter JSON.parse.

The child observed failing authored Node and Rust cases before removing the Unsupported gate. All 52 authored Node cases now match. The implementation passes two arguments. Newer source-text context is absent; this is an ES2022 compatibility limit, not an explicit Unsupported error. Arguments-count/source-context equivalence with current Node is not claimed.

The existing JSON depth/node caps bound open frames, visits and key snapshots. Parent review found that callback-injected byte views could materialize virtual keys before the node-cap check. A regression failed with Steps before the fix and now gives Json(Nodes) before key allocation. Heap::own_key_slots supplies a constant-time upper bound including dense holes, removed slots and virtual indices. The walk checks that bound, charges the scan and filters the key list in place. This conservative bound can reject hole-heavy objects before their live-key count reaches the cap. Limits remain uncatchable; no callback, catch or finally runs after one. No collection runs inside a synchronous traversal.

The workspace passes 1,544 tests. JS debug and release pass 433 tests, with one existing ignored test. JS clippy with warnings denied is clean. All 15 release load rows pass at 1x/10x/100x under 1 GiB address space and 45 seconds per process, on an 8 MiB VM stack. At 100x, shallow traversal takes 620 ms wall/CPU and 191.2 MiB peak RSS. The deep case takes 14 ms and 11.3 MiB. Explicit-GC rounds return to the 247-object baseline. Callback-created cycles stop at Json(Depth). These are single samples during concurrent builds. Reproduce with tools/js/revive_check.sh and tools/js/revive_load.sh.

No source-context support, Proxy behavior, allocator OOM, real-browser comparison, GUI, live staging authentication or cross-platform validation was tested. Node runs authored tiny programs only.

The final integrated cached run reports Page(Settled), 22 tasks, 21 scripts, zero script throws/load errors and one unhandled rejection. It fetched 15 files totaling 469,525 bytes. The required form is absent and the check exits 1. Settled describes the queue state only. A temporary bare-only probe read the rejected promise reason: TypeError, Array(12).fill is not a function. The probe was removed. This identifies the next runtime blocker; successful hydration and authentication are not established. The bootstrap acceptance check now also requires zero unhandled rejections.


## Array fill

Array.prototype.fill reads length once, converts relative start/end bounds in order and reserves traversal work before the first write. Each visited index uses the existing strict Set path, so holes, inherited setters, receiver mutation and non-writable properties follow ordinary property rules. It returns the receiver. Nullish receivers throw. Other primitives remain Unsupported because wrapper objects are absent. TypedArray.fill remains a separate gate.

Six child tests failed before implementation. The parent added a load case with an independent Rust array model and array-like receiver checks. A cross-script setter survives collection before its Promise job runs. A huge fill inside a job stops before writes with Steps; finally does not run and the next checkpoint repeats the failure. No dense fast path or new heap shortcut was added.

No wrapper-object behavior, Object.freeze/Proxy coverage, live staging authentication, GUI or cross-platform behavior was tested. Node runs only authored cases. Existing property descriptors are used to test write failures without Object.freeze. `Object.freeze` exists since batch 1.

The parent workspace passes 1,551 tests. Release JS passes 439 tests before the added interaction test; that final test passes separately in debug and release. JS clippy with warnings denied is clean. All 217 authored Array/Object Node cases match. All 18 capped load rows pass under 1 GiB address space and 45 seconds, on an 8 MiB release VM stack. Fill uses 4k/40k/400k elements at 1x/10x/100x. It takes 3.1/30.9/315.5 ms wall and 4.2/6.4/29.6 MiB peak RSS. The model checks array contents, relative ranges, receiver identity and array-like writes. These are single samples, not a measured speed improvement. Reproduce with tools/js/arrobj_check.sh and tools/js/arrobj_load.sh.

The final cached run reports Page(Settled), 22 tasks, 21 scripts, 15 fetches and 469,525 script bytes. Script throws, load errors and unhandled rejections are all zero. The form remains absent and the check exits 1. The previous fill rejection is resolved; the remaining hydration failure is unclassified. No authentication was attempted.


## Hydration globals and labeled control flow

Cached execution exposed missing parseInt in the Flight row client, then missing encodeURIComponent in React's minified error formatter. The formatter failure hid React error 306. A reduced control-flow case found that a label on a block leaked into a nested loop or switch. Pending labels now apply only to directly labeled statements. Four new authored cases match Node and the existing evaluator corpus.

Global parseInt performs string conversion before radix conversion, reads the longest valid digit prefix and preserves negative zero. The parent found a hexadecimal rounding regression: iterative floating-point accumulation rounded a large integer twice. Power-of-two radices now reuse the existing retained-bits/sticky-bit conversion. The decimal test value matches Node. Other radices retain approximate floating accumulation. encodeURIComponent encodes UTF-8 and throws URIError for lone surrogates. URIError joins the existing Error prototype set.

Parent regressions failed before fixes for rounding and uncharged global scans. Both globals now charge input work before scanning. URI encoding preflights exact ASCII output size against the string cap before allocation. The implementation still has no parseFloat, isFinite, encodeURI, decodeURI or decodeURIComponent. No general URI API completeness is claimed.

JS debug and release pass 443 tests with one existing ignore. All 206 evaluator Node cases match. JS clippy with warnings denied is clean. Six fresh capped release load runs cover 1k/10k/100k iterations, with independent numeric and encoded-length checksums. At 100k, parseInt takes 130 ms wall and 11.2 MiB peak RSS; URI encoding takes 148 ms and 4.0 MiB. The release VM uses 8 MiB stack. These are single samples, not before/after speed claims.

The child's cached bare-only trace next reached Date.now in the prefetch cache and reported Date absent. It also observed an undefined HTMLIFrameElement in a reduced React commit probe. Date support and native iframe branding are separate work. Error.stack is absent and React catches its probe error. React hydration error 418 was observed but not diagnosed. Temporary tracing was removed. No fetched bundle ran in Node, and no GUI, live staging authentication or cross-platform validation was performed.

The parent workspace passes 1,554 tests. No workspace failure occurred in this integration pass.

The parent cached run settles after 23 tasks and 21 script executions, with 15 fetches and 469,525 bytes. It has zero script throws, load errors and unhandled rejections, but reports one listener TypeError and still has no form. The check exits 1. This run does not identify the listener statement; the Date/iframe findings above come from the child's removed instrumentation.


## Resumable input and selection

Session.resume takes click, type and submit selectors through the existing Browser drivers, then runs the loader's existing task loop in the same realm. It does not reload the first document. Navigation drops the old realm and shares origin storage with the new document. Session budgets carry across calls. The parent found a per-page reset loophole: repeated resumes renewed the page's task allowance. A regression failed before the fix. Page task, round and time allowances now also retain their spent amounts. Time counts driver execution, not idle time between calls. A fresh Session.run deliberately starts a new run.

Missing or invalid selectors give NoTarget at the step index and leave the session usable. Fatal, resource and budget stops remain sticky for resume. Steps after navigation do not run in the old realm. Raw Session.browser access remains a host escape from the driver budgets; callers must use resume for bounded interaction.

Selection state belongs to native HTML inputs. Text, search, tel, URL and password inputs use UTF-16 offsets; other types return null and reject range setters. Input value updates reset the caret when the value changes. Clones copy value, not selection. Focus and blur preserve selection. select tasks are distinct from coalesced per-input selectionchange tasks. The pending flag clears before dispatch, so a listener can schedule the next change. Both use the existing scheduler and event engine. Queue caps are checked before state writes.

The follow-up corrected missing selectionchange and scans that previously copied values before work checks. Values are borrowed and UTF-16 scans are charged first. Test counters verify no scan occurs under an insufficient budget. A parent namespace regression failed because an SVG element named input reached the native driver. The common input predicate now uses native HTML identity. Direct driver reads have no JS step budget and remain bounded by DOM text caps.

HTMLIFrameElement is a native prototype brand for HTML iframe nodes, including parsed, created and cloned nodes. It inherits HTMLElement; foreign elements do not get the brand. The reduced React focus probe passes without a fake global. No nested context or contentWindow/contentDocument exists. Iframe src/srcdoc remain plain attributes, and no child document is fetched. Seven tests failed before implementation. No real React render or browser comparison is claimed by those reduced tests.

Known selection gaps: textarea, setRangeText, document selection, onselectionchange, caret/layout/keyboard selection and attribute/type-change notifications are absent. Selectionchange versus select order is an implementation inference, not a browser-verified ordering claim. No GUI, authentication, live staging or cross-platform validation was run.

The combined parent workspace passes 1,601 tests. The final bare library passes 386 tests with one ignored load test; four HTTPS integrations also pass in release. Six selection load rows pass under 1 GiB address space and 45 seconds on an 8 MiB release VM stack. At 100x, 100k short operations take 653 ms and 50.2 MiB peak RSS; long operations take 1.87 seconds and 50.8 MiB. Queues and events return to zero. Three resume load scales (100/1,000/10,000 rounds) pass with independent storage sums and loopback request counts. The largest takes 2.07 seconds and makes 1,000 fetches without reloading the document. These are single samples. The iframe tests verify zero child-document fetches and reduced focus-probe behavior; no iframe rendering load claim is made. Reproduce with tools/js/selection_load.sh and BARE_RESUME_LOAD=N cargo test --release -p bare --lib resume_load -- --ignored --nocapture.

## Date

The cached app calls `Date.now()` (prefetch, react-query, toaster) and the page threw `ReferenceError: Date is not defined`. Date is now a real native constructor in `bare-js`. It has a prototype, a brand (a time value per object in `Interp::date.times`) and UTC arithmetic from ECMA-262 section 21.4 (`crates/js/src/date.rs`, no heap, no clock, no crate).

Covered: `new Date()`, `new Date(value)`, `new Date(dateObject)`, `new Date(string)`, `Date.now`, `Date.parse`, `Date.UTC`, `getTime`, `valueOf`, `setTime`, `toISOString`, `toJSON`, `toUTCString` (and `toGMTString`, the same function), `@@toPrimitive`, the 8 `getUTC*` getters and the 7 `setUTC*` setters. TimeClip, MakeDay, MakeTime and MakeDate follow the standard. Leap years, negative epochs, `+YYYYYY` years, `24:00`, offsets and ISO fractions are covered. Strings: ISO 8601 as the standard writes it, and the `toUTCString` form. A fraction may have any number of digits: the first three count, as in V8.

Gates (`Unsupported`, which a script cannot catch). bare has no time zone database, so nothing is local. `new Date(y, m, ...)` with 2 or more arguments, `Date()`, an ISO date-time with no offset, and the local getters, setters and string methods are gated for a valid date. On an invalid date the getters give NaN and the string methods give `Invalid Date`, since no zone changes that. They are real functions, so feature tests work. `"" + date` and `date + 1` call `toString`, so they are gated too. `date - 1`, `+date` and comparisons work. A string with a digit that is not ISO and not `toUTCString` form (`Jan 1 2020`, `2020-1-1`, `+0100`) is gated. A string with no digit is NaN. A subclass (`class extends Date`, `Reflect.construct(Date, [], X)`) is gated by the existing native-constructor rule, because `new.target` is not passed.

Differences from V8, on purpose. A field out of range in a known format is NaN (`2021-02-29`, `Sat, 30 Feb 2020 ...`); V8 rolls it over. Every present argument of a `setUTC*` call is coerced, also on an invalid date; V8 coerces the first only. The setters use the time value read before the coercion; V8 reads it after. A `toUTCString` text with a negative year parses back (the standard asks for it); V8 returns NaN. Error messages for a wrong receiver are one text (`this is not a Date object.`).

Clock. `WallClock::System` (`SystemTime`, may go backward) is the default. `Interp::set_wall_clock(WallClock::Virtual(ms))` fixes it at a whole number of ms within +-8.64e15 until the next call; a bad value is `Unsupported` and changes nothing. `Browser::use_virtual_wall_clock_at`, `use_system_wall_clock` and `wall_clock` call it. The wall clock is not the scheduler clock: `performance.now()`, timers and `use_virtual_clock` count from the time origin and never move it, and it never moves them. A test that wants both to move sets both.

Caps (`Error::Limit(LimitKind::Sc(..))`, all checked before the thing grows). `ScLimits::max_dates` is 1,048,576 tracked Date objects, checked before the object is made. `ScLimits::max_date_string` is 128 UTF-16 units, checked and charged as steps before the text is read. One call can make at most `max_dates` Dates, because the collector does not run inside a call; the load tool works in batches for this reason.

Collector. The prototype and constructor are roots, so `delete globalThis.Date` does not lose them. `Interp::collect` drops the time value of each Date it did not reach. A handle carries a generation, so a reused slot never finds an old time value. A Date held by a Map key, a WeakMap value, a closure, a Promise reaction or a timer argument survives.

Tests. The Node corpus has 61 UTC cases (`tools/fixtures/js/date-cases.txt`). Its expected file is the same under `TZ` UTC, America/New_York and Asia/Kolkata. Cases where V8 differs from the standard are Rust tests. 31 js tests and 7 browser tests were added. The corpus failed with `Date is not defined` before the implementation. Reproduce with `tools/js/date_check.sh` and `tools/js/date_load.sh`.

Load (release, 8 MiB VM stack, 1 GiB address space, 45 s per process, 21 rows, single samples). The expected values come from a second calendar algorithm (Julian day numbers) in `crates/js/examples/date_load.rs`. `utc` is 20k/200k/2M `Date.UTC` calls: 22/220/2165 ms. `parse` is a round trip of `toISOString` and `Date.parse` at the same sizes: 26/246/2529 ms. `iso` hashes the text of the same dates: 222/2161/24220 ms; the JavaScript hash loop is most of that, so 100x is within 2x of the 45 s cap. `gc` shows the object count returning to the baseline after each batch. `clock` steps the wall clock forward and backward 500k times at 100x. `cap` ends in `Sc(Dates)` with 1M Dates (605 ms, 153.6 MiB peak RSS at 100x). `longstr` ends in `Sc(DateString)` in 0.2 ms. All rows pass.

The child did not test a real browser, any bundle, local time, `Intl`, a system clock that is outside 1970 to 275760, the stress crate, GUI and live staging authentication.

Parent integration keeps the existing selection scheduler fields and the wall-clock hook. A Date primitive hint now matches borrowed UTF-16 literals and does not allocate a copy of an arbitrary hint. This structural bound change preserves the TypeError behavior; no failing allocation regression was demonstrated. The first parent debug check found a test trying to set the scheduler to 5 ms after more than 5 ms had elapsed. The test now freezes the current scheduler time and uses a tolerance for floating subtraction. The first release JS pass found an old class test whose fixed 300-object cap could no longer install the expanded intrinsic set. It now measures the intrinsic baseline and allows 100 more objects. No runtime limit was removed.

MakeDay retains conservative V8-style year/month implementation bounds. Oversized fields can be rejected even when mathematical cancellation would return them to range. Full arbitrary-field arithmetic and local-time compatibility are not claimed.


Final parent checks pass: 1,638 workspace tests; release passes 393 bare tests, four HTTPS integrations and 473 JS tests. All 61 UTC oracle cases match, and JS clippy with warnings denied is clean. All 21 capped release load rows pass. At 100x, 2M UTC operations take 2.39 seconds, ISO hashing takes 23.63 seconds and parse round trips take 2.72 seconds. The object-cap run retains 1M Dates at 153.7 MiB peak RSS and ends in Dates. These are single samples, not speed comparisons.

The parent cached login reaches Fatal(Limit("Steps")) after 22 tasks and 21 script executions. It fetches 15 files totaling 469,525 bytes, with zero script throws/load errors and zero unhandled rejections. One listener TypeError is reported and no form exists. The check exits 1. The step budget stayed enforced; the cause of the repeated work is not established. No live staging request or authentication was performed.


## Error stack snapshots

New Error objects receive a writable, non-enumerable, configurable stack string. Error subtypes, AggregateError, interpreter errors and host-created errors share the capture path. Frames come from actual script, function, constructor, class and async execution. Microtask jobs start at their own frame base. Script slots identify code; line and column use UTF-16 coordinates from one. Missing positions are omitted. Native frames, inferred method names, source maps and V8 formatting hooks are absent.

Snapshots keep at most ten frames and 2,048 UTF-16 units, with a 512-unit header. Frame work and source-location scans are charged before scanning. Frame suffixes do not capture argument values, source bodies or URLs. The header still contains the supplied error message. Only the stack key is a permanent root; frame state unwinds and the final string lives on the error object.

Parent review found that an environment-limit failure could leave a constructor marker set. The next ordinary call was then printed as new. The regression failed before the marker was consumed ahead of allocation; it now passes. The workspace passed 1,652 tests before that final regression. Final JS passes 488 tests in debug and release, and release bare passes 393 tests plus four HTTPS integrations. JS clippy with warnings denied is clean. The first release bare pass failed the previously observed loopback fetch-reuse test; the subsequent full release pass succeeded. The socket failure's cause remains unresolved.

Three new release load processes run 1k/10k/100k errors under 1 GiB address space and 45 seconds on an 8 MiB VM stack. They check frame shape, header, text cap and collection. Run time is 3.9/36.8/352.4 ms and peak RSS is 4.8/10.6/69.1 MiB. All return to 304 live objects after collection. These are single samples, not speed improvements. Reproduce with cargo run --release -p bare-js --example stack_load -- N. Exact V8 text, nested host checkpoints, GUI, live staging authentication and other platforms were not tested.

A separate bare-only cached trace identified the current hydration step exhaustion. React commit reads missing Element.attributes and removeAttributeNode. The thrown listener TypeError leaves React's commit context set, so sync work retries until the step budget ends. A temporary empty-attributes probe stopped the spin but did not render a form and exposed missing link.rel reflection. The probe was removed; no stub is integrated. Live Attr/NamedNodeMap and link reflection need implementation. This finding is independent of the Error.stack checks.


## Link rel reflection

HTML link nodes get an HTMLLinkElement prototype that inherits HTMLElement. Parsed, created and cloned nodes keep their native brand. SVG and MathML links do not receive it. The interface has the same illegal-constructor policy as other native HTML interfaces.

rel reads the native null-namespace attribute. Absence gives an empty string, and present values keep their case and whitespace. The setter runs ToString before changing native state. Removal, direct attribute writes, namespace writes, clones and moves remain visible. A namespaced attribute with the same qualified name does not become rel. The parent getter borrows the native value and checks the string cap before copying it; this is a structural allocation-bound improvement, with no failing allocation regression demonstrated.

The child observed nine tests fail before implementation. The parent added a namespace-isolation test. All ten pass. The workspace passes 1,662 tests; the final bare library passes 403 in debug and release, and release also passes four HTTPS integrations. Clippy reports existing warnings only. All 18 release load rows pass under 1 GiB address space and 45 seconds per process on an 8 MiB stack. rel takes 9.6/105/1081 ms wall at 1x/10x/100x, with 7.0/7.7/14.1 MiB peak RSS. Native node/text storage grows within existing caps; 1,005 wrappers remain after collection. Checksums cover writes, reads, removal and cloning. These are single samples, not speed comparisons. Reproduce with tools/js/browser_load.sh.

No relList, href reflection, new stylesheet fetching or layout hook is added. Node has no DOM and was not an oracle. No Chrome comparison, cached-app rerun, GUI, live staging authentication or other-platform test was performed. The earlier live attributes blocker is still pending; the reduced rel expression alone does not prove hydration.


## Live attributes and native roots

Element.attributes returns a live SameObject NamedNodeMap over the native attribute list. Attr wrappers retain identity while attached. The document logs removal of watched attributes, so a detached Attr keeps its old value. Re-adding the name creates a distinct Attr. Element and map methods support get, set and removal by qualified or namespace/local name. Document creates detached attributes. Indexed reads and iteration use the existing heap and iterator engine. Named map properties such as map.id remain absent. Prefix-changing replacement is Unsupported.

The heap owns an Rc<dyn IndexedSource>. Dropping or replacing the source releases it. This removes the thread-local owner and its cross-thread empty-answer failure. Every Heap is now non-Send, including heaps without a host. Build the heap inside its execution thread. No source or a borrowed host/document returns Heap(HostUnavailable), never an empty map. Indexed fill retries are bounded at two. Wrapper, native text, name and attribute caps apply. Attached Attr wrappers remain reachable from their element. Native arena storage does not shrink.

Node.getRootNode reads the options dictionary, then walks native parents with step charges and the existing depth cap. Document returns itself. A detached tree returns its top node. Attr returns itself even while attached. There are no shadow roots. The parent observed the Attr regression fail with Illegal invocation before adding its branch. Source-ownership tests were written after the fix; the original Attr and root-node workers observed their API tests fail first.

The assembled workspace passes 1,717 tests with six ignored. Release bare passes 442 library tests and four HTTPS integrations. Release JS passes 500 tests with one ignored; HTML passes 133. The non-Send compile-fail doctest passes. Clippy reports warnings in unchanged code only. Ownership checks cover source replacement/drop, two independent browsers, 100 in-process browser create/drop cycles and explicit failure under active borrows.

Reproduce the attribute matrix with tools/js/attr_load.sh. All nine fresh release processes pass their Rust-model checks at 1x/10x/100x under 1 GiB address space and 45 seconds per process on an 8 MiB VM stack. The runner uses a temporary output file and checks that all nine success rows exist. These are correctness and resource checks, not comparative speed measurements. Separate debug load scales were not run by the parent.

The cached public login runs only in bare over loopback. It settles after 22 tasks, with 21 scripts, 15 script fetches, 469,525 script bytes, zero script throws, zero load errors and zero unhandled rejections. The form is absent. Before getRootNode, console errors included React 446 (resourceRoot) and 423 (client-render fallback). After it, the first console error is parameter 1 is not of type Node at script 7, line 1, column 99866. The cached React source there removes firstChild after assigning a script fragment to div.innerHTML. A reduced native-host probe confirms that innerHTML is undefined and that the assignment leaves firstChild null. This identifies the next platform gap; it does not establish that fragment parsing alone will complete hydration. Temporary probes were removed. No bundles ran in Node.

No Chrome comparison, GUI, live staging authentication, other-platform run or complete hydration claim is included.


## Native HTML fragments

Element.innerHTML is a native getter/setter. The setter converts the value before borrowing the document, parses with the existing tokenizer/tree builder, measures the result, and checks destination node/text/attribute/name/depth limits before replacing children. Held wrappers of detached children remain valid. Import preserves namespace identity. The getter walks iteratively and escapes native text and attributes. Text/comment/attribute scans are charged before scanning. A step-limit test already passed before that charging change because the final string conversion also charges; it does not prove pre-scan ordering. The earlier charge is a structural correction.

The fragment parser starts in the context's tokenizer state with a temporary html root and in-body rules. It gates table-family, select, template, html, frameset/frame, noscript and foreign context elements. Template/noscript/frameset/frame markup is gated. Nested table/select markup retains the page parser's existing limitations. There is no DocumentFragment, outerHTML or insertAdjacentHTML surface. Fragment source has an explicit cap. Temporary node/text/name/attribute/depth caps are checked after each token, so one token can overshoot an individual cap before rejection. These are not strict pre-allocation caps. Joined-text storage can reach the text cap early. Destination changes occur only after preflight. Native arena storage is not reclaimed.

Fragment-created scripts are marked already started. The parent expanded the inert-script regression with deep subtree and direct script clones. It failed with inline execution and a real loopback script fetch. Cloning now copies the flag, as required by the HTML script cloning steps: https://html.spec.whatwg.org/multipage/scripting.html. The regression passes, including zero script fetches. The child observed 21 original API tests fail before implementation.

Release bare passes 465 library tests and four HTTPS integrations; HTML passes 140. All nine parent release load rows pass on an 8 MiB VM stack under 1 GiB address space and 45 seconds per process. At 100x, parse/replace/deep take 392/221/982 ms and peak at 73/38/71 MiB. Each checksum comes from a Rust model. Child debug loads passed all nine rows on a 64 MiB stack. Clippy reports warnings in unchanged code only. The first parent workspace run failed the previously observed bare-net socket-reset test; its cause remains unresolved. The full workspace rerun passes 1747 tests.

The bare-only cached login now reaches Fatal(Unsupported("subclassing a host constructor (new.target is not passed to the constructor)")) after 21 tasks, 21 scripts and 15 fetches. Script throws, load errors and unhandled rejections are zero, and the form is absent. The exact host constructor has not been identified. No bundle ran in Node. No Chrome comparison, live staging authentication, GUI or other-platform result is claimed.

## URLSearchParams subclasses

URLSearchParams now opts in to native host construction with a differing new.target. The evaluator exposes host_ctor_with_new_target and host_proto_from_new_target. Existing host_function and Host::call signatures stay unchanged. Other host constructors keep their explicit subclass gate. New params objects keep real native list state and brand checks; their prototype comes from the derived constructor. URL.searchParams keeps its intrinsic prototype.

The host call saves and restores its construction target on every return, including resource and unsupported errors. Plain host calls see no target. Nested constructors see their own target. The existing call-depth and step guards remain in the construction path. The target is listed in interpreter roots while active; collection is refused inside a call.

Web IDL converts constructor arguments through overload resolution before internally creating the object and reading newTarget.prototype. A non-object prototype falls back to the interface prototype in the constructor's realm. Bare uses its existing prototype helper. The source is https://webidl.spec.whatwg.org/#es-interface-call . Node's JavaScript implementation differs on getter order and non-object fallback, so those cases use standards expectations rather than a Node match claim. The child's eight other authored Node cases matched; that ad hoc corpus was not committed.

The child observed nine of ten new browser tests fail before implementation. Four new interpreter tests cover opt-in, nested construction, failure cleanup, depth and cross-script calls. The parent added an actual failed-checkpoint test. A prototype getter in a Promise job hits the step limit; a second drain repeats the error, the getter runs once, and a later job runs zero times. That integration test passed on its first run. No failing-first claim is made for it.


Parent validation: the workspace passes 1,761 tests; the separate added checkpoint test passes. Release bare passes 476 library tests plus four HTTPS integrations; JS passes 504 with one ignored. Clippy reports existing warnings only. Three capped subclass load processes at 1x/10x/100x pass with Rust checksums, 1 GiB address space and 45 seconds per process. The child's corresponding peaks were 6.9/7.0/8.0 MiB and times 9/87/885 ms. These are single samples, not comparative speed claims. The navigation oracle wrapper and full navigation load matrix were not rerun.

The parent reran the cached public login only in bare over loopback. It settles after 22 tasks with 21 scripts, 15 fetches, 469,525 script bytes, zero script throws, zero load errors and zero unhandled rejections. No form appears. Temporary console inspection identifies Number.isNaN is not a function at script 10, line 1, column 4859, followed by React 423. The diagnostic probe was removed. No Chrome, GUI, live authentication or other-platform test is claimed.

## Number.isNaN

Number.isNaN returns true only for a numeric NaN value. It never converts its argument. Missing arguments, strings, symbols, objects, functions, finite numbers and infinities return false. The existing native path supplies length 1, name isNaN, writable/non-enumerable/configurable property flags, the constructor refusal and step/call guards. Global isNaN still converts its argument.

The child observed four Rust tests and seven authored Node cases fail before implementation. All 508 JS tests pass in the parent debug run. The parent added a Symbol.toPrimitive getter test; it returns false with zero reads. That case passed on its first run. Objects are covered by the actual test source, despite the child's report listing them as untested. The child reports a passing workspace run, debug/release JS tests, clean JS clippy and 224 matching Array/Object corpus cases. Existing numeric, collection, builtins and evaluator oracles also passed in the child. Only tiny authored programs ran in Node.

The child's isNaN load checksum is the iteration count. Six capped release/debug runs at 1x/10x/100x pass, with about 4 MiB release and 6 MiB debug peak RSS. Reported per-iteration overhead is 720–744 ns in release and 7.6–7.8 microseconds in debug, each from single runs against an empty loop. No speed improvement is claimed. Wrapper Number objects remain unsupported. Payload-specific NaN bit patterns and other platforms were not tested.


Parent release JS also passes 508 tests; JS clippy with warnings denied is clean. The cached public login runs only in bare over loopback. It settles after 22 tasks with 21 scripts, 15 fetches, 469,525 script bytes, zero script throws, zero load errors and zero unhandled rejections. No form appears. The next console error is window.getComputedStyle is not a function at script 16, line 1, column 29329, followed by React 423. The temporary console probe was removed. No GUI, Chrome comparison or live authentication was tested.

## Native computed style

getComputedStyle returns a live read-only declaration over the native cascade. A declaration retains its element. Each read rebuilds computed styles from the current native document; parsed rules are cached only while source inputs and viewport match. Inline, embedded and supplied external author sheets use the same cascade as native rendering. CSS direction now has an inherited ltr initial value, ltr/rtl keywords and UA dir/tel rules. No fixed JavaScript style object or direction stub is used.

The embedder supplies flattened external sheets through set_external_stylesheet(href, parts), keyed by the exact href text. style_availability reports missing sheets. An absent sheet refuses computed reads; an explicitly supplied empty list means a failed load with no rules. The page loader does not yet populate this API. No stylesheet network request was added in this integration.

The parent found two defects. A limited script could run an expensive cascade because the work cap counted nodes times rules but the step charge counted nodes plus rules. The regression failed before the full work estimate was reserved ahead of compute. Ancestor walking now charges before each step. Enumeration listed native property order and omitted custom properties. Its regression failed before item/length were changed to sorted longhands followed by live custom properties. CSSOM specifies that ordering: https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle . The earlier item-order expectation was replaced because it pinned the defect.

The work estimate is conservative accounting, not a cycle-accurate bound on selector matching. Node, source bytes/count, external bytes/count, rule and work caps bound the operation. Rule construction is bounded by source bytes, with rule/name counts checked after parsing. Each property read still costs a full cascade. No computed Styles cache spans reads. The default nodes-times-rules cap refuses the largest load before the cascade. Native arena storage remains retained.

Layout-dependent resolved values, pseudo-elements, approximate native value representations, dir=auto/bdi and computed cssText remain Unsupported. Width/height/margins of rendered elements and similar values are not invented from computed lengths. Properties unavailable in the engine are absent. No layout, animation or complete CSSOM claim is made. Disabled stylesheet handling and external import flattening remain embedder limitations.

The child reports passing workspace, release bare/CSS and clippy checks. It did not observe the host tests fail before implementation; its direction test failed with UA rules disabled, and a mutation check caught ignored missing sheets. Parent workspace passes 1,786 tests before the enumeration regression was added. The final computed subset passes 20 tests with one ignored; parent release bare/CSS also pass. Clippy reports warnings in unchanged code only. Three parent release model processes run under 1 GiB address space, 45 seconds and an 8 MiB VM stack. Scales 1 and 10 run 82 cascades over 608 and 6,008 nodes in 16 ms and 761 ms. Scale 100 refuses before compute in 16 ms. Checks use a Rust model. These are single samples, not comparative speed claims.

The bare-only cached login reaches Fatal(Unsupported("computed style needs an external stylesheet that the embedder has not supplied (Browser::style_availability lists them)")) after 21 tasks and 21 scripts, with 15 script fetches, zero throws/load errors/unhandled rejections and no form. No empty sheet was supplied to bypass this gate. No Chrome, GUI, live authentication or other-platform test is claimed.

## Session stylesheet loading

The page driver supplies active linked stylesheets and embedded-sheet imports to the native cascade. It runs before the first blocking script, before resumed input and at later task boundaries. No network operation runs inside getComputedStyle. Same-task link insertion followed by a computed read remains an explicit gate until the next driver boundary.

Downloads reuse the page client and resources::fetch. Status, text/css MIME, charset decoding, content encoding, body/retained bytes, fetch count, deadline and redirect policy are checked. CSS counters distinguish successful blank sheets from real failures. Failed loads supply no rules only after an actual failed attempt, emit sanitized diagnostics and fire error. Resource caps stop the driver; they do not become empty sheets. The cache is per document and keyed by resolved URL. Native computed inputs use the raw href key. Imports flatten depth first with layers, bounded depth/references/parts/bytes and cycle detection. Navigation discards the old cache. Resume keeps resource counters.

Parent review found that redirected CSS imports resolved against the original URL. The regression failed with an incorrect child request and ltr result. Resource now retains its final URL, and import flattening uses it. Discovery errors were swallowed; a failing cap regression now stops before scripts or requests. Sheet discovery no longer uses the full-cascade node cap: ordinary large documents can discover sheets without allocating a computed Styles table. The cascade node cap still guards computed reads. An updated computed test pins that distinction. A media regression failed because an inactive link got load. Notifications now filter matching media and include new active links using cached CSS. All 22 stylesheet tests pass.

The child reports passing workspace and release checks. Its load runs used a 4 GiB address-space cap despite the requested 1 GiB cap; low RSS alone did not establish the requested limit. Parent capped reruns and final test results are recorded below. Disabled links, @import supports(), same-document server-content refresh and synchronous same-task stylesheet reads remain unimplemented. Link notification ordering for several cached URLs has no real-browser comparison.

The parent fetched only the two public CSS assets referenced by the saved login HTML. They match the existing 2026-11-04 manifest exactly: f550c1958bfdbf7f.css is 1,599 bytes, SHA-256 fb9ee4acdf22c81bc34a0898d53950b6146e48bc10392c02894855fe178e5c17; c9673e01f666feef.css is 104,630 bytes, SHA-256 94c8b8386eb8781f6d83eacbdfe2b2a48b78f99ab6203fc2d451cb045c65848b. No credentials were used. The bootstrap harness now serves cached CSS as text/css, reports CSS loaded/failed/bytes, and requires zero CSS failures for success.

With those files the bare-only loopback run fetches 17 assets and 575,754 bytes, including two successful CSS loads and 106,229 CSS bytes. It stops at Fatal(Limit("TraversalDepth")) after 21 tasks and 21 scripts, with zero script throws, load errors and unhandled rejections, and no form. The traversal guard is unchanged. This does not establish the next cause, hydration, GUI rendering or live authentication.

Final parent checks: the workspace passes 1,809 tests. Release bare passes 518 library tests with two ignored, plus four HTTPS integrations. Clippy reports warnings in unchanged code only. All nine release load processes pass at 1x/10x/100x under 1 GiB address space and 45 seconds on an 8 MiB VM stack. At 100x, linked/imports/dynamic take 464/548/993 ms with 10.6/9.1/9.3 MiB peak RSS. Rust models check values and fetch counts. Parent debug load scales were not rerun. The final cached rerun retains the same TraversalDepth stop and zero CSS failures.

## Bounded session depth profile

The finite cached React walk needs 736 evaluator frames and 121 calls. The old 512-frame shared ceiling stops it before the 100-call guard. Raising only one ceiling in a diagnostic run reached the other guard. The trace found no entry-counter leak. The original default remains a valid conservative standalone setting.

EvalLimits now has max_traversal_depth, default 512 and clamped to 768. max_call_depth still defaults to 100 and is clamped to 128. SessionConfig::new selects 768/128. Browser::new uses its caller's limits. Parser nesting stays at 256. All recursive setup/execution walks still use the shared counter, and callbacks, constructors and jobs retain the guards. Requests for larger values cannot disable them. The new public field affects callers that construct EvalLimits without a struct update.

The child observed a default-session forty-level finite walk fail before implementation. Core tests keep the standalone refusal, verify configured forty/fifty-level walks, zero/lower limits, hard clamps, flat left-deep setup, cleanup after errors, and callback/coercion/class recursion. Existing class tests that request more than 100 calls now explicitly request the larger frame cap too. A Promise depth limit stays sticky and never replays.

Earlier scratch probes on x86_64 Linux with rustc 1.96.0 measured about 624 KiB of release stack span for the real run, with a small-thread cross-check passing at 640 KiB. Debug span was about 4.75 MB. Other guard-bound recursion fixtures needed about 704 KiB in release. These are sampled measurements, not formal upper bounds for all programs. Supported execution threads remain at least 8 MiB release and 64 MiB debug. An iterative evaluator is still required to remove finite native-stack ceilings entirely.

Parent workspace passes 1,820 tests. Release JS and bare checks pass. Eight core depth tests each pass in a fresh release process under 1 GiB address space and 45 seconds, on their 8 MiB test thread. The child also reports all eleven core/session/host cases passing in fresh debug and release processes on 64/8 MiB stacks. Clippy reports warnings in unchanged code only. No fuzzing, other architecture or formal stack proof is claimed.

The cached bare-only loopback run now settles after 22 tasks, 21 scripts and 17 fetches, with 575,754 bytes, two CSS loads, zero CSS failures and zero script throws/load errors/unhandled rejections. The form is absent. Temporary console inspection reports document.getElementsByName is not a function at script 8, line 1, column 7200. The probe was removed. The saved login module does create a form, so that readiness predicate remains. All seventeen copied cache assets match the existing manifest hashes and no longer depend on another session's symlink lifetime. No GUI, Chrome comparison or live authentication was tested.

## Live name lists, input handlers and shadow trees

Document.getElementsByName returns a real live NodeList in light-tree order, restricted to HTML elements with an exactly matching native name attribute. A native revision changes on tree, attribute and namespace mutations. A list caches NodeIds for one revision. Stale heap reads request the bounded host fill hook, which charges the scan and checks result/storage caps before replacing the cache. No uncharged full scan runs inside a heap callback. Lists use the realm's array iterator methods, trace their document and reclaim metadata at collect. The other query and child-list APIs remain snapshot arrays.

The parent found uncharged long-name comparisons. A regression returned 20 matches with a budget that could not pay for the comparisons; it now ends with Steps before that work. A stale list injected during a JSON reviver callback exposed NeedsHost; the reviver now fills before its key-slot preflight and key enumeration. That regression failed first. The child's reported JSON.stringify gap did not reproduce: stricter exact empty/member serialization tests pass. The weak test accepting either a result or an error was replaced. Native revision mutation checks and the original sixteen API regressions came from the child.

oninput and onchange are functional handler properties on Window, Document, HTMLElement and SVGElement. Each handler uses one ordinary listener slot. Replacing it preserves order; null deactivates it; a later assignment appends a new slot. Property handlers stay distinct from addEventListener of the same function, read the current value at invocation, and cancel a cancelable event only for exact false. Ordinary throws report and continue; resource faults stop. Non-callable objects are stored but skipped. Primitive values become null. No string handler compilation exists. MathML handler properties, other handlers and body/window forwarding remain gaps. The child observed fifteen tests fail before implementation; its sixteen final tests pass.

Native ShadowRoot nodes have sparse host/mode identity and no light-tree parent link. Open/closed roots, host access, node mutation, shadow-including connectivity/root walks and wrapper GC work. A connected host's shadow descendants remain absent from document name lists, selectors, text and fragment serialization. Saved closed roots remain usable. Hosts do not clone roots. DocumentFragment and ShadowRoot have native prototype brands and illegal constructors; ordinary fragment creation/insertion is not added. Parent integration tests combine name-list invalidation, a real input handler and shadow boundaries.

The shadow-root preflight initially counted only ordinary wrappers. A cross-feature regression failed before Attr/map/list counts were restored. The root wrapper is now allocated before native ownership changes. Shadow style, layout, paint, slotting, custom elements, adopted sheets and composed retargeting are not implemented. Connected-shadow computed-style reads now gate instead of silently returning an empty declaration; its regression failed first. Composed dispatch that would cross a root gates before listeners; non-composed events stop at that root. The supported attach options are open/closed mode with the other supported flags false/named; additional behaviors are explicit gates. customElementRegistry is not modeled.

The combined workspace passes 1,916 tests. The serial release rerun passes bare 614 with two ignored, HTML 143 and JS 516 with one ignored, plus four HTTPS integrations. Parallel debug/release runs each hit the previously observed fetch-reuse test once; the cause remains unresolved. Nine combined name-list load rows pass under 1 GiB address space and 45 seconds. Three shadow runs at 100/1,000/10,000 hosts pass with an 8 MiB release test thread. Three handler model runs at 1x/10x/100x pass on 8 MiB release threads; 100x takes 899 ms and peaks at 58 MiB. Original child debug matrices also passed. Clippy found two warnings in the new name-list load tool; direct node iteration removes them. Other warnings remain in older code. These are single-sample checks, not performance comparisons or a browser oracle.

The combined bare-only cached run currently stops at Fatal(Limit("TraversalDepth")) after 21 tasks and scripts, 17 fetches and 575,754 bytes, including both real CSS sheets without failure. No form or authentication is verified. The limits remain 768/128, and an independent runtime trace is investigating this new path. No GUI, real-browser comparison, live login, shadow rendering or broad compatibility claim is included.


## Form activation and native border-box dimensions

HTML form and button nodes now have native interface brands. Input and button names and boolean form-control attributes reflect the native attribute list. Form owners are read from the current tree, including explicit form IDs. The driver suppresses clicks on disabled controls, including disabled fieldsets with the first-legend exception. An uncanceled click on a submit button dispatches a real SubmitEvent with its native submitter. Click and activation share one interpreter budget. Script-created click events do not activate controls. Browser::submit remains dispatch-only.

Constraint validation and native form navigation remain explicit Unsupported gates. A submit button needs noValidate or formNoValidate. A submit listener must cancel navigation. Constraint validation, form.elements, FormData, requestSubmit, form.submit(), reset actions and checkbox activation are not implemented. Event ordering and the form-owner rules have authored tests, without a browser comparison.

HTMLElement.offsetWidth and offsetHeight now use the native cascade and layout. Each read rebuilds styles and runs layout. The result is the rounded union of native border-box fragments. Detached nodes and display:none subtrees return zero. Caps check visible nodes, depth, text, fragment estimates, multipass nesting and work before layout. The work estimate is charged in full. Its factor of four per multipass nesting level is a heuristic from the layout code, not a formal bound. Default depth is 96. Measured child probes passed at this depth on 1 MiB release and 4 MiB debug threads; these samples are not stack-safety proofs.

Parent regressions exposed two preflight errors before the fixes. Hidden script text consumed the visible-text cap. A hidden shadow host blocked visible geometry. The walk now skips display:none subtrees before either check. A hidden target returns zero before the whole-page layout preflight. A visible shadow host still gates geometry because flat-tree layout and the shadow cascade are absent. Culled inline boxes also gate when the native engine has no border-box fragment. ResizeObserver and the other geometry APIs are absent.

The assembled workspace passes 1,958 tests, with nine ignored. Release bare passes 656 tests, with four ignored. All 22 geometry tests pass in both profiles. Eighteen form model runs pass at 1x, 10x and 100x, under 1 GiB address space and 45 seconds, with 8 MiB release and 64 MiB debug VM threads. Six geometry model runs pass their expected dimensions or expected refusal. At 100x the node cap refuses before layout. A first debug harness invocation selected the CLI binary by mistake; the corrected runs select the test binary from Cargo's log. Clippy reports no warnings in changed code; older warnings remain.

A separate reduced probe mounts a controlled input through three cached React chunks in bare. Browser::type_text fires onChange and updates React state and the rendered text. A JS-side value write followed by input dispatch is suppressed by React's value tracker. Both probe runs use loopback only. The temporary probe was removed. This verifies that reduced input path, not the login flow.

The full cached login still stops at TraversalDepth after 21 tasks and scripts, 17 fetches and 575,754 bytes. Both CSS sheets load without failure. No form is present. Evaluator guards remain unchanged. No live staging, credentials, GUI, browser comparison, complete hydration or authenticated journey was tested.


## Statement machine and call ceiling

Blocks, loops, switch, try and labels now run on explicit heap frames in eval_statements.rs. An if statement is a tail step. A for-in or for-of body uses the same machine. Expressions, calls, patterns and setup walks still use native recursion under max_traversal_depth. max_statement_frames counts pending control frames of all synchronous calls together. Its default is 512 and its ceiling is 768. A resource fault skips catch, finally and IteratorClose. A fault poisons the job queue: later drains do not replay the job. At most 16 empty frame buffers are kept for reuse.

The full cached login stopped at CallDepth 128 with this machine. A probe mapped the stack by exact source length and function text. Script 7 is fd9d1056-41ce205d8e0c6ad6.js (172,837 bytes). Script 8 is 2117-562f130d3e4e3a40.js (124,727 bytes). The stack held the scheduler work loop, React's commit root, then 60 alternating pairs of a5 and a6. These are the commit-phase mutation walk over the fiber tree. A probe read each fiber argument of a5. The 61 fibers were distinct, and each fiber's return field was the previous fiber. So the walk was a root-to-leaf path, not a cycle. Its tags ran from 3 (host root) through providers to 5 (host components). Each tree level costs two JS calls.

A diagnostic run with all depth caps lifted measured the page needs. The peaks were 158 calls, 608 native frames, 406 statement frames and about 783 KiB of native stack. The 8 MiB release thread and a 512 MiB thread gave the same result. Only the call cap of 128 was binding. The call ceiling is now 256. SessionConfig::new selects 256 calls, 768 native frames and 768 statement frames. The standalone default stays at 100 calls. Every call takes a native frame first. So the call count can never pass the native-frame count, and the 768-frame guard still bounds the stack. Ordinary recursion costs about three native frames per call. It ends at the frame guard before the call ceiling. A chain of bound functions costs one frame per call. Its 257th nested call is refused as CallDepth. An explicit super() chain and a super.m() recursion now end at TraversalDepth, not CallDepth. Both remain limits, not stack overflows.

Real engines bound recursion by native stack bytes, not by a call count. V8 refuses at about 1 MB of stack, which allows thousands of frames. Here, the native-frame counter remains the stack guard. The call ceiling is a second, cheaper bound.

With the new ceiling, the full cached login creates the form. It stops at Unsupported("geometry of a shadow host (flat tree layout is not built)") after 21 tasks and scripts, 17 fetches and 575,754 bytes. Both CSS sheets load, and there are no script throws or unhandled rejections. The next blocker is visible shadow-host geometry.

Validation: the debug workspace passes, with 656 bare and 534 JS tests. Release bare passes 656 tests in serial and in parallel runs. Release JS passes 534 tests. All 24 eval_load rows pass at 1x, 10x and 100x under 1 GiB and 45 seconds. The new depth tests run on 8 MiB release and 64 MiB debug threads. These samples do not prove stack safety for all programs. All 65 statement oracle cases match Node. Node ran only authored tiny programs. A parallel release run once failed reused_connections_and_posts_are_never_resent; it passed alone, serially, and in the later parallel run. Its cause is not known. Not tested: live staging, credentials, GUI, browser comparison, complete hydration and the authenticated journey.

## Flat-tree layout for shadow hosts

The flat tree is the tree that is rendered. A shadow host shows the children of its shadow root, not its own children. Blink, Stylo and Ladybird walk this tree for style and layout. Blink's FlatTreeTraversal was read on 2026-11-05. It gives three rules. The parent of a shadow root's child is the host. A host's light child has a parent only when a slot takes it. Siblings stay the same. Stylo (style/dom.rs, read the same day) turns off document author rules for an element in a shadow tree. Only that shadow tree's own sheets apply there.

bare uses the same rules without slots. `Document::flat_parent` and `Document::flat_children` implement them. The cascade walks the flat tree and inherits through the flat parent. In a shadow tree only browser (UA) rules and the inline style attribute apply. Layout uses the flat parent and flat children everywhere it walks the tree. A light child of a host gets no style and no box, as in Chrome when no slot takes it. The geometry preflight walks the flat tree. It refuses a visible host whose shadow tree has a `<slot>`, a `<style>` or a `<link>`. The scan covers hidden shadow nodes too, because a hidden `<style>` still applies. The existing `doc.len()` charge already pays for it. An element inside a shadow tree is still a geometry gate.

Cost: the first version made the cascade and layout rows of bare-bench 10 to 13 percent slower on a page without shadow roots. User-space instructions rose by 873 million, about 4,400 per element. The cause was an origin check in the candidate loop, which runs up to 5,000 times per element in this bench. The fix filters candidates once, and only inside a shadow tree. `shadow_root` now skips the hash when the document has no shadow root, and the three helpers are inlined. After the fix, extra instructions are 95 million (0.24 percent). Mean user-space cycles over five interleaved runs are 14.19G on main and 14.27G on the branch; the minimums are 12.688G and 12.704G. Wall times varied too much on this machine to show a smaller difference.

The only shadow root on the login page is Next.js's `next-route-announcer`: an inline custom element whose shadow tree holds one absolutely positioned 1px live region. With flat-tree layout, the full cached login reaches Page(Settled) after 22 tasks. React logs two errors. Minified React error 419 is a recoverable Suspense fallback to client rendering. Whether Chrome logs it on this page is not checked. The second, `ReferenceError: ResizeObserver is not defined`, is thrown in a layout effect, and React then removes the rendered tree. So the form is no longer present at the end. ResizeObserver is the next blocker.

Validation: the debug workspace passes, with 659 bare tests. Release bare, css, layout and html pass. Three new geometry tests failed first at the old gate. They cover a host sized by shadow content, scoping and inheritance, the announcer shape, and the slot and shadow-sheet gates. Not tested: slots, shadow sheets, `:host`, getComputedStyle inside a shadow tree, and a browser comparison.

## ResizeObserver and the rendering step

The CSSWG Resize Observer draft and Blink's ResizeObserverController were read on 2026-11-05. Observers are delivered in the "update the rendering" step of a frame. The step gathers the active observations at depth 0. An observation is active when its size differs from the last reported size. That size starts at -1 x -1, so the first frame always reports. The step then broadcasts and gathers again below the shallowest delivered depth. It repeats until no observation is active. Depth counts the nodes on the flat-tree path to the root. An active observation that is not deeper is skipped. A skipped observation makes the step report "ResizeObserver loop completed with undelivered notifications." and ask for another frame. A non-replaced inline element reports an empty rect.

bare has no paint loop. The rendering step is one scheduler task, `Task::Render`, due 16 ms after a change. The scheduler queues it only when an observation exists and a frame key changed since the last frame. The key holds the document revision, a new text revision, the viewport and a supplied-sheet generation. So an idle page queues no frame and a session can settle. A page with no observation pays one `RefCell` borrow per scheduler step. Each gather measures the page once: one cascade and one bounded layout for all targets. The border boxes of all targets come from one pass over the fragments. The content box is the border box minus borders and padding. A padding that is not an absolute length is an `Unsupported` gate, as is `device-pixel-content-box`. Bounds: `resize-observers` (1024) and `resize-observations` (4096). An observer with at least one observation is a GC root, as in Blink, where the targets keep it alive.

Load (release, one observer, N flat divs, one machine): first frame 0.71 ms, 5.7 ms and 22.4 ms for 100, 1,000 and 4,000 targets; a frame after one change 0.33 ms, 3.3 ms and 13.2 ms; an idle advance under 1 µs with no task. The first version computed each border box with a scan of all fragments, which is quadratic: 20.8 ms for one change at 4,000. `observe` also scanned the observer's list on every call. A per-observer target set now skips that scan for a new target. After both fixes, the full re-cascade and layout dominate the profile. Incremental style and layout would remove that cost; they are not built.

On the full cached login, Radix `useSize` observes the checkbox with `box: 'border-box'` and copies the size into the inline style of its hidden input (`width: 20px; height: 20px`). The page reaches Page(Settled) after 25 tasks with 0 throws, and the login form stays present. React still logs the recoverable error 419.

Validation: eight new resize tests failed first (no `ResizeObserver`), then passed. They cover the interfaces and argument errors, first and changed reports, an idle page, the Radix shape, same-frame depth delivery and the loop error, a throwing callback, GC liveness, replacement and the caps. The debug workspace passes (667 bare tests). Release bare passes. Not tested: a browser comparison of sizes, vertical writing modes, percent padding, and the cost of a frame on the real login page.

## RegExp `u` flag

The staging route chunks use `/u` patterns, so the `u` flag is built. ECMA-262 "Pattern Semantics" and V8's regexp compiler were read on 2026-11-05. Each rule below was then checked against Node with authored oracle cases.

- A pattern character and an input character are code points. A surrogate pair is one character. `.`, classes and complements cover U+0000 to U+10FFFF.
- The search start still advances by one code unit, as in V8. A character read never starts at the trail half of a pair. A zero-width match can still sit inside a pair.
- A `lastIndex` inside a pair first tries the start of the pair. If that attempt fails, the search starts at `lastIndex`. V8 compiles the same choice (`OptionallyStepBackToLeadSurrogate`).
- After a backreference, a position inside a pair fails, as V8's `CheckNotInSurrogatePair` does.
- A backreference inside its own open group matches empty.
- Syntax is strict: identity escapes only for syntax characters and `/` (and `-` in a class); no octal; lone `]`, `{` and `}` are errors; `\c` needs a letter; a class escape cannot be a range end; a backreference must name an existing group. `\u{...}` and escaped surrogate pairs are accepted.
- `AdvanceStringIndex` steps by code point in global `replace`, `match`, `matchAll` and `split`. The `unicode` getter and the canonical flag order `gimsuy` are added.
- Gates: `\p` and `\P`, and `\k`. Flags `v` and `d` stay gated. `i` with `u` was gated here; see [RegExp `i` with `u`](#regexp-i-with-u).

An astral character emits two `Char` units, so a literal costs nothing extra. `.`, classes and a lone surrogate in `u` mode use three new instructions, `AnyU`, `AnyAllU` and `ClassU`. They read one code point out of line, in a non-inlined `read_u`. The first version put that code in the dispatch loop and added a second call site for `run`. Both raised register pressure in the loop: `short` rose by 5.7% in instructions. The out-of-line read, a cold `attempt` for the step-back and an always-inlined `run` bring instructions below the previous build in all three modes. Cycles in `near` stay within the run-to-run noise (about +1.5% on the mean of three runs). The leaf arms of the emitter moved into `emit_leaf`. This keeps the recursive `emit_inner` frame small; the debug frame was 4,520 bytes, which overflowed the 2 MiB test stack at 128 nested groups. It is now 3,064 bytes, against 3,736 before.

Load (release, scale 20): when `u` is added to each load pattern that accepts it, `short` and `caps` cost 2–4% more and `near` the same, with the same checksums. Validation: 233k oracle cases (hand, inventory and 11 fuzz seeds, with `u`, `mu`, `su` and `uy` fuzz flags) give 0 mismatches. The debug and release workspaces pass. Not tested: Unicode case folding, property escapes, named groups, and patterns with `u` from other real pages.

## RegExp `i` with `u`

The staging route chunks hold four framer-motion colour and CSS-variable patterns with flags `iu`. ECMA-262 `Canonicalize` with `u` and `i` is simple case folding: CaseFolding.txt, statuses C and S. The Unicode 18.0.0 file was added to `data/unicode/`. `tools/gen_unicode_tables.py` now writes `CASE_FOLD` (1,533 pairs, sorted by code point) and `CASE_FOLD_INV` (the same pairs, sorted by folding) into `bare-unicode`. `bare_unicode::fold` looks up one code point. `bare-js` now depends on `bare-unicode`.

- A literal compiles to the set of code points with the same folding. A group with only ASCII members (every letter but k and s) uses the inline `CharI` step: only ASCII maps to it under the non-Unicode `canon`, so the result is the same. k joins U+212A and s joins U+017F, so those use `ClassU`.
- A class is closed over folding: every member of a folding group with a member in the set is added. Each range finds its pairs in both sorted tables by binary search. The first version scanned all 1,533 pairs for each class, which cost 140 µs per staging pattern. The tables cut that to 10 µs. A plain `u` compile takes 4 µs. The existing non-`u` `i` compile takes 414 µs, because it scans 65,536 code units for each class; that is not changed here.
- WordCharacters gains U+017F and U+212A, which fold into it. This changes `\w`, `\W`, `\b` and `\B`, as in V8.
- A backreference compares foldings by code point. One folding crosses planes (U+1DF95 to U+00DF), so each side walks on its own.
- With `u`, a class without negation and without members from U+D800 now uses the inline `Class` step. It cannot match half of a surrogate pair, so the result is the same, and plain `u` patterns also get faster.

Cost in the shared matcher loop. Every change to the dispatch loop was measured with instruction counts, which are exact, and with the minimum of 10 interleaved cycle runs. Two new `Inst` variants for the folded `\b` and backreference added 0.5–2% instructions, even in variants where their arms were removed or merged. One nested `Inst::U(UOp)` variant was worse (+1.3–2.5%). Passing `regs` and `steps` to the out-of-line step made `exec` spill more, and `short` and `caps` took 5% more cycles. The final form makes the matcher generic over `const FOLD: bool`. `exec` picks `exec_in::<true>` only for `i` with `u`. The `FOLD = false` instance has no folding code. Instructions are within 0.5% of main, and cycles are within the noise of about ±2%.

Validation: Node checks every folding pair, and each code point against its single-code-point lower and upper case: 3,135 checks, 0 differences. The 100 other differences involve code points that Node 22 (Unicode 16) does not assign. Oracle hand cases cover the four staging patterns, ſ, K, ß and ẞ, final sigma, Deseret, `\w`, `\W`, `\b` and backreferences. The fuzz flags now include `iu`, `imu`, `isu` and `iuy`. 14 seeds give 0 mismatches. One seed-24 case is near Node's 300 ms oracle timeout, so the oracle sometimes keeps it; our engine stops at its 500M step budget on it, on main too. Not tested: Turkic and full (one-to-many) case folding, which `u` does not use, and `i` with `u` on pages other than staging.

## Await inside switch

After `i` with `u`, the live login stopped at `Unsupported("await inside switch")`. The async machine now runs a switch statement that contains `await`. It follows ECMA-262 CaseBlockEvaluation and the synchronous switch in `eval_statements.rs`.

- `SwD` waits for the discriminant. `SwS` catches an unlabeled `break` and frees the switch's slot on every exit. The discriminant stays in a Rust local while the tests run inline. It goes into a slot only before a test that awaits, and leaves the slot when a body starts, because no body reads it.
- If a case body declares `let`, `const`, `class` or a function, one block environment is made for all case bodies. The existing `Scope` frame restores and releases it. Case tests run in that environment, as in the spec.
- `sw_test` compares the tests in source order with strict equality. An await-free test runs inline on the ordinary evaluator. A test with `await` pushes `SwT` and waits. After the first match, no later test runs. With no match, the run starts at `default`, wherever it is, and falls through the clauses after it.
- `SwB` runs one case body through `list_from`. A normal end falls through to the next body.
- A labeled `break` and a `continue` pass through `SwS` to the `Lab` or loop frame, as before.

The frames hold ids and counters only. The discriminant is in a slot, so the existing GC trace keeps it alive. The block environment is in a `Scope` frame, which the trace and `run_discard` already handle.

Load test (`tools/js/async_load.sh`, release, one process per row): `switch` suspends n/2 activations in a switch with a lexical case, an awaited discriminant and an awaited case test. `ifchain` is the same program written with `if`. At 100x (50,000 activations): switch run 51 ms, drain 68 ms, peak RSS 78 MiB; ifchain run 60 ms, drain 63 ms, peak RSS 74 MiB. In that row half of the calls wait on an awaited case test, so they need the slot.

Memory per paused call, measured with a counting global allocator (50,000 calls, release). A switch paused in a body, with await-free tests, first stored the discriminant in a slot. That allocated the slot stack (4 slots, each big enough for a for-of iterator) for every call: 1,471 B and 6 allocations per call. The same code with `if` took 954 B and 2.8 allocations. With the lazy slot, the switch takes 1,183 B and 5 allocations (-20%). The rest of the gap is the block environment, which the spec makes for every call when a case declares `let`, and one more growth of the frame stack. The `if` form makes its block environment only on the path that runs it. Instructions and the minimum of 10 cycle runs are within 1.3% of the first version on the switch, ifchain, seq and trycatch rows. Peak RSS did not change.

Validation: nine new Node cases cover the discriminant, the order of tests, `default` in the middle, fall-through after an await, lexical scope and TDZ, `break`, `continue`, labels, nested switches, throw, rejection and return. All 103 async cases match Node. A GC test collects while the switch is suspended and then checks that frames, slots and environments return to zero. The four gate tests that expected "await inside switch" now use other gates. Not tested: a speed comparison of the other load rows against main.

## Async activation memory

Arjun set memory as the first priority. A counting global allocator measured the bytes that one paused async call keeps (50,000 calls, release). A paused `await k` took 902 B. Two parts of it did no work:

- `await v` runs `PromiseResolve(%Promise%, v)`. For a `v` that is not an object this makes a new promise, already fulfilled, that no script can see. Its only effect is one resume job in the microtask queue. The machine now queues that job at once (`Out::Ready`). The order is the same, because `PerformPromiseThen` on a fulfilled promise queues the job at that point. An object `v` keeps the full path, because reading its `then` is observable.
- A `Vec` grows from zero to room for four items. A `Reaction` is 56 B, so the first reaction on a pending promise took 224 B, and most promises get only one. The first reaction now reserves room for exactly one. Later pushes grow as before, so many reactions on one promise stay amortized.

Bytes per paused call, before and after: plain `await k` 902 to 685, a switch paused in a body 1,183 to 967, `await` of a new pending promise 2,947 to 2,779. During a drain the if-chain peak above the paused state fell from 594 to 161 B per call.

`async_load` at 100x, 10 interleaved runs, against d152ad0 (instructions, minimum cycles, peak RSS): seq -11.5%, -17.8%, 23 to 4 MiB; fanout -14.8%, -27.4%, 69 to 47 MiB; closures -10.1%, -19.3%, 77 to 61 MiB; trycatch -4.0%, -9.4%, 69 to 59 MiB; switch -11.8%, -22.9%, 78 to 55 MiB; ifchain -12.5%, -23.1%, 74 to 51 MiB. The rows that await real promises cost a little more: nested +0.47%, +1.9%; pending +0.44%, +0.6%; equal RSS. `promise_load`: instructions within 0.2%, cycles 0.6-6.6% lower, chain 110 to 93 MiB, reject 226 to 209 MiB.

The same measurements found more memory to remove later:

- A function object with its `prototype` takes 896 B; an arrow takes 582 B. A non-arrow function makes its `prototype` object at once. V8 makes it only when a script reads it. See [Lazy function prototype](#lazy-function-prototype).
- After a collection the side tables keep their largest capacity. See [Side tables after a collect](#side-tables-after-a-collect).
- Each `let` block makes an environment, even when no closure captures it. See [Let block declarations](#let-block-declarations).

## Side tables after a collect

`Slab::trim` already gives back object payload capacity. It keeps room for twice the live values, or for the live values plus what the program allocated since the previous sweep, whichever is larger. It shrinks only when that frees 1 MiB and half of the capacity. So a burst stays after the first collect, and a second, quiet collect releases it. The side tables had no such step. A hash map, a `VecDeque` and a `Vec` keep their capacity when entries go, so after a burst they stayed at the peak.

`Interp::trim_tables` runs at the end of `collect`, after every prune, with the same rule (`trim_to`). The heap objects allocated since the previous collect stand for the entries made since then. It shrinks:

- `funcs` (object to callee), the promise map, `done_pairs` and the microtask queue;
- the async activation table (it was shrunk only when empty);
- the environment table. A free environment drops its bindings buffer, which `free_env` keeps for reuse. Free slots at the end of the table are removed. An environment index is held only by live structures, and `collect` refuses to run inside a call, so no index points past the new end.

The object slot metadata (12 B per slot ever used) stays, because object handles must stay stable.

Memory still held after a burst of 60,000 items died, after two collects (counting global allocator, release, main 8134756 vs this change):

| burst | main | after |
| --- | ---: | ---: |
| arrows | 17.1 MiB (51%) | 0.8 MiB (2.3%) |
| function expressions | 17.9 MiB (35%) | 1.5 MiB (3.0%) |
| promises | 39.7 MiB (32%) | 7.2 MiB (5.8%) |
| `then` | 40.6 MiB (49%) | 3.0 MiB (3.6%) |
| `let` scopes | 26.3 MiB (62%) | 0.8 MiB (1.8%) |
| async calls | 66.0 MiB (37%) | 6.3 MiB (3.6%) |
| plain objects | 0.8 MiB (4.3%) | 0.8 MiB (4.3%) |

Steady churn does not shrink and regrow: 40 rounds of a 10,000-item burst with a collect after each have the same peak (48.6 MiB) on both builds. Collect time is within 10% (275-291 ms vs 283-307 ms over 3 runs). Total time was 10-15% lower (hypothesis: a shorter environment table makes the per-collect scan cheaper; not verified).

## Lazy function prototype

A census of the cached login page found 1,854 ordinary functions. 1,523 of them were live, and the scripts never read the `prototype` of 1,501 (98.6%). Those unused objects were 25% of the live heap objects. V8 makes a function's prototype only when a script reads it. bare now does the same.

A new ordinary function gets its `prototype` property with the correct attributes (writable, not enumerable, not configurable) and a placeholder value, and the object gets the `lazy_proto` flag. The `Object` stays 80 B; a compile-time assert checks this. The prototype object and its `constructor` are made by `Heap::unlazy` on the first path that reads or redefines the own `prototype` value:

- `lookup` (and so `get`, `f.prototype`, `new f`, `instanceof`, a chain through `Object.create(f)`);
- `get_own_property` (`getOwnPropertyDescriptor` and the evaluator paths that read a value);
- `define_own_property` (assignment and `defineProperty`; a new value replaces the object, which is then garbage).

Key lists, `in`, `hasOwnProperty` and `delete` (always rejected, because the property is not configurable) need no object. Paths that read only the attributes or trace values (`collect`, enumerability checks in `Object.keys`, `values`, `entries`, `assign`, spread, for-in, JSON and fetch bodies) use `peek_own_property`, which never makes the object. The placeholder is `undefined`, so tracing it costs nothing.

The check runs on the object that each path already fetched. `is_lazy` tests the flag first, so other objects pay one flag test. A first version fetched the object a second time and cost +10% instructions on the `loop` row; this version costs +0.02% to +0.67%.

Results, release, main d79a592 vs this change:

- Bytes per kept function expression (counting global allocator, 50,000 functions): 896 to 582 B, the same as an arrow. A function whose `prototype` is read still takes 896 B.
- The cached login page (saved bundles over loopback): live objects 7,849 to 6,019 (-23%), allocated bytes 11.39 to 11.03 MB (-3.1%). The parsed scripts hold most of the bytes.
- Peak RSS at 100x: `promise_load` chain 93 to 64 MiB, fanout 99 to 70, thenable 283 to 226, finally 345 to 312, reject 209 to 151, ctor 339 to 261, closures 220 to 161. `eval_load` closures (10x) 52.7 to 37.2 MiB. `async_load` rows are equal; they use arrows and async functions, which have no `prototype`.

10 interleaved runs per build (instructions, minimum cycles):

| rows | instructions | min cycles |
| --- | ---: | ---: |
| `promise_load`, 7 rows at 100x | -3.8% to -13.6% | -2.2% to -11.9% |
| `eval_load` closures, webpack (10x) | -9.3%, -2.2% | -15.0%, -2.5% |
| `eval_load` other 6 rows (10x) | +0.03% to +0.41% | -0.4% to +1.7%; fib +2.5% (10x), +1.6% (100x) |
| `class_load`, 6 rows (10x) | +0.26% to +0.67% | -0.6% to +2.7% |
| `reflect_load`, 7 rows (10x) | +0.11% to +0.49% | -2.4% to +0.4% |
| `async_load`, 8 rows (100x) | +0.02% to +0.20% | -1.5% to +0.7% |

`fib` runs the same instructions (+0.03%) in more cycles. Hypothesis: code layout. Not verified.

Validation: a new test failed on main at its first object count and passes now. It checks that key lists, `in` and `hasOwnProperty` make no object, that one read makes exactly one with a stable identity, and that the prototype keeps its function alive through `constructor`. Three new Node cases cover identity, order, descriptors, `new`, `instanceof`, replacement and chains. Two GC tests counted the eager prototype objects; their counts now match the lazy heap.

## Let block declarations

Each entry into a block with a `let` or `const` cost about 812 instructions more than the same block with `var`, and one heap allocation. A profile showed that most of the cost was not the environment. `declare_pat_lexical` made a new `Vec` for the names, and `pat_names` interned each name again from the source text with a hash lookup. The walk before the run had already cached the name id in `pat_name`. A plain name now takes the cached id and allocates nothing. A destructuring pattern keeps the old path. A debug assert checks that the cached id is the interned one.

Results, release, main 09ff02c vs this change, 10 interleaved runs per build (mean instructions, minimum cycles). Each microbenchmark row runs 2,000,000 block entries in a function:

| row | instructions | min cycles | allocations |
| --- | ---: | ---: | --- |
| `{ let t = ...; }` in a loop | -10.4% | -10.9% | 2,000,742 → 741 |
| `{ const t = ...; }` in a loop | -10.4% | -10.8% | 2,000,742 → 741 |
| `for (const x of a)` | -17.3% | -18.1% | 2,000,757 → 757 |
| `if (...) { let t = ...; }` | -5.5% | -6.0% | 1,000,742 → 741 |
| `async_load` switch, ifchain (100x) | -2.3%, -0.6% | -1.8%, -0.3% | |

All rows of `eval_load`, `class_load` and `reflect_load` (10x), and the other rows of `async_load` and `promise_load` (100x), run the same instructions (within 0.01%). Their cycles are within -3.8% to +3.0%. Peak RSS does not change.

A `let` block now costs about 408 instructions more than `var`. That cost is the environment, its frame, the binding and one more hop on each name lookup.

### Elision tried and dropped

V8 keeps a block's `let` variables out of a context when no closure captures them. A test version put the bindings of a block in the current environment and removed them on exit. It applied only when the walk found no function or class in the block, the current environment was not captured and not the global one, and the names were plain and new there. The conditions protect against two cases: a closure made earlier would see the names, and a called function that closes over the global environment would see them too.

It was correct on 9 new Node cases and on all the checks. It did not pay:

- In the `let` loop it cost 1.6% more instructions than the allocation fix alone. The clash check scans the current environment, and lookups of the block names scan the function's bindings.
- The cached login boot ran 0.06% fewer instructions, with the same memory.
- The memory gain is one environment for each async call that is paused inside such a block. A block with an arrow, such as `await new Promise(r => ...)`, does not qualify, so the async benchmark did not change.

A useful version needs analysis for each variable: is the name referenced by any closure in the function? That needs scope resolution in the walk. Measure paused generators first, because a paused generator keeps its block environments.

Validation: 9 new Node cases cover shadowing and earlier closures, a called function that reads a global with the same name, exit by throw, `break` and `continue`, TDZ and `const`, parameter clashes and nesting, more than 12 bindings, catch bodies, and `await` inside blocks with interleaved calls. They match Node on main and on this change.

## Error stack locations

A profile of the cached login boot (saved bundles over loopback, 201 M instructions) found that `lex::location` took 17%. Each new error builds its `stack` text. For each frame with a position, `frame_line` found the line and column by scanning the script from byte 0. The bundles are minified to one long line, so every frame scanned up to the size of its script. Name lookup, which looked like the next target, did not appear in that profile.

`Program::location` now makes location marks on its first call in a script longer than 4 KiB. A mark stores the byte offset, line, UTF-16 column and the carriage-return state at the first character boundary of each 4 KiB block (16 B per 4 KiB of source). A lookup finds the last mark before the offset with a binary search and scans at most 4 KiB from it. A shorter script needs no marks. The scan reads bytes, not characters. A byte above `\r` that is not `0xE2` is counted with one test and no branches: a continuation byte adds 0 units, a lead byte of a 4-byte character adds 2, other bytes add 1. Line breaks and LS/PS take the slow path. The step budget charges the bytes that the scan can read: the whole script for the first lookup, then 4 KiB.

Results, release, main 5878337 vs this change, 10 interleaved runs per build (mean instructions, minimum cycles):

| row | instructions | min cycles |
| --- | ---: | ---: |
| cached login boot | -14.0% | -7.7% |
| `stack_load` 20,000 | +0.16% | +0.56% |
| `eval_load` throws (10x) | +0.07% | -1.1% |
| `eval_load` loop, webpack (10x) | 0.00%, +0.01% | -1.6%, -0.4% |

The boot allocates 1.5 KB more (the marks for 575 KB of scripts). A 1 KiB step saved only 0.66% more boot instructions and cost 3.4 KB more, so the step stays 4 KiB. After this change the scan is 3.3% of the boot, mostly the single pass that makes the marks. Formatting the text only on the first read of `stack`, as V8 does, would remove that and the formatting. It needs a side table, a property hook and GC pruning. It waits until a profile shows that it is worth that cost.

A byte scan with one `match` per byte had the same instruction count as the character scan, but took more cycles on short ASCII scripts (`throws` +5.7%). The fast path removed that.

Validation: a test compares the marked lookup and the plain lookup with the old character scan at every offset of a 12 KB text with CR, CRLF, LS, PS and 2-, 3- and 4-byte characters. A new stack test failed on main with `Limit(Steps)` and passes now: 300 errors with two positioned frames each, at the end of a 1.5 MB one-line script, under a 3,000,000 step budget.


## Generators

The live login stopped at `Unsupported("generators are not supported")`. The parser rejected `function*` in a d3 chunk. bare now parses and runs generators: `function*` declarations and expressions, `*m(){}` in objects and classes, `yield`, `yield*`, and `next`, `return` and `throw`. Async generators stay a gate ("async generators are not supported").

Parser:

- `Ctx.is_gen` makes `yield` an operator. It is parsed at the start of an assignment expression, as Acorn does. `yield` takes no argument before a newline or a token that cannot start an expression (`starts_expr`). Arrows and class field initializers reset the flag.
- A `yield` in parameters is an early error. It shares `await_pos` with `await`, so arrow parameters and async arrow parameters check both.
- A generator declaration is lexical in a block, and it is not allowed as the body of `if` or a label. Annex B.3.3 applies only to plain functions. The evaluator gave the var binding to block-level generators and async functions as well. It no longer does.
- `Func.mode` (`Plain`, `Async`, `Generator`) holds both flags in one byte. Two bools made `Func` 48 B, not 44 B, and the cached login boot kept 10.6 KB more. A size assert now guards it.

Runtime (`eval_generator.rs`):

- A generator runs on the async machine. The preflight marks `yield` as it marks `await`, so only the nodes that contain a `yield` become frames. Every other subtree runs on the ordinary evaluator. A `yield` ends a step with `Out::Yield`. `next(v)`, `return(v)` and `throw(v)` drive the run again with `Next::Val(v)`, `Abr::Return(v)` or `Abr::Throw(v)`. `finally` blocks and the close of a for-of run as in an async function.
- A call binds the parameters at once, so a parameter error throws at the call. Then it makes the generator object with `F.prototype`, or `%GeneratorPrototype%` when that is not an object. Generator functions are not constructors. Their own `prototype` has no `constructor`.
- `aw.gens` maps a generator object to `Gen::{Start, Yield, Running, Done}`. A suspended run is boxed, so a finished generator costs one 16 B entry. Frames and slots count in the `AsyncLimits` caps. A call from inside a running generator to itself throws a TypeError. A resume counts as a call for the call depth cap.
- `yield*` keeps the inner iteration in a `Slot::It`. A generic inner iterator gets `next`, `throw` and `return` with the received value. Its result objects pass through unchanged, as the spec requires for sync generators. Arrays, strings, Maps and Sets step without result objects. They have no `throw`, so a `throw` closes them and throws a TypeError.
- `%GeneratorFunction.prototype%` and `%GeneratorPrototype%` are made with the first generator function, so a page without generators pays nothing. `%GeneratorFunction%` (the constructor that compiles source text) does not exist.
- GC: `collect` traces a suspended run when it reaches its generator object, and drops the entries of dead objects.

For-of, spread, destructuring and `Array.from` over a generator whose `next` is the intrinsic step it directly through `iter_native_step`. They make no result object for each value. V8 avoids this allocation only after its optimizing compiler removes it.

Load test (`tools/js/gen_load.sh`, release, one process per row, 100x):

| row | work | run | peak RSS |
| --- | --- | ---: | ---: |
| `forof` | 1M values, for-of over a generator | 556 ms | 4.2 MiB |
| `iterobj` | the same loop over a hand-written iterator object | 777 ms | 291 MiB |
| `next` | 1M explicit `next()` calls | 1,123 ms | 291 MiB |
| `spawn` | 1M generators run to the end | 1,720 ms | 164 MiB |
| `delegate` | 200,000 values through 6 levels of `yield*` | 298 ms | 62 MiB |
| `finally` | 1M generators closed by `break` | 1,954 ms | 430 MiB |

The heap is not collected during a run, so RSS includes the dead result objects. A profile of `next` shows the cost in the ordinary member call and the result object, not in generator code.

Memory per suspended generator, counting global allocator, 20,000 generators after a collect: `yield k` 769 B (1 object), a closure 1,060 B, the d3 shape (a for-of over an array) 1,357 B, a for-of with try/finally 2,115 B. Most of this is the machine layout that async calls share: the call environment (56 B plus bindings), the boxed `Run` (88 B), the frame stack (4 frames of 28 B on the first push) and a `Slot::It` of 72 B. A smaller layout would help async calls and generators alike. It waits until a real page shows the cost. The only generator in the target bundles runs to the end at once.

Results, release, main 551ab69 vs this change, 10 interleaved runs per build: instructions are within -0.16% to +0.34% on every row of the cached login boot, `parse_load`, `eval_load`, `class_load`, `reflect_load`, `async_load` and `promise_load`. Peak RSS and the boot's bytes do not change. Two cycle outliers at the same instruction count (`parse_load` bodies +21%, `eval_load` webpack +14%) gave -3.3% and +1.1% in 20 more runs each.

Known deviations: an unsupported `yield` position reports the `await` message of the same gate (for example "await inside for-in"). `TypedArray.from` is still a gate, and the d3 chunk passes its generator to `Float64Array.from`.

Validation: 50 new Node cases (`tools/fixtures/js/generator-cases.txt`, `tools/js/generator_check.sh`) cover the protocol, finally on return, yield in expressions, loops, switch and try, delegation to generators, arrays and custom iterators, prototypes and descriptors, receivers, and the d3 shape. The parse corpus has 46 new generator cases and matches Acorn (2,133 cases). Rust tests cover GC of suspended runs and environments, the call depth cap under deep `yield*`, the frame cap and the allocation-free for-of.

## Tagged templates

The live login stopped at `Unsupported("tagged template")` after it stored its token. The parser already read `` tag`...` `` and checked its escapes. bare now runs it.

- The call works as an ordinary call. The tag is linked as a callee, so `o.t`x`` and `super.t`x`` get their `this`. The arguments are the strings object, then the substitutions in order. A tag that is not callable throws a TypeError after the substitutions run, as in the spec.
- GetTemplateObject: each site gets one strings array on its first call. The array is frozen, and its `raw` property holds a frozen array of the raw pieces. A cooked piece with a bad escape is `undefined`. The same site always gives the same object, and two sites with the same text give two objects. Libraries use this identity as a cache key.
- `Interp.tpl_objs` maps (script, expression) to the object. Scripts number their expressions from 0, so the script is part of the key. A function from another script uses the site of its own script. The table lives as long as the realm, as the spec's `[[TemplateMap]]` does. Both `collect` and `roots` root it. A test failed before `collect` rooted it: the object was freed and made again, so its identity changed.
- The cooked and raw texts are interned, so a piece without escapes shares one string between both arrays.
- The async preflight refuses `await` (and, in a generator, `yield`) in the tag or in a substitution: "await in a tagged template". Before this change the scan did not look inside a tagged template.

Load test (`tools/js/tagged_load.sh`, release, one process per row, 100x):

| row | work | run | peak RSS |
| --- | --- | ---: | ---: |
| `call` | 1M calls of one site | 635 ms | 4.2 MiB |
| `plain` | the same loop as an ordinary call with an array made once | 659 ms | 4.2 MiB |
| `sites` | 100,000 sites, each called once | 140 ms | 132 MiB |
| `css` | 1M calls of a styled-components shape | 2,653 ms | 368 MiB |

In `css`, the 1M rest arrays (`...v`) and the joined strings stay in the heap until the collect after the run. They are the cost of the tag function, not of the site.

A repeated call costs no more than an ordinary call: it takes one hash lookup and makes no object. A site costs 564 B of heap: two array objects, the frozen elements and `length` as property entries, `raw`, and the interned texts. Frozen elements cannot stay in the dense element vector, because it holds only writable data. A frozen flag on the object would save about 130 B for each site. The saved staging chunks have fewer than 75 possible sites (a text count that includes backticks in strings), so this would save less than 10 KB. It is not built.

Results, release, main aa52994 vs this change, 10 interleaved runs per build: instructions are within -0.12% to +0.01% on every row of the cached login boot, `parse_load`, `eval_load`, `class_load`, `reflect_load`, `async_load` and `promise_load`. Peak RSS and the boot's bytes do not change.

Known gaps: `String.raw`, `Object.freeze`, `Object.isFrozen` and `Object.isExtensible` do not exist yet. `await` inside a tagged template is a gate. Batch 1 later added `String.raw`, `Object.freeze`, `Object.isFrozen` and `Object.isExtensible`.

Validation: 40 new Node cases (`tools/fixtures/js/tagged-cases.txt`, `tools/js/tagged_check.sh`) cover cooked and raw values, CR and CRLF, bad escapes, per-site identity, freezing and descriptors, key order, `this` for member, index and `super` tags, evaluation order, chained tags, `new`, arrows, classes, generators and async functions. Rust tests cover GC of a site object that only the table holds, and sites in more than one script.

## Intl.DateTimeFormat

The live login stopped at `ReferenceError: Intl is not defined` after it went to `/`. The post-login chunks call `Intl.DateTimeFormat("en-US", { timeZone: "Asia/Kolkata", ... }).formatToParts(...)` at module level. They also call `toLocaleDateString` and `toLocaleTimeString` with `en-US`, `en-CA` and a `timeZone`. bare now has `Intl.DateTimeFormat` and the three `Date.prototype.toLocale*String` methods.

Design:

- Locales: `en-US`, `en`, `en-CA`, `en-IN` and `en-GB`. Lookup drops subtags from the right, so `en-AU` is `en`. A locale with no match falls back to `en-US`. ECMA-402 allows this when a locale is not available, but Node formats `fr` in French.
- Patterns come from tables, not from a CLDR pattern generator. `tools/js/intl_gen.mjs` makes `crates/js/src/intl_data.rs` from the ICU 77 data of Node 22.18. The tables are:
  - a date pattern for each set of date fields;
  - a time pattern for each set of time fields and hour cycle;
  - a joiner (", " or " at ");
  - 576 whole patterns for the pairs that ICU does not join;
  - the style patterns;
  - the default hour cycles;
  - the names.

  The data is 619 patterns in a 12,628 B pool. Nothing is generated at run time.
- Time zones come from the system tzdata. `tz.rs` reads TZif v1 to v4 from `TZDIR` or `/usr/share/zoneinfo` on first use, with its POSIX footer rule. A realm keeps up to 64 zones by name. A file read costs 10,000 steps. The UTC names and fixed offsets (`+05:30`) need no file. The default zone is `set_default_time_zone`, else `TZ`, else the `/etc/localtime` link target, else UTC.
- Options are read in the order of CreateDateTimeFormat, with V8's RangeError and TypeError messages. hour12 replaces hourCycle. A style together with a field throws.
- V8 quirks, found by the Node comparison:
  - `format()` prints U+202F (narrow no-break space) as U+0020. `formatToParts()` keeps U+202F.
  - When `toLocale*String` fills in the default time fields, h11 prints as h12 and h24 as h23.
  - Locale errors have two messages. A malformed subtag gives "Incorrect locale information provided". Other errors give "Invalid language tag: <tag in lower case>".
- `format` is a getter that gives one bound function per formatter. The formatter keeps the function alive, and the function keeps the formatter alive.
- Gates (unsupported): `era`, `dayPeriod`, `fractionalSecondDigits`, `timeZoneName`, a locale extension (`-u-`, `-t-`, `-x-`), a style that prints a zone name (`timeStyle` full or long), and a year before 1.
- Known deviations:
  - `resolvedOptions().timeZone` gives the name as it was passed in (`Asia/Kolkata`). V8 gives `Asia/Calcutta`.
  - `calendar` and `numberingSystem` always resolve to `gregory` and `latn`.
  - A wrong receiver that is an object shows as `object`, not `#<Object>`. This is the same for every builtin in bare.

Storage: one `Dtf` per formatter in `intl.dtfs`, capped by `max_formatters` (65,536) and pruned by `collect`. The 20 option keys are made once and rooted. UTC is one shared zone.

Optimization: the first profile of `toLocaleTimeString` with options showed `Heap::intern` at the top. Each option read interned its name again. With the keys cached, that row runs 45% fewer instructions, and `make` runs 41% fewer. The first memory run showed 285 B per formatter outside the heap, because each UTC formatter made its own zone. With one shared UTC zone, the cost is 105 B. What remains is the spec's 20 option reads (`Heap::lookup`) and the interpreter. In `format`, the Intl code is about 10% of the profile.

Load test (`tools/js/intl_load.sh`, release, one process per row, 100x):

| row | work | run | peak RSS |
| --- | --- | ---: | ---: |
| `make` | 50,000 formatters kept alive | 92 ms | 16.1 MiB |
| `format` | 1M `format` calls on one formatter | 791 ms | 4.3 MiB |
| `parts` | 100,000 `formatToParts` calls | 207 ms | 126.8 MiB |
| `locale` | 1M `toLocaleTimeString` calls with options, Asia/Kolkata | 2,157 ms | 139.7 MiB |
| `plain` | 1M string concatenations of the same length (baseline) | 1,138 ms | 4.4 MiB |

A formatter costs one heap object and 105 B outside the heap. This is the difference between `make` and the same loop with `{}` (`INTL_EMPTY=1`). A `format` call costs less than the baseline concatenation. `parts` and `locale` keep their garbage (4 objects per call, and one `Date` per call) until the collect after the run.

Results, release, main fda7325 vs this change, 10 interleaved runs per build:

- Instructions are within -0.18% to +0.20% on every row of the cached login boot, `parse_load`, `eval_load`, `class_load`, `reflect_load`, `async_load` and `promise_load`. The exception is `eval_load concat` at +0.96%. Hypothesis: this is the larger realm setup.
- Minimum cycles on `parse_load` rose 3% to 7%, but its instructions did not change, and `parse_load` does not run the evaluator. To verify: code layout.
- Peak RSS rises by about 0.1 MiB, which is the size of the binary.

Validation:

- 90 Node cases (`tools/fixtures/js/intl-cases.txt`, `tools/js/intl_check.sh`, TZ=UTC) cover:
  - the bundle's exact calls;
  - defaults for each method, and styles with hour cycles;
  - DST in New York and Sydney, Kolkata in 1943, and LMT;
  - zone names, offsets and bad names;
  - locale resolution and errors, and `supportedLocalesOf`;
  - option errors, option order and wrong receivers;
  - the `format` getter, the shape of the builtins, `resolvedOptions`, part types, every hour cycle around the clock, years, names and day periods.
- 60 matrix cases format every option combination for each locale, weekday width and year width at 4 instants. Each returns a count and a hash.
- Rust tests cover:
  - TZif parsing of every version, and POSIX rules;
  - tag canonicalization;
  - the patterns and the U+202F rule;
  - GC of formatters and their bound `format`;
  - the formatter cap, the gates, locale fallback and system zones.

## Number, Math and the global functions

The live login stopped at `ReferenceError: parseFloat is not defined`. A scan of the 74 cached chunks found more missing functions in the same group. This topic adds them:

- Globals: `parseFloat`, `isFinite`, `encodeURI`, `decodeURI`, `decodeURIComponent`, `escape` and `unescape`.
- `Number`: `isFinite`, `isInteger`, `isSafeInteger`, the 8 constants, and `parseFloat` and `parseInt`. These two are the same objects as the globals.
- `Number.prototype`: `toFixed`, `toExponential` and `toPrecision`.
- `Math`: the 7 missing constants, 23 one-argument functions, and `atan2`, `pow`, `imul` and `hypot`. `Math[Symbol.toStringTag]` is "Math".
- `String.fromCodePoint`.

The Object and Reflect gaps need an extensible flag in the heap. They are the next topic.

Design:

- Math functions must give the same bits as V8. Rust's `f64` methods call the system libm. That libm differs from V8 in the last bit on 1% to 30% of inputs, depending on the function. `crates/js/src/ieee754.rs` is a port of V8 12.4.254 `src/base/ieee754.cc` (fdlibm), with V8's BSD license in `crates/js/LICENSE-V8`. The port keeps the C shape so it can be read against the source. The `**` operator now uses the same `pow`.
- `Math.hypot` follows V8 `math.tq`. Infinity wins over NaN. The arguments are scaled by the largest, summed with Kahan summation, and the square root is multiplied back.
- `toFixed`, `toExponential(n)` and `toPrecision` round half up on the exact decimal value of the double. `number::exact_digits` makes that value with a base-10^9 bignum. A double has at most 767 significant digits. So `(1.005).toFixed(2)` is "1.00", because 1.005 is 1.00499999999999989... .
- `toExponential()` with no argument uses the digits of `Number::toString`. Rust's `{:e}` breaks a tie between two shortest texts another way, for example on `-8958.816528320312`.
- Range checks follow V8 order: `toFixed` checks the range before NaN; the other two check finiteness first. The error messages are V8's.
- `decodeURI` decodes UTF-8 with `str::from_utf8`, which refuses overlong forms, surrogates and values above U+10FFFF. It keeps the escape of a reserved character (`;/?:@&=+$,#`).
- `encodeURI` and `escape` count the exact output size and check it against the string cap before they make the text. Every global that scans its argument charges the scan in steps first.

Optimization: the first profile of `toFixed` showed `exact_digits` and `u32` formatting at about 7%. The function formatted each limb with `format!`. It now writes the digits directly. The `toFixed` and `toPrecision` load rows run about 15% fewer instructions. The rest of each profile is the interpreter (`eval_expr_inner`, `PropMap::find`, `Heap::lookup`, `Heap::intern`).

Load test (`tools/js/numeric_load.sh`, release, one process per row, 100,000 calls):

| row | call | ns per call | peak RSS |
| --- | --- | ---: | ---: |
| `empty` | `0` (loop baseline) | 263 | 3.9 MiB |
| `pow` | `(i % 100) ** 1.5` | 451 | 3.9 MiB |
| `sin` | `Math.sin(i) + 1` | 475 | 3.9 MiB |
| `atan2` | `Math.atan2(i, 7) + 2` | 480 | 4.0 MiB |
| `tofixed` | `(i / 7).toFixed(2).length` | 722 | 11.2 MiB |
| `toprecision` | `(i * 1.1).toPrecision(6).length` | 762 | 11.1 MiB |
| `toexp` | `(i / 3).toExponential().length` | 1,332 | 14.2 MiB |
| `parsefloat` | `parseFloat(i / 7 + 'x')` | 1,277 | 22.5 MiB |
| `encodeuri` | `encodeURI('é /?#' + i).length` | 1,482 | 30.4 MiB |
| `escape` | `escape('é /?#' + i).length` | 1,238 | 30.1 MiB |

The ns column is the time per call minus the empty loop. RSS grows with the strings each row makes, because nothing is collected during a run. A Math call costs about the same as `Math.log` before this change.

Results, release, main bd26c75 vs this change, 10 interleaved runs per build:

- Instructions are within -0.19% to +0.31% on most rows of the cached login boot, `parse_load`, `eval_load`, `class_load`, `reflect_load`, `async_load`, `promise_load` and `numeric_load`.
- `eval_load concat` is +2.51% (it was +0.96% after Intl). Hypothesis: the realm setup installs more functions. To verify: profile the realm setup.
- `reflect_load construct_args` is +0.72%. `numeric_load pow` is +0.95%, because fdlibm `pow` does more work than the system `pow` to give V8's bits.
- Minimum cycles vary from -2% to +11% with no matching change in instructions (`reflect_load views` +10.65%). Hypothesis: code layout and noise.
- Peak RSS is the same or about 0.4 MiB lower.

Validation:

- 22 Node cases (`tools/fixtures/js/number-cases.txt`, `tools/js/number_check.sh`). They cover parsing, the constants and shapes, special values, coercion order, the three formats with their errors, URI round trips and errors, `escape`, and `fromCodePoint`. Four matrix cases hash 63,000 Math results, 21,000 `toFixed` results and the other formats.
- `tools/js/math_check.sh` compares the 22 fdlibm functions with Node bit for bit on 75,811 inputs (`tools/js/math_gen.mjs`): random values, special values, values near multiples of π/2, overflow thresholds and random bit patterns. All match.
- Rust tests (`eval_number_tests.rs`, `number.rs`): the corpus without the matrices, the string cap for `encodeURI` and `escape`, step charges for each scan, and GC after the globals are deleted. Unit tests cover `exact_digits`, rounding and `float_prefix`.

## Boa compared with bare-js

On 2026-11-05 the live login ran on main c13c257. It stores its token, goes to `/` and settles with 42 scripts and no throws. It then reports `TypeError: [...u.e].reverse is not a function`. `Array.prototype.reverse` is missing.

A built-in inventory followed. `tools/js/builtin_inventory.sh` lists the standard built-ins of Node 22.18 and checks each one in bare-js. 311 of 655 are missing. Some of the 311 are getters that throw on the prototype, for example `Map.prototype.size`. The staging bundles call many of the missing names: Array `reverse`, `at`, `flat`; Object `setPrototypeOf`, `getOwnPropertySymbols`, `defineProperties`, `freeze`; `Proxy`, `BigInt`, `DataView`, most typed arrays and the Iterator helpers. `BigInt` and `Proxy` change the core value and property code.

The question was: switch to Boa, or keep bare-js and add the built-ins. Boa (MIT) passes about 94% of test262 at v0.21. We measured Boa 0.22.0 against bare-js before the decision.

Setup: AMD Ryzen 9 5900HX, release builds. Boa runs `Context::eval` in a small runner outside the repository. bare-js runs `eval_check -e`. Each row is the best of 3 runs. Peak RSS is `ru_maxrss` from `wait4`. Both engines give the Node result on every row.

| Workload | bare-js s | bare-js MiB | Boa s | Boa MiB | Boa time / bare-js time |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty script | 0.00 | 12.3 | 0.00 | 12.3 | - |
| arithmetic loop, 3M | 1.55 | 12.3 | 2.07 | 12.3 | 1.34x |
| 300k small objects | 0.53 | 94.0 | 0.74 | 62.0 | 1.41x |
| 1M function and closure calls | 0.86 | 12.3 | 1.04 | 12.3 | 1.21x |
| 200k strings, join, indexOf | 0.39 | 41.1 | 0.37 | 45.5 | 0.96x |
| 200k array map, filter, sort | 0.37 | 12.3 | 0.42 | 14.3 | 1.12x |
| JSON stringify and parse, 50k objects | 0.20 | 88.2 | 0.86 | 261.1 | 4.40x |
| 300k class instances | 0.53 | 89.5 | 0.76 | 13.1 | 1.43x |
| Map and Set, 200k keys | 0.53 | 49.4 | 0.51 | 36.8 | 0.96x |

The 40 cached staging chunks (1.7 MiB):

| Engine | Work | Time | Peak RSS |
| --- | --- | ---: | ---: |
| bare-js | parse | 0.030 s | 12.5 MiB |
| Boa | parse and compile to bytecode | 0.634 s | 52.6 MiB |

Stripped binary: bare-js `eval_check` is 1.8 MB. The Boa runner is 13.0 MB, with 176 crates.

Results:

- bare-js runs faster on 6 of 8 workloads, by 1.1x to 4.4x. Boa is 4% faster on the other two (strings, Map and Set).
- bare-js parses the real chunks 21 times faster. Boa also compiles to bytecode, so the rows are not the same work. A page pays both costs before it runs.
- Boa uses less memory when garbage builds up during one script: 13 MiB against 90 MiB for the class row. `eval_check` does not collect during a run. Hypothesis: a collection inside long scripts closes most of this gap. To verify in the session, which collects between tasks.

Decision: keep bare-js. Add the missing built-ins in batches. Use Boa's code as a reference and copy it where that is faster than writing it. Use ICU4X for locale data. Re-run `tools/js/builtin_inventory.sh` after each batch.

Limits: one machine, 3 runs per row, microbenchmarks only. The real page was not run on Boa, because Boa has no DOM.

## Batch 1 built-ins: Array, String, Object, Reflect

The live login stopped at `TypeError: [...u.e].reverse is not a function`. Batch 1 adds the missing Array, String, Object and Reflect built-ins that the bundles and the inventory name. `tools/js/builtin_inventory.sh` now prints 241 of 655 missing, down from 311. Older sections of this file that say a name is absent describe the state before this batch.

Added:

- Array.prototype: `reverse`, `at`, `flat`, `flatMap`, `findLast`, `findLastIndex`, `lastIndexOf`, `reduceRight`, `copyWithin`, `toReversed`, `toSorted`, `toSpliced`, `with`, `toLocaleString`, `[Symbol.unscopables]`.
- String: `String.raw`. String.prototype: `at`, `codePointAt`, `lastIndexOf`, `padStart`, `padEnd`, `repeat`, `replaceAll`, `search`, `normalize`, `trimStart`, `trimEnd`, `trimLeft`, `trimRight`, `isWellFormed`, `toWellFormed`, `substr`, `toLocaleLowerCase`, `toLocaleUpperCase`. `RegExp.prototype[Symbol.search]`.
- Object: `getOwnPropertyDescriptors`, `getOwnPropertySymbols`, `hasOwn`, `is`, `preventExtensions`, `seal`, `freeze`, `isExtensible`, `isFrozen`, `isSealed`, `defineProperties`, `setPrototypeOf`, `groupBy`. Object.prototype: `isPrototypeOf`, `propertyIsEnumerable`, `toLocaleString`, the `__proto__` accessor, `__defineGetter__`, `__defineSetter__`, `__lookupGetter__`, `__lookupSetter__`.
- Reflect: `apply`, `defineProperty`, `deleteProperty`, `get`, `getOwnPropertyDescriptor`, `getPrototypeOf`, `has`, `isExtensible`, `preventExtensions`, `set`, `setPrototypeOf`. `Map.groupBy`.

Design:

- Three new files: `eval_array_more.rs`, `eval_string_more.rs` and `eval_object_more.rs`. Each is `include!`d like the older builtin files and has its own install and dispatch function. The older files forward to them (`call_ao`, `call_ext`, `call_refl`). `call_refl` now receives `this`.
- Every method follows the specification on a generic receiver: one `ToLength`, holes by `HasProperty` or read as `undefined`, strict writes, getters in the order of the specification. V8's error text is copied where Node 22 differs from the specification text.
- Dense-array fast paths. `Heap::dense_elems` returns the element slice of a plain array when every index below `length` is dense or a hole that reads `undefined` on the whole prototype chain. `Heap::edit_dense` changes that slice in place when the array is extensible. No script runs inside a fast path, so the answer is the same as the generic algorithm. `reverse`, `copyWithin` and `fill`-like moves work in place, `lastIndexOf` and `flat` read the slice, and `toReversed`, `toSpliced` and `with` copy it. A result array is made in one step (`Heap::fill_array`), after its length and bytes are checked. The generic path runs when a condition fails.
- Work is charged after the fact: 1 step per 8 elements read and 1 per 32 moved. `ao_reserve` runs first, so a range longer than the remaining budget fails with `Steps` before it does any work. `lastIndexOf` on a string uses a bounded scan (`rfind_units`) that charges each 256-unit chunk.
- `toSorted` shares `ao_merge_sort` with `sort`. It makes the sorted copy first and no comparator runs after a throw.
- `Object.freeze` and `Object.seal` run in one pass (`Heap::set_integrity`). The object stops being extensible and every property stops being configurable. With `freeze` every data property also stops being writable. Dense elements move to the property table, which keeps their attributes. A frozen element costs 72 B instead of 16 B (48 B entry and 24 B index). Freezing 1M elements adds about 56 MB. A flag that keeps dense storage for frozen arrays would avoid this. It would touch every element write, so it waits until a page needs it. `isFrozen` and `isSealed` read the flags without a key list (`Heap::test_integrity`).
- A byte view with elements cannot be frozen (`TypeError`, as in V8 and the specification). `Object.seal` of such a view succeeds, as in V8. The specification says it throws, because the view's elements are configurable.
- `normalize` uses `bare-unicode` (`nfd` is new, and the accent sort is now a stable sort of each run, not a swap loop that took O(n²) on a long run of accents). A string below U+0300 (NFC) or U+00C0 (NFD) is returned without a copy.
- `groupBy` uses the shared iterator machinery. The iterator is closed when the callback or the key conversion throws. Groups are kept in first-seen order.

Results, release, AMD Ryzen 9 5900HX, one script in `eval_check`, 1M-element arrays. The generic row is the same data as an array-like object:

| Operation | dense path | generic path |
| --- | ---: | ---: |
| `reverse` | under 1 ms | 70 ms |
| `copyWithin(1, 0)` | 1 ms | 71 ms |
| `toReversed` | 9 ms | 30 ms |
| `with(5, 1)` | 8 ms | 31 ms |
| `lastIndexOf` (no match) | 1 ms | 35 ms |
| `Object.freeze` | 81 ms | - |
| `Object.isFrozen` | 2 ms | - |

Other single runs: `toSorted()` of 200k numbers 81 ms. `flat(Infinity)` of 100k `[i, [i]]` 111 ms. `padStart(10M)` 19 ms. `repeat` to 10M units 7 ms. `replaceAll` of one unit in 1.2M units 14 ms. `normalize("NFD")` of 1M non-ASCII units 56 ms. `getOwnPropertyDescriptors` of 100k keys 42 ms. `Object.groupBy` of 1M numbers 363 ms. Each time is one run. Times of 1 ms or less are at the limit of the clock.

Load rows (`tools/js/arrobj_load.sh`): 9 new cases (`reverse`, `flat`, `tosorted`, `lastindexof`, `pad`, `repeat`, `replaceall`, `freeze`, `descriptors`) at 1x, 10x and 100x. All 45 rows of the matrix have `checksum=ok`. A row includes the loop that builds its input, so the 100x times below are mostly that loop:

| case (100x) | size | run | steps |
| --- | ---: | ---: | ---: |
| `reverse` and `toReversed` | 2M elements | 2,958 ms | 74.3 M |
| `flat` and `flatMap` | 200k pairs | 556 ms | 8.6 M |
| `tosorted` (comparator and default) | 200k numbers | 807 ms | 26.6 M |
| `lastindexof` | 5M elements, 40 scans | 3,171 ms | 90.0 M |
| `pad` | 5M units | 20 ms | 0.3 M |
| `repeat` | 20M units | 18 ms | 0.6 M |
| `replaceall` | 3M units | 257 ms | 12.9 M |
| `freeze` | 2M elements, 200k keys | 2,098 ms | 27.3 M |
| `descriptors` | 20k keys, 20k elements | 55 ms | 0.7 M |

Cost on the existing rows, main a3e4c7a against this change, 10 interleaved runs per build (20 for the rows marked *), mean instructions and minimum cycles, `perf stat` user mode:

| row | instructions | min cycles |
| --- | ---: | ---: |
| `eval_load` loop, calls, closures, objects (10x) | 0.00% to +0.03% | -0.7% to +1.1% |
| `eval_load` fib (10x) * | +0.18% | +3.2% |
| `eval_load` throws (10x) * | +0.22% | +3.8% |
| `eval_load` calls (10x) * | +0.01% | +1.8% |
| `eval_load` webpack (10x) * | +1.08% | +1.7% |
| `eval_load` concat (10x) * | +5.32% | +3.5% |
| `arrobj_load` old rows (10x) | -1.09% to +0.66% | -2.7% to +1.2% |
| `numeric_load` 6 kinds (100,000) | +0.02% to +0.08% | -0.9% to +0.9% |
| `scripts_load` many, few (10x) | +0.65%, +0.35% | +1.6%, -0.5% |
| cached login boot (434 M instructions) | +0.31% | -0.9% |

Regressions over 1%:

- `webpack` (55.5 M instructions) and `concat` (9.8 M) are short rows. Both pay the fixed cost of a larger realm: 66 more function objects, +380,000 instructions and 0.03 ms for an empty script (2.15 M to 2.53 M). On the login boot this is +0.3%. A faster `native()` would cut the cost for every built-in. It is not part of this change.
- `fib`, `throws` and `calls` show +1.8% to +3.8% cycles with no more than +0.22% instructions. Hypothesis: code layout of the larger binary. To verify with a build that adds only dead code. The cycle counts are the minimum of 10 or 20 runs and differ by up to 1% between two measurements of the same build.

Validation:

- 47 Node cases (`tools/fixtures/js/builtins1-cases.txt`, `tools/js/builtins1_check.sh`) match Node 22 byte for byte. They cover holes, negative and fractional indices, array-likes, getter order, species-free results, frozen and sealed arrays and views, error types, V8 messages, descriptors, `groupBy` with iterator close, the legacy accessors, and every Reflect function. The cases with `M(...)` compare only the error name. See the differences below.
- 12 Rust tests (`eval_batch1_tests.rs`): the corpus against a saved result, the gates, the string limits, the step budgets of `lastIndexOf`, `replaceAll` and `normalize`, the deep `flat` limit, equal results of the dense and generic paths on 600-element data, holes, frozen arrays, byte views, `groupBy` close order and Reflect against Object. 5 older assertions changed, because they tested that a name was absent: `typeof 'a'.replaceAll`, `'x'.search`, `Reflect.get`, the Reflect key count, and a realm object cap that rose from 400 to 500.
- `cargo test --workspace --release`: 2,042 tests pass, 0 fail, 9 ignored. One network test in `crates/net` failed once in an earlier full run and passed alone. This change does not touch `crates/net`.

Gates (never silent):

- `normalize("NFKC")` and `normalize("NFKD")`: `Unsupported`. `bare-unicode` has no compatibility tables.
- `toLocaleLowerCase` and `toLocaleUpperCase` with a `tr`, `az` or `lt` locale: `Unsupported`. Other locales use the root rules.
- `Reflect.set` on an integer index of a byte view with a receiver other than the target: `Unsupported`.
- `RegExp.prototype[Symbol.search]` on a non-RegExp object with a callable `exec`: `Unsupported`.
- `reverse` and `copyWithin` on a primitive receiver: `Unsupported`, because they would write to a wrapper object. `flat` and `flatMap` keep the species gate of the other Array methods.
- `matchAll` is not added. It needs a RegExp String Iterator with its own iterator state. It is the one name of the scope that is still missing.

Known differences from Node 22:

- A rejected write keeps the message of the existing `put_prop`: `Cannot assign to read only property 'a' of object`. V8 adds `'#<Object>'` or `'[object Array]'`, and says `Cannot add property b, object is not extensible` for a new property. The error type is the same. The cases use `M` for these.
- `Object.isFrozen(Object.preventExtensions([]))` is `false` here and `true` in V8. The array's `length` is writable, so the specification says `false`.
- `Object.getOwnPropertyDescriptors` of a sloppy function lacks `arguments` and `caller` (the existing gate of `getOwnPropertyNames`).
- A string longer than the heap string limit is `Error::Limit`, not a catchable `RangeError`. This is how every other string builder here behaves.
- `toLocaleString` of an array that holds itself stops at the depth limit. V8 returns `""`. `join` has the same difference.
- Array `toLocaleString` calls `toLocaleString` on each element, and `Number.prototype.toLocaleString` is not bound yet, so `[1].toLocaleString()` throws.

Other problems found and not changed: `Object.create(proto, properties)` was still a gate (fixed after this batch; see below). `Object.getOwnPropertyDescriptor("ab", 0)` returns `undefined` for a primitive string. `hasOwnProperty` converts `this` before the key, and the specification converts the key first. `Array.prototype.slice` takes 62 ms for 1M elements, 60 times more than the in-place paths above.

Not tested: a real page on this build (the live login was not run), `Proxy`, `DataView` and typed arrays other than `Uint8Array` with these methods, locales other than the root rules, and Node versions other than 22.

## Paint and pixel capture

S4 topic 1. The crate is `bare-paint` (`crates/paint/`). Date: 2026-11-05. Machine: archMachine (16 cores), release build.

### Design

- Layout gives a flat list of fragments in tree order. Paint reads that list once and writes a display list (`display.rs`): rectangles, text runs and clips. A second pass draws the list into a `Canvas` (`canvas.rs`). The canvas is a `Vec<u32>` of opaque `0xAARRGGBB` pixels, as `decisions.md` ("Platforms") asks.
- A `Painter` keeps the display list, the text buffer, the scratch lists and the glyph cache between frames. A repeat paint allocates nothing (a test checks the capacities and the pixel buffer address).
- The order is CSS 2.1 Appendix E for each context: own background and borders, block backgrounds, floats, lines (inline backgrounds, text, markers, inline-blocks), positioned boxes. Floats, inline-blocks, boxes that clip and positioned boxes are contexts. The scratch lists are stacks that a context uses and gives back, so the walk is one pass over the fragments plus one visit for each context.
- The root background (or the `body` background) becomes the canvas color, and the element does not paint it twice.
- Rectangles snap to whole pixels like Chrome: each edge goes to the nearest pixel. A border width under 1 px is 1 px, and other widths go down to whole pixels.
- What is outside the canvas makes no item. A box that clips and has no inside on the canvas ends its walk.
- Text: the text of a fragment goes into one `String` (no `String` for each run on the common path). Letters are `font::width_of` apart, in whole tenths of an em as layout adds them, so a run ends where layout says. The baseline is `rect.y + ascent`, as layout computed it, on a whole pixel row. Each letter sits on a quarter of a pixel.
- Glyphs come from Noto Sans Mono Regular and Bold, as subsets of 22 KB each (`data/fonts/`, license `OFL.txt`) built into the program. The subset holds Basic Latin, Latin-1, common punctuation, bullets, boxes and arrows. Built in means that the pixels do not depend on the machine. The layout metrics (600 of 1000 units) equal the advance of the font: a test checks it for both files.
- The glyph cache maps (face, glyph, size, quarter pixel) to a coverage mask. Masks live in one `Vec<u8>`, the map uses a one-multiply hash, and the cache starts again from empty at 8 MB.
- Capture: `png.rs` writes RGB rows through the `png` crate's stream writer: no copy of the whole picture. `bare --paint=OUT.png FILE.html [--width=N]` lays out a local file with no network and writes the window (N by 800).

### Crate choices

Each row is one spike (`target/spike`, not kept) on the same subset font. 95 ASCII glyphs, 200 repetitions, sizes 12, 16, 24 and 48 px. Binary size is the stripped release binary of the spike, minus 345,624 B for an empty `main`.

| Work | Candidate | Time | Binary |
| --- | --- | --- | ---: |
| Fill one glyph outline (µs, 12 / 16 / 24 / 48 px) | tiny-skia 0.12 path fill into a new pixmap, outlines from skrifa 0.44 | 10.1 / 7.8 / 8.7 / 15.0 | +1,118,872 B |
| | skrifa 0.44 and ab_glyph_rasterizer 0.1 | 1.07 / 1.42 / 2.11 / 5.10 | +923,408 B |
| | fontdue 0.9.5 (no fractional x offset in its public API) | 0.36 / 0.39 / 0.53 / 1.09 | +142,976 B |
| | ab_glyph 0.2.32 (with a 0.25 px offset) | 1.64 / 1.14 / 1.30 / 2.28 | +165,328 B |
| Solid rectangles, 10,000 of 60 by 20 px, half translucent (µs for each) | tiny-skia `fill_rect`, no anti-alias | 2.47 | +240,224 B |
| | hand-written row fill and blend | 0.72 | 0 |
| PNG of a 1280 by 800 picture (a page-like test picture) | `png` 0.18.1, `Fast`, `Sub` filter | 1.45 ms, 261,014 B | +161,008 B |
| | `png`, `Balanced` | 17.6 ms, 98,794 B | |
| | own writer, stored deflate blocks | 22.4 ms (naive CRC), 4,097,178 B | +31,360 B |

Choices:

- tiny-skia is not used. A filled glyph costs about 6.5 times more than with ab_glyph (14 to 28 times more than with fontdue; the spike allocates a pixmap for each glyph, and that is part of the cost). A plain rectangle costs 3.4 times more. It adds 1.1 MB to the binary with skrifa (0.24 MB alone, in the rectangle spike). Paint here needs only axis-aligned solid rectangles and glyph masks. tiny-skia comes back when a topic needs paths, gradients or radius. `border-radius` is not parsed yet.
- Rectangles are hand-written (about 30 lines in `canvas.rs`).
- ab_glyph (Apache-2.0) fills the outlines. fontdue is 2 to 5 times faster per glyph and 22 KB smaller, but it cannot place a glyph at a fraction of a pixel, and the glyph cache makes the per-glyph time matter little (see the load test). Chrome and Firefox place letters at fractions of a pixel with Skia. The width of a letter here is 9.6 px at 16 px, so whole pixel places would make uneven gaps.
- `png` 0.18.1 writes the capture. The own writer is 130 KB smaller but makes files 16 times bigger. `Fast` makes a 1280 by 800 capture in 1.5 ms.
- skrifa was not adopted: with ab_glyph (ttf-parser inside) it was 0.76 MB larger and not faster.

Added crates (versions in `Cargo.lock`): `ab_glyph` 0.2.32 (Apache-2.0), `ab_glyph_rasterizer` 0.1.10 (Apache-2.0), `owned_ttf_parser` 0.25.1 (MIT OR Apache-2.0), `ttf-parser` 0.25.1 (MIT OR Apache-2.0), `png` 0.18.1 (MIT OR Apache-2.0) with `fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`, `bitflags`, `simd-adler32`, `adler2`, `cfg-if` and `zlib-rs` (MIT, Zlib or Apache-2.0). Measured on the `bare` binary: stripped 5,384,464 B before, 5,722,024 B after: +337,560 B (+6.3%). The two font subsets are 44,796 B of that.

### Check against Chrome

Method: `google-chrome-stable --headless=new --hide-scrollbars --force-device-scale-factor=1 --window-size=400,800 --screenshot=out.png file.html`. Each fixture gets `* { font-family: 'Noto Sans Mono' !important }` after the doctype, as in the layout checks. `bare --paint=out.png --width=400 fixture.html` gives the other picture. Both are 400 by 800. Pixel difference counts:

| Fixture | Pixels that differ | What differs |
| --- | ---: | --- |
| `backgrounds` (fractional edges, margins, a translucent box, `body` background) | 0 of 320,000 | nothing: the same in every pixel |
| `borders` | 416 | 20 pixels at the corners where sides have two colors (Chrome draws a diagonal, we draw square corners), and the rest is the `dashed` border (we draw `solid`) |
| `order` (clip, float, relative, absolute, inline-block) | 1,396 | glyph shading only: all boxes and their order are the same, and all differing pixels are in text |
| `text` | 7,598 | glyph shading: 17,312 pixels hold ink in one picture or both, and 2,051 of those differ by more than 64 levels. The ink boxes of four checked lines are at the same place to 1 px |
| `login` | 18,872 | glyph shading, and the inputs: layout gives `<input>` only its padding and border (no line of text and no value), so the page is shorter and every box under the first input moves |

The text shading differs because Chrome uses hinted FreeType glyphs and we fill unhinted outlines. The plan says that exact Chrome pixels are not an initial gate.

### Load test

`cargo run --release -p bare-paint --example load -- <case> <n> [canvas height]`. One process for each row. A canvas of 1280 by 800 unless noted. Times are in milliseconds. `list` makes the display list, `raster` draws it into a cold canvas and glyph cache, `repeat` is the whole second frame (list and raster, everything warm), `png` encodes the capture. `list KB` is the memory of the display list, the text buffer and the scratch lists.

| Case | n | Fragments | Layout | list | raster | repeat | png | list KB | Glyphs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| boxes (background and border) | 100 | 102 | 0.0 | 0.01 | 0.50 | 0.39 | 1.7 | | 0 |
| | 1,000 | 1,002 | 0.2 | 0.05 | 0.21 | 0.21 | 1.5 | | 0 |
| | 10,000 | 10,002 | 1.7 | 0.43 | 0.42 | 0.81 | 1.6 | | 0 |
| | 100,000 | 100,002 | 15.3 | 4.55 | 0.30 | 4.41 | 1.6 | 14 | 0 |
| lines (one text line each) | 100 | 202 | 0.1 | 0.02 | 0.72 | 0.43 | 2.9 | | 42 |
| | 1,000 | 2,002 | 0.6 | 0.09 | 0.59 | 0.52 | 3.0 | | 42 |
| | 10,000 | 20,002 | 5.6 | 0.79 | 0.57 | 1.25 | 2.9 | | 42 |
| | 100,000 | 200,002 | 56.6 | 7.88 | 0.69 | 8.72 | 2.9 | 5 | 42 |
| mixed (card, heading, inline boxes, text) | 100 | 1,002 | 0.2 | 0.04 | 0.35 | 0.25 | 1.9 | | 21 |
| | 1,000 | 10,002 | 1.6 | 0.28 | 0.34 | 0.52 | 1.9 | | 21 |
| | 10,000 | 100,002 | 16.8 | 2.74 | 0.44 | 2.99 | 1.8 | | 21 |
| | 100,000 | 1,000,002 | 154.4 | 27.1 | 0.60 | 27.7 | 1.8 | 8 | 21 |
| nested (20 levels) | 100,000 | 105,023 | 22.1 | 3.37 | 1.13 | 4.41 | 1.7 | | 1 |
| dense (a full screen of 4,800 letters) | 100 | 302 | 0.1 | 0.03 | 0.94 | 0.73 | 3.6 | 6 | 69 |
| | 10,000 | 30,002 | 9.8 | 2.03 | 1.07 | 2.79 | 3.6 | 6 | 69 |

Results:

- The list costs 27 to 45 ns for each fragment (the page is walked once, with a style lookup for each box). It is 14 to 30% of the layout time. The raster of a screen costs 0.2 to 1.1 ms, so a 1280 by 800 frame of a page of 100,000 boxes takes 4.4 ms, and one of 1,000,000 fragments takes 28 ms, nearly all of it in the walk over fragments that are not on the canvas.
- The display list holds only what is on the canvas: 334 items for 100,000 boxes, 14 KB. The glyph cache holds 21 to 69 glyphs (up to 6 KB of masks) for a full screen.
- The glyph cache makes a screen of 4,800 letters 4 to 5 times faster: 0.8 to 1.1 ms with the cache, 4.2 ms when the cache is turned off (`MAX_BYTES` 0, measured once and put back). The two measurements are the only comparison; the per-glyph numbers in the spike show the same ratio.
- A canvas of the whole page costs the pixel bandwidth: 1280 by 8192 (42 MB) takes 6 to 8 ms to draw and 29 ms to encode as PNG, and 1280 by 16384 (84 MB) takes 15 ms and 58 ms. A page of 10,000 lines is 220,000 px high, so a capture of the whole page needs a canvas that big (1.1 GB) or a capture in pieces. Not built.
- Extra RSS of a viewport paint (`VmHWM` after `clear_refs`, over the 4.1 MB canvas that already exists) is at most 108 KB in every run (the canvas is the memory that matters: 4 bytes for each pixel, 4.1 MB for 1280 by 800).

Possible savings, not made because the walk is already small: skip a whole subtree that is below the canvas. It is not safe with the data that layout gives: a child can sit outside its parent (negative margins, absolute boxes, overflow), and layout keeps no visual overflow rectangle. Add the rectangle to layout first.

### Gaps

S4 topic 3 closed several of these gaps: `z-index`, `opacity`, flex and grid items, `border-radius`, `dashed` and `dotted` borders, form controls, border-width snapping and part of the missing symbols. See [Paint for the staging pages](#paint-for-the-staging-pages). The list below is the state after topic 1.

- No `z-index` (not parsed). A positioned box paints in tree order over the flow, and its positioned children paint inside it.
- Flex and grid items paint as blocks, not as inline-blocks.
- A box with `overflow` other than `visible` paints as one unit. No scrollbars.
- Borders: only `solid`. `dashed`, `dotted`, `double` and the 3D styles draw as `solid`. Square corners: the top and bottom sides own the corner pixels when two sides differ in color. No radius.
- An inline box cut over two lines draws all four borders on each piece.
- Text decoration is read from the parent of the text only, as a line in the color of the text (no thickness or color from CSS). Italic is not drawn (no italic face). Bold is the Bold face for `font-weight` 600 and more.
- A letter that the font subset lacks (Chinese, Japanese, Korean, most symbols and emoji) is not drawn, and layout still gives it its width. There is no fallback font. A tab or a newline kept by `white-space: pre` is not drawn and has no width in paint.
- No images, no SVG, no gradients, no shadows, no opacity (not parsed), no form control drawing: layout makes an `<input>` with no text, and paint draws its border and background only.
- No hit testing and no scroll offset: the canvas is the top of the page.

Problems seen in other crates, not changed:

- Layout: `border-width: 0.4px` stays 0.4 px. Chrome gives 1 px (a width under 1 px is 1 px). Each such border moves the boxes under it.
- Layout: an `<input>` has no line of text and no `value` text, so its height is only its padding and border.
- `bare --layout` and `bare --styles` accept only `http://` and `https://` URLs. Only `--paint` reads a local file.

## Preload links

Found on 2026-11-05 from the live login trace on main 0798ea3. After the login submit, the Next.js App Router starts a client-side navigation to `/`. It fetches the RSC payload and adds 22 route chunks, which all load. React 19 then adds `<link rel=preload as=style href=...css>` with `appendChild` and waits for its `load` or `error` event before it commits the route. `bare` never fetched a preload link and never fired an event on it, so the navigation hung with no error. Two other gaps showed in the same trace: `HTMLLinkElement.href` was not reflected (`typeof link.href` was `undefined`), and `document.scripts` threw.

Design (`browser_loader/preload.rs`, with a change in `resources::fetch`):

- A preload pass runs inside each style pass, at the same driver boundaries. It runs again after it fires an event, so a link that a `load` listener adds is handled in the same boundary. It reads the document only when `Document::revision` moved.
- A link is handled when it is an HTML `link` in the tree, its `rel` has `preload`, its `href` resolves to an http(s) URL, its `media` matches and its `as` is a destination of the Fetch Standard. A missing or unknown `as` gives no fetch and no event. A link that is detached or changed before its turn is skipped.
- `as=style` goes through `fetch_sheet`, so a later `<link rel=stylesheet>` with the same URL reads the cached text and does not fetch. `as=script` is fetched with the script MIME rules. Its text is kept by URL until the first parsed or dynamic script with that URL takes it. Kept text counts in the total byte cap. Every other destination (`font`, `image`, `fetch`, ...) uses the new `Expect::Opaque`: status, redirects, time and the size cap of one resource apply, the body is read in an 8 KiB buffer and dropped, and nothing is decoded.
- `load` or `error` is dispatched as a task (`res.tasks` grows by one). It never runs inside `appendChild`. A failure counts in `PreloadStats::failed` and names its origin and path in a diagnostic. The fetch count, the byte total, the time limit and the new `preload-links` cap (`max_references`) apply. As in `sheets.rs`, a time, style-size or link cap ends the run. A script or opaque fetch that hits the fetch or size cap gives `error`.
- The same URL is fetched once per document and kind. Each `(link, href text)` gets one event, so a changed `href` loads again and a later `as` starts the load.
- The links of the parsed document are only marked, not fetched. A browser fires their events before any script can listen, `bare` has no HTTP cache, and the staging login has four font preloads that would cost four fetches per page for no gain.
- `HTMLLinkElement` now reflects `href` (the resolved URL, else the text; the setter writes the attribute), `as` (a known destination in lower case, else the empty string) and `crossOrigin` (`null`, `anonymous` or `use-credentials`). `document.scripts` is a snapshot array of the script elements, like `getElementsByTagName`.
- A dynamic `<link rel=stylesheet>`, with or without `data-precedence`, already loaded, applied and fired `load` or `error` when it got its `href` with `setAttribute`. It failed with `link.href = ...` only because `href` was a plain expando property. The reflection fixes that.

Validation. Eleven tests in `preload_tests.rs` use real pages and a loopback server. Their reduced fixture is the React pattern: a timer adds a preload link, the commit waits for its `load`, then it adds the style sheet link with `data-precedence` and waits for its `load`. On main with only a stub of the new result field, 8 of the 9 tests written first failed (the React test logged only `appended`); the detached-link test passes on both, by design. All 11 pass now. They also cover `error` on a 404 and on a wrong MIME type for each kind, a detached link, a link removed in the same task, an unknown or empty `as`, a parsed preload, `load` after `appendChild` and after a microtask, script reuse, one fetch for 30 links to two URLs, a late `as`, a changed `href`, the fetch cap, the size cap, and the reflection. The workspace passes 2,053 tests with `cargo test --workspace --release`. One existing test that pinned the link prototype to the name `rel` now lists the four names.

Load test: `cargo run --release -p bare --example preload_load -- font|style|script|dups 1|10|100`. Each row is one process under 1 GiB of address space, on a virtual clock, one sample, with a loaded machine (load average 10 to 20). Every row checks the event counts and that the server saw one request for each URL. The follow-up stylesheet and script of the `style` and `script` rows cost no request.

| Case (links / URLs) | 1x | 10x | 100x |
| --- | --- | --- | --- |
| font, 10 / 100 / 1,000 URLs, 2 KiB each | 3.1 ms, 7.6 MiB | 10.3 ms, 7.6 MiB | 71 ms, 10.6 MiB |
| style, then `<link rel=stylesheet>` | 2.3 ms, 7.5 MiB | 6.5 ms, 8.3 MiB | 115 ms, 14.6 MiB |
| script, then `<script src>` | 3.0 ms, 7.4 MiB | 4.7 ms, 7.8 MiB | 75 ms, 12.2 MiB |
| dups, 10 URLs | 2.2 ms, 7.5 MiB | 3.7 ms, 7.6 MiB | 21 ms, 10.0 MiB |

The font rows keep no body: 1,000 fonts of 2 KiB cost 3 MiB of RSS over the empty page. The `script` row runs one style pass for each dynamic script (1,003 passes at 100x), as before this change. The style sheet rows from `sheets_load` at 100x take 473/1,019 ms (linked/dynamic) on main and 482/1,027 ms with this change, and 497/1,008 against 475/1,016 ms on a repeat: no change beyond noise.

Gaps: `modulepreload` (modules do not run in `bare`), `type`, `imagesrcset`, `integrity`, `crossorigin` as a CORS mode, `fetchpriority`, `Link:` headers and `prefetch` are ignored. `document.scripts` is a snapshot, not a live `HTMLCollection`. A preloaded script text that a script took is not kept, so a second script with the same URL fetches again. A mixed-content `http:` font gives the diagnostic `insecure script URL refused`, which names the wrong kind. A page that adds more than `max_references` preload links stops with `Limit("preload-links")`. Not tested: the live staging login (the parent runs it), a real browser comparison of event order, and other platforms.

## Batch 2 built-ins: Iterator, Set methods and small built-ins

Batch 2 adds the iterator helpers, the Set methods and a group of small built-ins. `tools/js/builtin_inventory.sh` now prints 202 of 655 missing, down from 241 on main a273925 (and 311 before batch 1). Two of the 202 names are getters that throw on the prototype (`Set.prototype.size`, `Symbol.prototype.description`); the Node cases check them by hand. Older sections of this file that say a name is absent describe the state before this batch.

Added:

- `Iterator` (abstract constructor, a subclass works), `Iterator.from`, `Iterator.prototype` with `map`, `filter`, `take`, `drop`, `flatMap`, `reduce`, `toArray`, `forEach`, `some`, `every`, `find`, `[Symbol.iterator]` and the accessors `constructor` and `[Symbol.toStringTag]`. Array, String, Map, Set, generator and helper iterators all inherit from it.
- `Set.prototype`: `union`, `intersection`, `difference`, `symmetricDifference`, `isSubsetOf`, `isSupersetOf`, `isDisjointFrom`, with GetSetRecord. The `size` getters of Map and Set match Node.
- `Symbol.prototype.description`, `Symbol.dispose`, `Symbol.asyncDispose`. `Boolean.prototype` with `toString` and `valueOf`. `JSON[Symbol.toStringTag]`.
- `EvalError`, `Error.captureStackTrace`, and `Error.stackTraceLimit` (honoured by `new Error` and by `captureStackTrace`).
- `WeakRef` with `deref`, and `FinalizationRegistry` with `register` and `unregister`.
- `Promise.withResolvers`.

Design:

- A helper is a `Copy` record (`Helper`, 136 bytes) in `sc.helpers`, not a generator. It has the states of the specification: suspended-start, suspended-yield, executing and completed. A reentrant `next` throws "Generator is already running".
- A helper reads its source with `iter_next`, so an array, string, Map, Set, generator or helper source needs no result object per step. `for-of`, spread and the consumers read a helper the same way (`iter_native_step`). A chain read by a consumer makes only the helper records and the result: 9 objects for a 1,000,000-item chain. A call of `next()` makes the one result object that the specification requires.
- Closing follows the specification. `take` closes the source on the `next` after its count is reached. A throwing callback closes the source and keeps its own error. An error from the source's `next` never closes it. `return()` on a helper closes the inner iterator, then the source. `flatMap` opens an inner array or string with the same fast paths.
- Order of checks and message text follow Node 22 (V8 12.4): receiver, then callable, then one read of `next`. A bad argument does not close the receiver (later specification versions do). V8 names the method, not the value, for a missing callable.
- The Set methods read `size`, `has` and `keys` once. When the argument is a real Set or Map with the intrinsic `has` or `keys`, the method calls the intrinsic directly and makes no iterator and no result object. The result starts as a copy of the entry list and index of the receiver (`coll_clone`, a memcpy-like copy when no entry was deleted). Methods that walk the receiver use a cursor, so an entry that `has` adds is visited and one it deletes is skipped, as in the specification. A `-0` from `keys()` is stored as `+0`.
- `Error.captureStackTrace` reuses the lazy stack text of `eval_stack.rs`. The default limit is 10. A limit that is not a number, or a deleted limit, gives no stack, as in V8.
- A WeakRef keeps its target out of the roots. A collection clears the target of a ref that was not marked. The registry keeps its callback and held values alive and holds targets and tokens weakly. `collect` runs only between jobs, so the KeepDuringJob list is always empty.
- Cleanup callbacks never run on their own. The host calls `Interp::run_finalizers()` as a task after a collection. It runs each dead cell's callback once and reports a throw to `Host::report_exception`. A host that never calls it gets the "never run" behaviour that the specification allows.
- `Promise.withResolvers` returns a plain object with `promise`, `resolve` and `reject`, made with the intrinsic capability. It uses the same gate as the other Promise statics for a `this` that is not the intrinsic `Promise`.

Gates and deviations (never silent):

- A subclass of `WeakRef` or `FinalizationRegistry` is `Unsupported`, like Map, Set, WeakMap and WeakSet. A subclass of `Iterator` works.
- A non-registered Symbol as a WeakRef target or registry target or token is `Unsupported`, like the WeakMap gate. A registered Symbol is a TypeError.
- `Promise.withResolvers.call(F)` with a user constructor is `Unsupported`.
- `Symbol.dispose` and `Symbol.asyncDispose` follow the specification (non-writable, non-enumerable, non-configurable). Node 22 does not define them in a `vm` context, so the corpus has no case for them. A Rust test checks them. Nothing reads them: there is no `using`.
- `Error.prepareStackTrace` stays absent. `Atomics` stays absent. The stack text is bare's own frame format (as before).
- The realm now holds 38 more function objects and 44 more heap objects (418 and 461, against 380 and 417 on main). The test `the_other_caps_still_apply_to_combinators` had a function cap of 400 that left 20 spare. It is now 600.

Load test (`tools/js/iterator_load.sh`, release, one process per row, setup not counted, n = 1,000,000, Sets of 500,000 with 250,000 shared, one run each on a quiet machine):

| row | operation | time | objects made | peak RSS |
| --- | --- | ---: | ---: | ---: |
| `chain` | `values().map().filter().take(250000).reduce()` | 285 ms | 9 | 19.5 MiB |
| `array_chain` | the same with `map`, `filter`, `slice`, `reduce` of Array | 467 ms | 6 | 46.2 MiB |
| `chain_forof` | the chain read by `for-of` | 331 ms | 8 | 19.4 MiB |
| `flatmap` | `flatMap(x => [x, x]).drop(10).reduce()` | 1,244 ms | 1,000,005 | 381.9 MiB |
| `generic` | `map` over a hand-written iterator object | 972 ms | 1,000,004 | 290.3 MiB |
| `set_union` | `A.union(B)` | 105 ms | 1 | 159.4 MiB |
| `set_inter` | `A.intersection(B)` | 95 ms | 1 | 134.5 MiB |
| `set_diff` | `A.difference(B)` | 82 ms | 1 | 148.0 MiB |
| `set_sym` | `A.symmetricDifference(B)` | 116 ms | 1 | 159.5 MiB |
| `set_like` | `A.intersection(setLike)` (script `has`) | 216 ms | 1 | 134.4 MiB |
| `set_union_like` | `A.union(setLike)` (script `keys`) | 115 ms | 2 | 159.4 MiB |
| `resolvers` | 250,000 `Promise.withResolvers()` + `then` + `resolve`, drained | 913 ms | 1,500,000 | 488.7 MiB |
| `promise_new` | the same with `new Promise` | 991 ms | 1,500,000 | 545.6 MiB |

A helper chain makes no object per step, so it runs 1.6 times faster than the Array methods on this input and needs 42% of their memory. `flatmap` and `generic` make one object per item because the script makes it (the inner array, the result of `next`). RSS grows with those objects because nothing is collected during a run. `Promise.withResolvers` makes as many objects as `new Promise` (6 per promise and handler) and takes about the same time (913 ms against 991 ms in the final run; an earlier run on a loaded machine gave the other order). At x1 and x10 the matrix gives the same shape.

Results, release, main a273925 vs this change, 10 interleaved runs per build, `perf stat -e instructions:u,cycles:u`, minimum of each, with the machine shared with other builds (cycles vary by several percent between runs):

- The realm setup costs 166,000 more instructions (2.525 M on main, 2.692 M here, `eval_check -e 1`). That is +6.6% of a realm and 0.07 ms. It comes from the 38 new function objects (about 4,400 instructions each, the same as the existing ones).
- Instructions at scale 10 on `eval_load`, `gen_load`, `promise_load` and `async_load` (31 rows) are within -0.25% to +1.05%, except `eval_load concat` at +2.79%. Two rows are over +1%: `eval_load fib` (+1.05%) and `concat` (the row is only 10 M instructions, so the setup is 3% of it).
- At scale 1 the setup cost shows: `async_load` rows +3.0% to +4.0%, `eval_load concat` +7.3%, `eval_load webpack` +3.0%, `eval_load fib` +1.8%, `gen_load` rows +0.3% to +1.7%, `promise_load` rows +0.5% to +1.7%. In the async, generator, promise, `concat` and `webpack` rows the excess is a fixed 0.27 M to 0.5 M instructions.
- `eval_load fib` and `calls` grow by about 0.3% to 1% with the scale as well (+3.1 M instructions at x10 on fib, against +0.5 M at x1). A profile of `fib` x10 on both builds shows the same functions in the same order (`eval_expr_inner` 25.3% against 26.2%). Hypothesis: code layout and inlining in `eval_expr_inner` changed with the larger module. To verify: build with `-C codegen-units=1` and compare. Not done.
- Minimum cycles vary from -6% to +12% with no matching change in instructions (one earlier series gave +41% on `promise_load fanout` and +17% on `eval_load calls`, which the next series did not repeat). Hypothesis: noise from the shared machine.

Validation:

- 80 Node cases (`tools/fixtures/js/iterator-cases.txt`, `tools/js/iterator_check.sh`). They cover the shapes and descriptors, inheritance, every helper and consumer, argument validation, order of the reads, laziness, closing on early exit and on a throw in every position, reentrancy, `Iterator.from`, the Set methods with Sets, Maps and set-likes, the order of the reads of the record, the branch that `size` selects, mutation during `has` and `keys`, `-0` and `NaN`, errors, Boolean, `Symbol.prototype.description`, EvalError, `captureStackTrace`, `stackTraceLimit`, `Promise.withResolvers`, the JSON tag, WeakRef and FinalizationRegistry. Two matrix cases hash 300 random helper chains and 120 random Set operations. All match Node 22.18.
- Cases that print a message use the V8 text. Cases whose message bare words in another way print only the error name.
- Rust tests (`eval_iterator_tests.rs`): the corpus without the matrices, the size of a helper, the call order of a lazy chain, closing on early exit, on throw and when done, Set methods with set-likes and their caps, `-0`, objects made per step (fewer than 40 for three chained helpers over 10,000 items, fewer than 20 for three Set methods over 20,000 entries), helpers freed by `collect` and kept alive by their source, callback and inner iterator, reentrancy, gates, WeakRef after a collection, `run_finalizers` (including a throwing callback and `unregister`), the weak caps, the stack limit and `captureStackTrace`, `Promise.withResolvers` order, Boolean and the dispose symbols.
- `cargo test --workspace --release` passes. The other `tools/js/*_check.sh` scripts that run here pass (`parse_check.sh` needs the npm package `acorn`, which is not installed here).

Problems seen, not changed:

- A BigInt literal and `new String(...)` stopped with `Unsupported` in the evaluator when this was written, so some Node cases avoid them. A BigInt literal runs now (see "BigInt").
- A getter that reads `typeof this` on a primitive gives the primitive type in bare. Node (sloppy mode) gives "object".
- `browser_fetch_tests::reused_connections_and_posts_are_never_resent` failed once in a full parallel run and passed in each of 3 solo runs and in the next full run. Hypothesis: a timing race in the test server.
- `tools/js/number_check.sh` printed `limit` for `math_unary_matrix` on the Node side once while the machine was busy. It passed on the next run.

## Wayland window

S4 topic 2. The crate is `bare-window` (`crates/window/`). Date: 2026-11-05. Machine: archMachine (Hyprland, Wayland, 16 cores), release build. `bare --window FILE.html` opens a window with the painted page. The argument can also be an `http(s)` URL: it goes through `get` and the style sheet loader of `--layout`, with no scripts.

### Design

- The interface is small (`lib.rs`). The program implements `App`: `resize(w, h)` says the size, `frame(ctl)` gives `w * h` pixels. `Control` lets a frame ask to close or to draw again. `Waker` is a handle that wakes the loop from another thread and asks for a frame (a timer or a fetch will use it). `Window::open` connects, `run` shows the window and blocks until it closes.
- Only `wayland.rs` knows Wayland. `redraw.rs` (when to draw) and `slots.rs` (which buffer to use) are plain logic with unit tests, so a port to Windows or macOS keeps them. No trait object: a port adds a module with the same names and the app does not change.
- Event loop: `dispatch_pending`, one draw, `flush`, then `poll` on the Wayland socket and on a pipe for the `Waker`, with no timeout. The window never wakes by itself. There is no timer, no polling and no thread. The loop uses `prepare_read`, so a second thread could read the socket later.
- Buffers: `wl_shm` `XRGB8888`, two slots. Each slot has its own pool (`RawPool`, one `memfd`) and one `wl_buffer`. A slot is busy from `commit` until `wl_buffer.release`. A busy slot is never drawn into and never attached. A free slot of the right size is used again, so a window that keeps its size allocates nothing after its first two frames. A free slot of another size is destroyed (`wl_buffer.destroy`, then the pool is unmapped and closed) and made again at the new size.
- Frames: the first `configure` gives the size, and nothing is drawn before it. A frame is drawn only when all four hold: something changed, the window has a size, the callback of the last frame came (`wl_surface.frame`), and a slot is free. Many events in one round make one draw. A `configure` with the size that was already drawn (a focus change) changes nothing.
- Resize: the compositor sends a size, which is recorded and acknowledged by the toolkit. The next draw calls `App::resize`, and `bare` then lays the page out again at the new size and makes a new canvas. The commit with the new size is the first one that the compositor sees, so the old frame stays until the new one is ready. If both slots are busy, the resize waits for a `release`.
- Copy: the canvas (`Vec<u32>`) is copied into the shared buffer row by row (`to_le_bytes`). For 1280 by 800 this takes 0.09 to 0.10 ms, the same as a `memcpy` (4 MB, warm). Painting straight into the shared memory would save that, and needs `Canvas` to borrow a slice. Not done: 0.1 ms is under 5% of a frame.
- Size limits: a side is at least 1 and at most 8192 px (256 MB for a buffer). Size 0 from the compositor means "your choice": the first time it is the `--width` size (1280 by 800), later the size we have.
- Close: `xdg_toplevel.close` or `Control::close` ends the loop. The buffers and the window are dropped, and the socket is flushed before it closes.
- HiDPI: not done. Paint draws one canvas pixel for each CSS pixel and has no device pixel ratio. A buffer scale of 2 with a canvas of the same size would show the page at half size. So the buffer scale stays 1, and a compositor with a scale above 1 enlarges the picture (a blurry window). A sharp window needs a scale in paint (rectangles, glyph size, snapping) and in layout (`vw`, media queries): that is a topic of its own. `OutputState` is bound, so a later topic can read `scale_factor` and `wp_fractional_scale` without a change to the connection code.
- Input: not here. S4 topic 4 added it: see "Window input" below. The toolkit now has the `xkbcommon` feature.
- Style is computed once, at load. A resize runs layout and paint only. A `@media` rule or a `vw` length does not follow a resize until S4 topic 6.

### Crate choice

Two spikes in `scratchpad`, not kept: a window that fills one color, in the stripped release binary (empty `main`: 343,512 B). Both ran 10 s idle on a hyprcage screen.

| Candidate | Binary (stripped) | Over an empty `main` | RSS idle | CPU ticks, context switches in 10 s idle |
| --- | ---: | ---: | ---: | --- |
| `bare-window` (smithay-client-toolkit 0.21.1, no default features) | 856,288 B | +512,776 B | 10.6 MB | 0, 0 |
| winit 0.30.13 (`wayland` only) with softbuffer 0.4 (`wayland` only) | 1,850,848 B | +1,507,336 B | 12.4 MB | 0, 0 |

Choice: smithay-client-toolkit. Both sleep when idle. The toolkit costs a third of the binary of winit. It also keeps the buffer lifecycle in our hands: with winit and softbuffer the buffers, `release` and the frame callback are hidden, and the plan asks to check them. winit would bring a port to Windows and macOS. `decisions.md` ("Platforms") keeps one window module for each OS, and each is a thin layer, so this does not change the choice now.

The `bare` binary (stripped): 5,844,992 B before (main a273925), 6,329,840 B after, +484,848 B (+8.3%).

### Checks

- `cargo test -p bare-window`: 9 unit tests (draw rules, buffer choice, size rules) and `tests/lifecycle.rs`. That test runs the window against a compositor of its own in the same process (`wayland-server`, over a socket pair): no desktop and no `WAYLAND_DISPLAY`. The fake compositor holds a buffer until `release`, fires the frame callback only when told, hashes the memory of every held buffer at every turn, and counts live pools and buffers. It checks:
  - the first frame has the size of the program and carries the number of the frame (the content arrives);
  - an idle window draws nothing for 300 ms and woke up fewer than 10 times (a busy loop would count thousands), and a configure with the same size draws nothing;
  - a frame waits for the callback of the last frame, and five configures before the callback make one frame at the last size;
  - 50 resizes: each gets exactly one frame at its size, at most 3 buffers are alive at any moment (2 at rest), and 30 redraws from another thread (`Waker`) use no new buffer;
  - with both buffers held, a redraw and a resize draw nothing, and write nothing into a held buffer. After the release, one frame has the new size;
  - close from the program and from the compositor both end `run`, and leave 0 pools and 0 buffers;
  - the file descriptors of the process after all scenarios equal the count before.
  A change that lets the program use a busy buffer makes the test fail (tried once, then removed).
- `tests/live.rs` opens a real window and closes it after three frames, with two wakes from another thread. It opens a window, so it runs only with `BARE_WINDOW_LIVE=1` and a `WAYLAND_DISPLAY`. Without them it prints why and passes. `cargo test --workspace --release` therefore never opens a window on a person's screen.
- Live run (hyprcage screen `hc-bare-live`, cage): `live` passed (0.40 s).
- The `http` path of `load_page` was run with `--paint` against a local `python3 -m http.server`: the same hash as the file. `--window` with a URL was not run.

### Measurements

| What | Result |
| --- | --- |
| First frame (`bare --window order.html` and `text.html`, hyprcage screens of 1280 by 800 and 900 by 600): from the start of `show_window` to the commit. It includes the file read, parse, style, the connection, the first configure, layout and paint | 1.67 ms and 1.57 ms |
| Pixels of the window against `bare --paint` (screenshot of the screen; 900 by 600 against the same page painted at 900 wide and cut to 600) | 900 by 600: 0 pixels differ. 1280 by 800: 253 pixels differ, all in the 16 by 23 box of the mouse cursor, which the screenshot draws |
| Idle CPU, 10 s, window open | 0 ticks of 10 ms, 0 context switches. The process waits in `poll`. RSS 13.6 MB, 7 file descriptors, 1 thread |
| Frames and wake-ups for open, a second configure with the same size, close | 1 frame, 1 buffer made, 3 wake-ups, 2 configures |
| Resize to new frame, fake compositor, synthetic app (a canvas that fills one color), `configure` sent to `commit` seen, 50 sizes from 200 by 150 to 599 by 449 | median 0.64 and 0.82 ms, worst 1.2 and 1.5 ms (2 runs) |
| RSS after 50 resizes (change between resize 10 and resize 50) | +544 kB. A run of 500 resizes: -212 kB over the last 490. Buffers alive: 2 at rest, 3 at most |
| Copy of one 1280 by 800 canvas into the buffer | 0.09 to 0.10 ms |
| Stripped binary of `bare` | +484,848 B (+8.3%) |

The resize time of the real page is the number above plus its own layout and paint. Topic 1 measured them: 0.2 to 1.1 ms for the raster of a screen, and layout of a page of 100,000 boxes in 15 ms. It was not measured in a window: cage always sizes a window to its screen, so a hyprcage screen cannot resize one. The resize path is checked on the fake compositor only. The real compositor (Hyprland) was not resized.

### Gaps

- No input events at all (pointer, keyboard, focus): added by S4 topic 4, see "Window input" below. The window cannot be moved or closed from inside the page. The compositor can close it.
- No HiDPI scale (see Design), no fractional scale, no `wp_viewporter`.
- No client-side decorations. `WindowDecorations::RequestServer` asks for server decorations. A compositor with none shows a window with no title bar (cage does this).
- No cursor shape (added by S4 topic 4) and no clipboard.
- `Waker` wakes the loop, but nothing in `bare` calls it yet. Timers, `fetch` and scripts (S4 topic 6, S5) will.
- A hidden window gets no frame callback, so it draws nothing until it shows again. This is wanted. The cost is that a `Waker` request can wait a long time.
- If the compositor never sends `release` (a broken one), both buffers stay busy and the window stops drawing. There is no timeout and no third buffer.
- The page is loaded before the window opens, so a slow URL shows nothing until it loads. A load error prints and exits with 1.
- `bare --paint` now also accepts an `http(s)` URL, because it shares `load_page` with `--window`.

Problems seen in other crates, not changed: none new. The notes under "Paint and pixel capture" still hold.

## Typed arrays, DataView and ArrayBuffer members

This topic is batch 3 of the missing built-ins (see "Boa compared with bare-js"). It adds:

- The 11 typed-array constructors: `Int8Array`, `Uint8Array`, `Uint8ClampedArray`, `Int16Array`, `Uint16Array`, `Int32Array`, `Uint32Array`, `Float32Array`, `Float64Array`, `BigInt64Array` and `BigUint64Array`. `Uint8Array` was the only one before.
- `%TypedArray%`: `from`, `of`, the getters, and every method of the prototype: `at`, `copyWithin`, `entries`, `every`, `fill`, `filter`, `find`, `findIndex`, `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`, `join`, `keys`, `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`, `set`, `slice`, `some`, `sort`, `subarray`, `toLocaleString`, `toReversed`, `toSorted`, `toString`, `values` and `with`.
- `DataView`: the constructor, the getters, and `get` and `set` for 8 element types with `littleEndian`.
- `ArrayBuffer`: `maxByteLength`, `resizable`, `detached` and `transfer`/`transferToFixedLength`.

`tools/js/builtin_inventory.sh` now prints 145 of 655 missing, down from 202 on main 38f55d5. The `SharedArrayBuffer` items are still absent.

Design:

- Elements stay in the byte buffer. There is one `Exotic::View { buffer, offset, len, ty }` for all views, with `Elem` as the type (11 types, and `Data` for `DataView`).
- `load` and `store` (`heap_binary.rs`) convert one element exactly as the spec says. An integer type wraps by modulo 2^n through `to_uint32`. `Uint8ClampedArray` rounds half to even and clamps. `Float32Array` uses `as f32`. A NaN is stored as the canonical quiet NaN.
- A method that does not call JavaScript works on bytes. `fill` writes one encoded element over the range. `copyWithin` and `set` of the same type are `memmove`. `set` of another type converts in runs (`convert_run`). `reverse` swaps in place. The default `sort` sorts a copy of the elements in their own type. For the BigInt types it sorts bytes. Only a method that calls back reads an element as a number, one at a time.
- A callback can detach the buffer. Each callback method reads the element when it needs it, and a missing element reads as `undefined`. A method that is done with JavaScript checks the buffer again before it writes.
- `indexOf`, `lastIndexOf` and `includes` search the bytes. A 1-byte search compares 8 bytes per step with an exact SWAR test (`find_byte`). Wider elements compare fixed-size chunks. Each started 64 bytes costs one step.
- `DataView` get and set compute the error name only when they fail. A call does not allocate.
- A numeric key that is not a valid index (`-0`, `1.5`, `"-1"`, an index past the end) is never a property of a typed array. This follows the spec and V8. A detached view has no index keys.
- Errors use V8 messages. V8 has two wordings for a bad receiver (`this is not a typed array.` and `Method %TypedArray%.prototype.X called on incompatible receiver ...`). The text of the receiver differs too (`#<Object>`, `[object Int8Array]`). `TA_METHODS` records the wording of each method. The detached message of `findLastIndex` says `findIndexLast`, as V8 does.
- Limits: a typed-array constructor checks the byte length against the buffer cap before it allocates. `Symbol` as an element value throws `Cannot convert a Symbol value to a number`.

Gates (each is `Unsupported`, never a wrong result):

- Reading or writing an element of a BigInt array, and the 4 `DataView` BigInt methods. They were gates when this topic landed. The BigInt topic (see "BigInt") removed them.
- `ArrayBuffer.prototype.resize` and the `maxByteLength` option of the constructor. `resizable` is always false.
- A subclass, a `@@species` other than the default, and a `from` or `of` receiver that is not one of the intrinsics.
- `toLocaleString` of a non-empty array, while `Number.prototype` has no own `toLocaleString`. `Object.prototype.toLocaleString` would give `1234.5` where V8 gives `1,234.5`. (`(1234.5).toLocaleString()` has the same wrong result today. It is not part of this change.)
- An object with `valueOf` as the value in `Object.defineProperty` on a typed-array index.

`SharedArrayBuffer` and resizable buffers are not done.

Optimization: the first search was generic and 6% slower than the old `Uint8Array` one. The 1-byte search now uses a word at a time. `indexof` runs 17.5% fewer instructions than before this change. The first `view_own` (the element-read path of a script loop) did 5 hash lookups. It now does 2. `scan` fell from +2.7% to +1.9%.

Load test (`tools/js/binary_load.sh`, release, one process per row, 1 GiB address-space cap, 45 s cap). Rows at 100x:

| row | work | run | peak RSS |
| --- | --- | ---: | ---: |
| `f64from` | 2,000,000 values through `Float64Array.from` with a map function | 0.91 s | 51 MiB |
| `i32sort` | 10,000,000 int32: slice, fill, sort, toReversed, set | 0.24 s | 270 MiB |
| `dataview` | 10,000,000 turns of `getUint32`, `setUint32`, `getInt16` | 13.3 s | 118 MiB |
| `indexof` | 200 MiB view, hits and a miss | 0.09 s | 404 MiB |

All 27 rows (9 cases, 3 scales) check their result against a Rust model and stay inside the caps. A `DataView` turn costs about 1.3 µs. Three calls per turn is the interpreter cost of a method call, not of the conversion.

Results, release, main a3e4c7a vs this change before the rebase onto main 38f55d5 (not measured again after it). Instructions are `perf stat -e instructions:u` (deterministic). Cycles are the minimum of 10 interleaved runs. Over 1% on instructions:

- Fixed start-up cost: 0.37M instructions per realm for the new constructors and prototypes. It shows at x1 in the small rows (`eval_load concat` +10.6%, `webpack` +4.5%, `binary_load copy` +6.8%, `arrobj_load objects` +3.8%). At x100 the same rows are +0.01% to +0.7%.
- `eval_load loop` +1.2% at all three scales and `throws` +1.0% to +1.2%. Neither runs a typed array. Hypothesis: hash layout. The global object has more properties, and the property table (`PropMap`, Fx-style hash) then makes a different collision pattern. Evidence: adding 11 plain globals to the base build gave +1.0% to +1.2% on the same rows.
- `binary_load scan` +1.9% and `scanarr` +0.9%. `scan` reads typed elements, so some of it is real: `view_own` now does the element decode for 11 types. About 1.2% of it is the layout effect above.
- `binary_load indexof` -17.5%. Every other row at x10 and x100 is within -0.7% to +0.7%.
- Minimum cycles at x10 are within -2.7% to +2.7%, except `scanarr` +2.7% and `concat` +7.7% (start-up cost on a 3.6M-instruction row), `copy` +8.9% (a 20M-cycle row).

Validation:

- 63 Node cases (`tools/fixtures/js/typed-cases.txt`, `tools/js/typed_check.sh`). They cover every constructor form, `from` and `of`, each method with edge arguments, conversion of every element type, `DataView` for all types and both byte orders, detaching in callbacks, and the error messages. All match Node 22.18. The 93 cases of `binary_check.sh` still match.
- Rust tests (`eval_typed_tests.rs`): the corpus, every element type against a plain model of the conversion, the canonical NaN, each gate, the byte and step limits, detached buffers, GC with views and `find_byte` against a plain scan at every length and position.

Not tested: BigInt elements (a gate then; the BigInt topic runs them), resizable buffers, a realm with a user-defined subclass, other engines, and the live staging login.

V8 quirks noticed: the receiver wording (above) and `findIndexLast` in a detached message. Both are matched. Node 22.18 gives a result for `fill(v, undefined, end)` that this change does not copy. V8 converts the value before the index in `with`.

## Intl.NumberFormat, Intl.Collator and Intl.PluralRules

The live staging dashboard calls `Number.prototype.toLocaleString` with `en-IN` and `String.prototype.localeCompare`. bare now has `Intl.NumberFormat`, `Intl.Collator`, `Intl.PluralRules`, `Intl.getCanonicalLocales`, and both methods. `(1234567.89).toLocaleString("en-IN")` gives `12,34,567.89`.

Crates and data (ICU4X 2.4, Unicode-3.0 license):

- Crates: `icu_decimal` (digits, grouping), `icu_plurals`, `icu_collator`, `fixed_decimal` (exact decimals and rounding), `icu_locale_core`. All are used with `default-features = false`, so no compiled data is linked.
- Data: `crates/js/src/icu_data` is made by `tools/js/icu_data_gen.sh` (icu4x-datagen 2.4.0, CLDR 49). It holds the 5 English locales of `intl.rs` and the root. The collator reads the root collation: English has no tailoring. The Chinese and other tailorings are dropped. The data is 676 KB of source.
- Currency symbols and digits are not in ICU4X (its currency formatter is unstable). `tools/js/intl_num_gen.mjs` makes `intl_num_data.rs` from the ICU 77 data of Node 22.18. It has 22 symbols, 87 narrow symbols and the non-2-digit currencies.
- Compact notation needs the unstable `CompactDecimalFormatter`. It is a gate.

Data choice, measured (release, stripped `eval_check`, main a3e4c7a is the base):

| Build | Size | Change |
| --- | ---: | ---: |
| main | 1,771,600 B | - |
| ICU4X with its compiled data (all locales and all collations) | 3,220,928 B | +1,449,328 B (+82%) |
| ICU4X with the baked data of 5 English locales (chosen) | 2,291,736 B | +520,136 B (+29%) |

The same code in a small test program (no bare-js): 1,682,072 B with compiled data, 747,056 B with the baked data. Most of the saving is the collation tailorings (2.1 MB of source) and the locales that bare does not format. The text segment grew by 239 KB (ICU4X code and ours). The read-only data grew by 202 KB.

Run time of the chosen build (`eval_check -e`, minimum of 5 runs, machine under load, so wall time is not exact; instructions are):

| Run | Instructions | Peak RSS |
| --- | ---: | ---: |
| `1` on main | 2,146,256 | 11.9 MiB |
| `1` | 2,105,580 | 12.0 MiB |
| first `toLocaleString("en-IN")` | 2,128,212 (+22.6 k over `1`) | 11.9 MiB |
| first `localeCompare` | 2,116,850 (+11.3 k) | 11.9 MiB |
| first `Intl.NumberFormat` made and used | 2,283,698 (+178 k, of which 150 k make `Intl`) | 12.0 MiB |
| 100,000 `toLocaleString("en-IN")` | 1,610 M, 355 ms | 13.0 MiB |
| 100,000 `localeCompare` | 908 M, 107 ms | 11.9 MiB |
| the same loop with string concatenation | 1,058 M, 269 ms | 12.2 MiB |

The first call costs 11 k to 23 k instructions. ICU4X data is read in place, so no data is parsed or copied. The RSS did not change by more than the noise of the run.

Design:

- `Intl` is made on its first read. The realm gets a `Intl` accessor and the methods `toLocaleString` and `localeCompare`. Before this change, the realm made all of `Intl` at set-up (84 k instructions, 4% of an empty run). Now an empty run is 1.9% cheaper than on main. Known deviation: `Object.getOwnPropertyDescriptor(globalThis, "Intl")` shows an accessor until the first read.
- Option reads follow ECMA-402 order and the messages of V8, as for `Intl.DateTimeFormat`. The digit options follow SetNumberFormatDigitOptions, including `roundingPriority`, `roundingMode` and `trailingZeroDisplay`.
- A Number is read as its shortest digits (so `1.005` rounds to `1.01`, as in V8). A string argument keeps its exact digits.
- `toLocaleString` and `localeCompare` make no tracked object. A call with options builds a small struct from the options and uses the shared ICU4X formatter. The formatters and collators are made once and shared.
- Number formatters, collators and plural rules count against `max_formatters` with the date formatters, and `collect` drops the dead ones. The object is not made past the cap.
- `format` and `compare` are getters that return one bound function per object, as in V8.
- A tag such as `en-US-US` is now a malformed tag ("Incorrect locale information provided"), as in V8. Before, it gave "Invalid language tag". This also fixes `Intl.DateTimeFormat`.

Gates (Unsupported, never a silent wrong result):

- `style: "unit"`, `notation` other than `standard`, `currencyDisplay: "name"`, `numberingSystem` other than `latn`, `roundingIncrement` other than 1.
- `formatRange` and `formatRangeToParts`.
- Collator `usage: "search"` and any `collation`.
- A locale with an extension (`-u-`, `-t-`, `-x-`).
- Locales other than the 5 English ones fall back to `en-US`, as for `Intl.DateTimeFormat`.

Known deviations: `Intl.supportedValuesOf` and the other constructors (`RelativeTimeFormat`, `ListFormat`, `Locale`, `DisplayNames`, `Segmenter`) do not exist. A wrong receiver that is an object shows as `#<Object>` or `#<NumberFormat>` in the new code only. The old `DateTimeFormat` code shows `object`.

`Intl.DateTimeFormat` stays hand-written. ICU4X date formatting was not measured here. Its time zone and calendar data would come on top of the 520 KB. Our patterns are 12.6 KB and a formatter costs 105 B. Hypothesis: ICU4X is not smaller. Replace it only if a page needs a locale or a calendar that the tables do not have.

Load test (`tools/js/intl_load.sh`, release, one process per row, 100x = 1M calls, machine under load):

| row | work | run | peak RSS |
| --- | --- | ---: | ---: |
| `nf_format` | 1M `format` calls on one en-IN formatter | 1,344 ms | 101.9 MiB |
| `nf_locale` | 1M `toLocaleString("en-IN")` calls | 1,637 ms | 101.9 MiB |
| `nf_locale_opts` | 1M `toLocaleString("en-US", { style: "currency", currency: "USD" })` calls | 1,941 ms | 4.5 MiB |
| `cmp` | 1M `localeCompare` calls with accents | 786 ms | 4.4 MiB |
| `cmp_opts` | 1M `localeCompare` calls with `{ numeric, sensitivity }` | 1,348 ms | 4.4 MiB |
| `plain` | 1M string concatenations (baseline) | 1,116 ms | 4.2 MiB |

The 102 MiB of `nf_format` and `nf_locale` is the result strings (66 bytes each): the evaluator does not collect during a run. Hypothesis: a string over 7 characters is a heap object, and `nf_locale_opts` makes strings of 7 characters or less. The `locale` row of `Intl.DateTimeFormat` behaves the same way.

A profile of `toLocaleString` is flat: the evaluator (`Heap::intern`, `Heap::lookup`, `link_inner`) is above the ICU4X code. ICU4X (`Cache::format`, the grouper) is about 4%. No fast path was added.

Results against main a3e4c7a, 10 runs per row, minimum cycles and instructions (user), same machine and load (`perf stat`):

- `eval_load` (8 rows, scales 1 and 10): instructions are within -1.45% and +0.24%. The 1x rows are lower, because the realm set-up is cheaper (`concat` -1.44%, `webpack` -0.66%, `fib` -0.18%). `throws` is +0.24% at 10x. Hypothesis: code layout.
- `numeric_load` (16 rows, 1,000 and 10,000): instructions are within -0.75% and +0.15%. Cycles are noise: the load of the machine changed between runs.
- `intl_load` rows that existed before: `format` +0.12% to +0.32%, `parts` +0.11% to +0.96%, `make` +1.79% to +2.88%, `locale` +1.37%. This is a regression over 1% on `make` and `locale`, which build a `DateTimeFormat` with options. The option names grew from 20 to 46, and every option read finds its name in a `match` on the text. A first version that scanned the array cost +9.9%. The `match` brought it to +1.4%. One more small cost is the larger `Intl` made on first use (+25 objects). It is not removed.
- Cycles of all rows vary by more than 5% between runs, because other jobs ran on the machine (load 8 to 12). They are not used for any claim.

Validation:

- 74 Node cases (`tools/fixtures/js/intl-num-cases.txt`, `tools/js/intl_num_check.sh`) match Node 22.18. They cover:
  - grouping and decimals for en-US, en, en-GB, en-CA and en-IN;
  - negatives, NaN, Infinity, -0, 1e21 and 5e-324;
  - rounding (half expand on shortest digits, all 9 modes, significant digits, `roundingPriority`, `trailingZeroDisplay`);
  - percent, currency (INR, USD, JPY, CHF, 12 narrow symbols, accounting, 5 sign displays) and string arguments;
  - `formatToParts` and `resolvedOptions` for each locale;
  - option errors, option read order, receiver errors and the shape of the builtins;
  - collation order with accents, case, numeric, sensitivity, `ignorePunctuation`, `caseFirst`;
  - cardinal and ordinal plural rules;
  - `getCanonicalLocales` and `localeCompare` errors.
- The currency tables are made from Node itself. Any difference in ICU versions shows in the check.
- 90 `Intl.DateTimeFormat` cases still match Node (`tools/js/intl_check.sh`). The Node oracle has a 2 s timeout. Under load it stops a matrix case with `limit`. I ran the oracle once with a 60 s timeout to confirm.
- Rust tests: ICU4X wrappers (grouping, rounding, currency, parts, collation, plural), lazy `Intl`, GC of the new objects, the shared cap, the gates, `new` for `PluralRules`, and the option-name index.
- `cargo test --workspace --release` passes. The checks `eval`, `number`, `builtins`, `date`, `math`, `class`, `intl_num`, `arrobj`, `reflect`, `sc`, `weak`, `tagged`, `generator`, `async`, `promise` and `stringify` match Node. `parse_check.sh` fails with `ERR_MODULE_NOT_FOUND` in Node before it runs our code. It was not run on main.

Limits: one machine, under load. The live staging page was not run on this build. Locales other than the 5 English ones were not tested.

## HTMLElement.dataset and host named properties

Found on 2026-11-05 from the live login trace on main a654a56. After the login, the Next.js client navigation to `/` reaches the commit of React. The stylesheet ordering code of React (`ck` and `cT` in the minified react-dom) reads `element.dataset.precedence` of every `<link rel=stylesheet data-precedence>` and `<style data-precedence>`. `bare` had no `HTMLElement.dataset`, so `dataset` was `undefined`. The trace shows `TypeError: Cannot read properties of undefined (reading 'precedence')`, reported from a `load` listener. The session then ran out of steps.

Design of the hook (`crates/js/src/heap_named.rs`, read its header first). An object made with `Heap::new_named` is an exotic object whose own string-keyed properties belong to the host (WebIDL named properties, "legacy platform object"). The host is a `NamedSource` with four reads (`named_has`, `named_get`, `named_len`, `named_keys`). The heap answers `[[GetOwnProperty]]`, `[[HasProperty]]` and `[[OwnPropertyKeys]]` from them at once, with no evaluator, because every read path of the heap is `&self` or has no evaluator. A write cannot be answered at once: it can throw a `DOMException`, can run `toString`, must charge the step budget and must obey the caps. So `[[Set]]`, `[[DefineOwnProperty]]` and `[[Delete]]` change nothing in the heap. The heap stores one `NamedRequest` and returns `HeapError::NeedsNamed`. The evaluator (`hxw!`, the write form of `hx!`) calls `Host::named_write`, gives the answer back (`complete_named`) and repeats the operation. The repeat returns the answer. This is the loop of `NeedsHost` for host-indexed objects, with a second kind of request.

The cost for an ordinary object is nil. `Object::exotic` is now a `u8` (`EXOTIC`, `EXOTIC_NAMED`), so the object keeps its size. Every dispatch point sits behind the `exotic != 0` test that already guarded views. A read that finds a name leaves the loop of `lookup` through a cold call (`realize`), which gives the real value (it needs the interner). `hx!` is unchanged. Only writes use `hxw!`, because only writes ask for `NeedsNamed`.

`Proxy` did not reuse this request slot. It was built later with its own answer, `HeapError::Proxy`, because a trap result is made in the evaluator (see [Proxy](#proxy)).

`dataset` (`crates/bare/src/browser_dataset.rs`):

- `dataset` is a getter on `HTMLElement.prototype` and `SVGElement.prototype`. It returns one `DOMStringMap` for each element (SameObject). The map holds only its element. Every read and write goes to the live `data-*` attributes. MathML elements have no `dataset`.
- Name conversion: `data-foo-bar` is `fooBar`. A name with an ASCII upper letter in the attribute is not in the map. `in` and `Object.keys` list the `data-*` attributes in attribute order. Other attributes are invisible. An own name hides the prototype chain (`[LegacyOverrideBuiltIns]`).
- The setter runs `ToString` on the value, then throws `SyntaxError` for a name with `-` and a lower case letter, and `InvalidCharacterError` when `data-` plus the name is not a valid attribute name. `delete` removes the attribute. `Object.defineProperty` with a data descriptor is a set. An accessor descriptor is `false`.
- Caps: map objects count against `dom-wrappers`. Written names and values use the checks of `attr_set` (`dom-text`, `dom-attrs`, `dom-names`). An element with more than 32 attributes makes each read ask the evaluator to charge 1 step for 8 attributes before the scan.

Validation:

- 12 tests in `crates/js/src/eval_named_tests.rs` use a plain test host. 17 tests in `crates/bare/src/browser_dataset_tests.rs` use real pages. The test of the React pattern (`react_stylesheet_ordering_reads_dataset_precedence`) failed on a654a56 with the error above, and passes now. 13 dataset tests failed on a654a56 for the same reason. `cargo test --workspace --release` passes.
- Interpreter cost against main c4cc7ec (a654a56 plus later merges; the numbers on a654a56 before the rebase were the same), same example binaries, `perf stat -e instructions:u`, scale 10, one run each (instruction counts do not vary between runs): `eval_load` loop, calls, fib, closures, objects, webpack, throws and concat are between +0.01% and +0.18%. `arrobj_load` has 15 cases: 14 are between -0.09% and +0.09%. The case `arrays` is +0.53% (139.20 M against 138.47 M instructions). It is one hot loop of `lookup`, and a change in code layout moves it. Earlier shapes of the same hook measured +3.5% there: a call that needs `&mut self` inside the loop of `lookup`, and a `NeedsNamed` arm in `hx!`. Both are out. The limit of 0.5% is missed by 0.03% on this one case.
- Load test: `tools/js/dataset_load.sh`, 20 elements, one process for each row under 1 GiB of address space, one sample, machine load average 2 to 7, 16 cores. The checksum comes from Rust, and the native attributes must equal a model after the writes. Time and peak RSS for 1k, 10k and 100k operations:

| Case, data attributes | 1k | 10k | 100k |
| --- | --- | --- | --- |
| read, 1 | 1.0 ms, 7 MiB | 10.2 ms, 7 MiB | 99 ms, 7 MiB |
| read, 10 | 1.3 ms, 7 MiB | 12.9 ms, 7 MiB | 129 ms, 7 MiB |
| read, 50 | 1.9 ms, 7 MiB | 18.4 ms, 7 MiB | 179 ms, 7 MiB |
| write, 1 | 1.3 ms, 7 MiB | 13.7 ms, 7 MiB | 134 ms, 7 MiB |
| write, 10 | 1.5 ms, 7 MiB | 15.2 ms, 7 MiB | 151 ms, 7 MiB |
| write, 50 | 1.7 ms, 7 MiB | 15.9 ms, 7 MiB | 158 ms, 7 MiB |
| `Object.keys`, 1 | 1.8 ms, 7 MiB | 17.6 ms, 10 MiB | 166 ms, 11 MiB |
| `Object.keys`, 10 | 4.1 ms, 7 MiB | 42.7 ms, 13 MiB | 393 ms, 13 MiB |
| `Object.keys`, 50 | 25.9 ms, 8 MiB | 258 ms, 20 MiB | 2,554 ms, 20 MiB |

A read or a write costs 1.0 to 1.8 µs. Time grows in a straight line with the operations, and memory does not grow, because a collection runs every 10,000 operations. `Object.keys` is quadratic in the number of attributes: for each name, `Object.keys` asks `peek_own_property`, and each ask scans the attributes. 50 attributes cost 26 µs. This is the weak point. It is within the step budget (the scan of a long list is charged), but a list with thousands of attributes is slow.

Gates and gaps: a lone surrogate in a name or value is `Unsupported` (the document is UTF-8). `Object.freeze` and `Object.seal` of a map that has names are refused with `TypeError`. `[[PreventExtensions]]` of a map is not forced to `false`. `new DOMStringMap()` is an illegal constructor. A map is not plain data for `structuredClone` or `history.replaceState`. A write on a path without `hx!` (an `h!` site) gives `Error::Heap(NeedsNamed)`, never a silent success. The `peek_own_property` of a name gives the placeholder `undefined` (its callers read only flags). The error texts follow Blink as remembered and are not checked against Chrome. Not tested: a real browser comparison (the oracles in `tools/js` run Node and have no DOM), the live staging login (the parent runs it), and other platforms.

## Window input

S4 topic 4. Date: 2026-11-06. Machine: archMachine, release build. `bare --window http://...` now runs the page in a session and sends pointer and keyboard input to it. A `--window FILE.html` stays a static picture (a session needs an `http(s)` URL).

### Design

- Hit testing (`crates/paint/src/hit.rs`). `hit_test(doc, styles, layout, x, y)` gives the topmost node at a point. It walks the fragments in the same order as `display.rs` (self, blocks, floats, lines, positioned) and keeps the last one that contains the point, so the picture and the hit agree. A test paints a page with overlapping boxes, floats and positioned boxes and checks that the pixel color and the hit node name the same box. Topic 3 (z-index) changes the paint order. The function reads the order from one place in `hit.rs` (`Phase`, `Hit::context`): topic 3 must change that place and the consistency test with it. The cost is one pass over the fragments with no allocation.
- Window events (`crates/window/src/input.rs`, `wayland.rs`). `wl_pointer` and `wl_keyboard` go through smithay-client-toolkit with the `xkbcommon` feature. The window turns them into `Event::{PointerMove, PointerButton, PointerLeave, Key}` with the DOM `button`, `buttons`, `key` and `code` (evdev scancode to `code`, keysym to `key`). All events of one loop round come before the next frame, so a burst makes one frame. Key repeat is a poll timeout from the repeat rate and delay of the compositor: no timer thread, and the loop sleeps without a timeout when no key is down. The cursor shape (arrow, hand over a link or a button, I-beam over a text field) uses the cursor theme of the system.
- Events and default actions (`crates/bare/src/browser_ui.rs`). Events are real `MouseEvent`, `PointerEvent`, `KeyboardEvent` and `InputEvent` objects with `isTrusted` true, dispatched through the normal path (capture, target, bubble, one microtask checkpoint after each listener). The editing writes the same native value and selection state as `input.value` and `setSelectionRange` (UTF-16 offsets). Tab order is the HTML sequential order. The header of the file lists the rules.
- The page (`crates/bare/src/page.rs`). One event: hit test, `Browser::ui_*`, then `Session::resume` runs what the listeners queued (microtasks, tasks, a navigation), then the old layout is dropped and one frame is asked for. A frame cascades and lays out again, paints, and draws what the paint crate does not yet: the text of `<input>` fields, the selection (blue) and the caret (1 px, steady, text color). A password shows bullets. A press in a text field puts the caret at the nearest letter boundary.
- Redraw. Only an event that ran a listener or changed the page (a press, a key) asks for a frame. A pointer move over a page with no listener asks for none. There is no busy loop and no timer.
- Event objects of window input are freed by the heap collector. The browser has a cap of 16,384 live events, so `ui_*` collects when half of it is used.

### Event order against Chrome

`tools/ui/chrome_record.mjs` drives headless Chrome (CDP `Input.dispatchMouseEvent` and `dispatchKeyEvent`, the same pipeline as a device) over `tools/fixtures/ui/events.html`. A script on `window` logs each event with `type @target`, the pointer and key fields, `bubbles`, `cancelable`, `isTrusted` and the constructor name. The log of 25 scenarios is `tools/fixtures/ui/chrome-events.txt`. The test `events_match_chrome` (`browser_ui_tests.rs`) sends the same input to `Page::handle` (the window boundary) and compares every line.

Result: all 25 scenarios give the same lines in the same order. The scenarios are: move into a button, between two elements and out to the page; click on a button, link, text field, checkbox and submit button; typing, capital letters with Shift, Backspace, Delete, arrows, Home, Shift+Arrow, Ctrl+A; Tab and Shift+Tab; Enter in a password field (`keypress`, `beforeinput` with `insertLineBreak`, `change`, a click on the default button, `submit`); Space on a checkbox (`click` on key up, then `input`, `change`); Enter on a submit button; `change` before `blur` after an edit. Chrome facts that the test fixes: a click is a `PointerEvent`, `pointerdown` has `detail` 0 and `mousedown` has `detail` 1, `input` of a checkbox is a plain `Event`, `enter` and `leave` events go only to elements with a listener, a field that gets focus by Tab has its text selected, and `change` fires on blur only when the value differs from its value at focus.

Left out of the comparison: `select` and `selectionchange`. Both are queued tasks, so their place between key events depends on task timing (Chrome puts `selectionchange` before `keyup` in one scenario and after it in another). `browser_selection_tests.rs` tests them.

### Checks

- `hit.rs`: 9 unit tests with no compositor: blocks and the root, text and nested boxes, lines over blocks, absolute boxes, floats and a clipping box, hidden boxes, inline-blocks, list markers, and one test that paints a page and checks that the hit node is the box the painter drew on top.
- `input.rs` (window): 6 unit tests (button and key maps, repeat rule).
- `browser_ui_tests.rs`: 17 tests. They cover the 25-scenario Chrome comparison, tab order (positive `tabindex`, hidden, disabled, `tabindex=-1`, wrap in both directions), editing at the caret and over a selection with an emoji (two UTF-16 units), the caret from a press position, cancel of `keydown`, `keypress`, `beforeinput` (stop the edit) and `input` (not cancelable), a canceled click on a checkbox (undone, no `input`), radio groups, a label click, a link click (navigation request, none when canceled), implicit submission with one field and no button, a disabled button, `isTrusted`, no constructors for the four classes, 10,000 key and move rounds inside the event cap, bullets for a password and the caret and selection pixels.
- Live (hyprcage screen 800 by 600, a local `python3 -m http.server`, `tools/fixtures/ui/login.html`): a click in the user field, typing `arjun`, Tab to the password field, typing `s3cret` (6 bullets, caret after them), a click on the checkbox, Shift+Tab twice, Backspace (deleted the selected `arjun`), Enter. The page showed `submitted: user= pass=6 chars remember=true trusted=true`, `focus: user`, the I-beam over the field and the hand over the label.

### Measurements

Release build, archMachine. In-process, through `Page::handle` and `Page::paint` on a 400 by 400 page of 27 nodes, 10,000 rounds of (one key press and release, one move, a paint after the key, a click every 10th round). The field holds up to 100 letters. Run: `cargo test --release -p bare --lib measure_input -- --ignored --nocapture`.

| What | p50 | p99 |
| --- | ---: | ---: |
| Key press and release: hit test not needed, events, edit, session pump | 6 us | 11 us |
| Key to a painted frame (adds cascade, layout and paint of the page) | 45 us | 63 us |
| Pointer move (hit test and events) | 5 us | 9 us |
| Click (move, press, release) | 32 us | 46 us |

- Cost grows with the length of the value: with 40,000 letters in one field, a key took 76 us and a frame 497 us (the value is measured once for each edit and the text is drawn whole).
- Idle CPU after the live login: 0 ticks in 5 s (the process waits in `poll`). RSS 14.0 MB, 2 threads. The whole live run, from the start to after Enter, used 3 ticks (30 ms).
- Memory over 10,000 events: RSS 16.5 MB after 1,000 rounds, 17.0 MB after 10,000 and 17.3 MB after 20,000 (a round is 3 to 4 window events). The growth is 0.5 MB for 10,000 rounds and slows (270 kB, 240 kB, 290 kB for 4,000, 5,000 and 10,000 rounds). Hypothesis: allocator and heap arena growth, not a leak that grows with each event; not checked with a heap profile. The event table stayed under 4,200 entries.
- The input to frame latency on a real compositor was not measured: the numbers above end at the painted canvas. The compositor adds its own frame time.

### Gaps

- `<textarea>` cannot be edited: the browser has no value state for it (`browser_selection.rs`). `contenteditable`, clipboard keys, Ctrl+Left and Ctrl+Right, IME and `composition` events, `maxlength`, number filtering, `contextmenu`, drag selection and double-click word selection are absent.
- `:hover`, `:focus` and `:active` do not match. The cascade has no input state. Checkbox and radio boxes, `<input type=submit>` labels and the placeholder are not drawn (the paint crate draws controls in topic 3). Until then the demo fixtures give fields explicit borders. Topic 3 may paint the text of fields itself: then the field text in `page.rs` must go.
- Timers and animations that are not due at an input event do not run: the window loop has no timer and `Waker` is not used. A navigation or a `fetch` that a listener starts runs inside `Session::resume` and blocks the loop until it settles.
- A link click starts a navigation, and a form submit now validates and navigates (topic 5). `<base>`, `target`, `download` and `<summary>` toggling are not done.
- Radios are all Tab stops (HTML tabs into the checked radio of a group only). Scrolling came in topic 5 (see "Scrolling").
- `new MouseEvent(...)`, `new PointerEvent(...)`, `new KeyboardEvent(...)` and `new InputEvent(...)` throw `TypeError`: only the window makes these events.
- The window event `PointerLeave` runs `mouseout` and `mouseleave` at the last element, with no `relatedTarget`.
- The isTrusted flag follows the window: focus and `submit` events made by the host are trusted while a window event runs, and not otherwise (Chrome also marks the events of a script call to `focus()` as trusted; here they stay not trusted).

## Paint for the staging pages

S4 topic 3. Crates: `bare-css`, `bare-layout`, `bare-paint`. Date: 2026-11-06. Machine: archMachine (16 cores), release build. The aim is to draw what the staging login and the dashboard need: forms, account menus, rounded cards, layers.

### Design

- **Form controls.** The UA style sheet (`ua.css`) has the Chrome Linux defaults for `input`, `button`, `select` and `textarea` (font 13.3333 px, padding, border, `box-sizing`). `layout/src/controls.rs` gives the size of the content box when `width` or `height` is `auto`. A text field is `size` letters wide (20 when there is no attribute). Chrome counts the letters with the average letter width of the font (`OS/2.xAvgCharWidth`, rounded to a whole pixel) and adds the width of the font box minus that average (`head.xMax - head.xMin`, rounded). For Noto Sans Mono that is 0.644 em and 2.416 em: 203 px at 13.3333 px, so 211 px with padding and border. Buttons are as wide as their label. A `<select>` is as wide as its widest option. A `<textarea>` is `cols` letters (20) plus a scrollbar (Chrome reserves it) by `rows` lines (2). A checkbox and a radio are 13 px. The text line sits in the middle of the content box, and the baseline of the control is the baseline of that line.
- **Text of a control.** A field has no child boxes, so paint draws its text: the `value`, or the placeholder (`#757575`) when the value is empty. A password shows one `•` for each letter. A `submit` or `button` input shows its label in the middle. A `<select>` shows the chosen option. The text is clipped to the content box. A `<textarea>` keeps its text in a child, so the normal text path draws it, and paint draws only the placeholder.
- **Chrome's own look.** Chrome draws a control with its theme when the page does not style the border and the background. Paint does the same test: the four borders, the background and `appearance` must be the ones of the style sheet. Then it draws a 1 px `#767676` border with no pixel in the corners (field, textarea, select), a rounded `#efefef` button with the same border (radius 2.9 px), a checkbox (white, or blue `#0075ff` with a check mark), a radio (a circle and a dot) and the arrow of a select. When the page sets `border`, `background` or `appearance: none`, the control is a CSS box and gets the rounded border, the background and the text of the page. The check mark and the arrow are strokes (`Canvas::stroke`, round caps and joins).
- **Stacking contexts.** A box makes a context when it is positioned (or a flex or grid item) with a `z-index` other than `auto`, or when its `opacity` is under 1. The boxes of a context with a `z-index` other than 0 wait in `DisplayList::hoist` while the walk goes on, with the clip that held them. At the end of the context, they are sorted by `z-index` and then by tree order. The negative ones paint first and are moved in front of the blocks of the context (`slice::rotate_right`), the others paint last. A positioned box with `z-index: auto` is no context, so its children with a `z-index` go up to the context above it. A flex or grid item paints as an inline-block. A page that has no `opacity` under 1 and no `z-index` skips all of this (`Page::stack`, found with one look at each different style, `Styles::each`).
- **Opacity.** `PushLayer` keeps the pixels of the box area (`Canvas::save`), the box paints as usual, and `PopLayer` mixes the new pixels with the kept ones (`Canvas::restore`). Over an opaque canvas this equals a layer of its own mixed in, and there is no second canvas: the cost is one copy of the area. Overlapping children of the box mix once, not twice (a test checks that). A layer of `opacity: 0` paints nothing. The area is the border boxes of the whole subtree.
- **Rounded boxes.** `border-radius` has four corners, with a length or a percent (the part after `/` is dropped). Radii that do not fit are cut by one factor (CSS Backgrounds 3, 5.5). `Canvas::fill_shape` fills the rows between the corners with plain fills and computes only the pixels of the corner boxes: coverage is `0.5 - distance to the arc`, in the pixel center. `Canvas::fill_ring` draws a border as the outer shape without the inner shape (the radius of the inner shape is the radius minus the border width), and looks only at the border strips and the corners. No crate was adopted. The shapes are rectangles with elliptic corners, so two functions in `canvas.rs` cover them. tiny-skia was not measured.
- **Dashed and dotted borders.** Chrome sizes the dashes (`StrokeData::SetupPaintDashPathEffect`): a dash is 3 times the width under 3 px and 2 times above, the gap starts at 2 and 1 times, a dot is the width. Then the gap changes a little so that a whole number of dashes fills the side from end to end. The numbers were measured in Chrome: a 3 px side of 26 px has dashes of 6 and gaps of 4. A dash starts on the pixel that holds its start. A dot of 4 px and more is round, and under that it is a square (Chrome draws squares at 3 px).
- **Border width.** The cascade snaps the computed width as Chrome does: under 1 px is 1 px, above goes down. So layout and paint both use the whole number, and a border of `0.4px` no longer moves the boxes under it.
- **Overflow.** `overflow` other than `visible` (`hidden`, `auto`, `scroll`) clips to the padding box, as in topic 1. The padding box now uses the snapped border widths. Nothing scrolls, and no scrollbar is drawn. A box with a `z-index` that is hoisted keeps the clip that held it.
- **Text fallback.** The two Noto Sans Mono subsets grew from 22 KB to 32 KB each (Latin-1, the punctuation block with `…`, `‹ ›` and the spaces, the currency signs with `₹`, the arrows `← ↑ → ↓ ⇒`, a few math signs, the geometric shapes). A third face, a 4 KB subset of Noto Sans Symbols2 (14 characters, same OFL license, `data/fonts/OFL-Symbols2.txt`), draws `✓ ✔ ✕ ✖ ✗ ✘ ★ ☆ ☰ ⚠ ✉ ✎ ✚`. A letter that no face has (Chinese, Japanese, Korean, emoji) is not drawn. Layout still gives it its width. A system fallback through `fc-match` was not taken: one call takes 27 ms (measured), the answer depends on the machine, and then the pixels of a test would too. The set above is fixed and the cost is 24 KB.

### Check against Chrome

Method as in topic 1 (`tools/paint_diff.sh FIXTURE.html OUT_DIR [WIDTH]` runs Chrome headless and `bare --paint`, then `crates/paint/examples/diff.rs` counts the pixels). Chrome gets `* { font-family: 'Noto Sans Mono' !important }`. The fixtures are in `crates/paint/tests/fixtures/`. "Big" is a pixel that differs by more than 64 levels in a channel.

| Fixture | Topic 1 | Now (differ / big) | What differs now |
| --- | ---: | ---: | --- |
| `backgrounds` | 0 | 0 / 0 | nothing |
| `borders` (3 colors, dashed) | 416 | 164 / 62 | the diagonal where two sides of different color meet (we draw square corners) |
| `order` | 1,396 | 1,396 / 298 | glyph shading only |
| `text` | 7,598 | 7,598 / 2,051 | glyph shading only |
| `login` (topic 1 fixture) | 18,872 | 2,089 / 288 | glyph shading only: every box, border and field is at the same place and the same color |
| `controls` (inputs, checkbox, radio, buttons, textarea, select) | none | 3,502 / 429 | glyph shading, the edge shading of the radio ring and the check mark, the arrow of the select (1 px) |
| `radius` (radii, dashed, dotted, 0.4 px) | none | 1,751 / 628 | rounded boxes and rings: 439 pixels, 4 big (edge shading). Dashes: 184, 148 big (1 px dashes are shaded in Chrome). Dots: 828, 476 big (round dots sit at fractions of a pixel) |
| `stacking` (z-index, opacity, negative z-index, flex item) | none | 7,700 / 0 | nothing over 1 level: the opacity pixels differ by 1 level of gray (hypothesis: Chrome truncates the mix and we round) |
| `overflow` (hidden, auto, scroll, text) | none | 811 / 161 | glyph shading of one text line. The clips are the same |
| `fallback` (₹, arrows, marks, Latin) | none | 5,157 / 1,576 | the symbol glyphs: Chrome uses another font for `✓ ✕ ★` and for `⇒` (wider advance), so the text after it moves |
| `staging_login` (rounded card, inputs with values, checkbox, pill, primary button) | none | 5,092 / 681 | glyph shading only |

Every difference of the new fixtures that is not text is edge shading of a curve or a dash. The boxes, the borders, the clips and the order are the same as Chrome. Chrome draws the text of a control with the same hinted FreeType as other text, so the same shading gap as in topic 1 applies.

### Load test

`cargo run --release -p bare-paint --example load -- <case> <n>`: 1280 by 800, one process for each row, milliseconds. New cases: `rounded` (a 2 px border, `border-radius: 8px`, background), `clipped` (`overflow: hidden` with a child of 3,000 px), `dashed` (a 2 px dashed border, the whole width), `stacked` (positioned boxes, `z-index` from -3 to 3, every third with `opacity: .8`, all on the canvas), `controls` (an input with a value, a checkbox, a select, a button in one row). `boxes` is the topic 1 case, for comparison.

| Case | n | Fragments | Layout | list | raster | repeat | items |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| boxes | 100,000 | 100,002 | 18.7 | 2.69 | 0.53 | 2.97 | 334 |
| rounded | 100 | 102 | 0.0 | 0.01 | 0.63 | 0.44 | 62 |
| | 1,000 | 1,002 | 0.2 | 0.04 | 0.47 | 0.42 | 62 |
| | 10,000 | 10,002 | 1.7 | 0.27 | 0.47 | 0.65 | 62 |
| | 100,000 | 100,002 | 18.3 | 2.69 | 0.56 | 3.16 | 62 |
| clipped | 100 | 202 | 0.1 | 0.02 | 0.39 | 0.22 | 160 |
| | 10,000 | 20,002 | 3.5 | 0.49 | 0.26 | 0.66 | 160 |
| | 100,000 | 200,002 | 35.9 | 4.90 | 0.71 | 5.21 | 160 |
| dashed | 100 | 102 | 0.0 | 0.25 | 0.41 | 0.45 | 14,836 |
| | 10,000 | 10,002 | 1.7 | 0.61 | 0.91 | 1.11 | 14,836 |
| | 100,000 | 100,002 | 17.2 | 2.84 | 0.60 | 3.32 | 14,836 |
| controls | 100 | 902 | 0.2 | 0.07 | 0.80 | 0.75 | 727 |
| | 10,000 | 90,002 | 21.8 | 3.56 | 0.74 | 3.73 | 727 |
| | 100,000 | 900,002 | 226.2 | 35.82 | 0.75 | 31.52 | 727 |
| stacked | 100 | 102 | 0.0 | 0.02 | 0.21 | 0.15 | 168 |
| | 1,000 | 1,002 | 0.3 | 0.23 | 1.49 | 0.88 | 1,668 |
| | 10,000 | 10,002 | 2.2 | 1.41 | 7.79 | 8.39 | 16,668 |
| | 100,000 | 100,002 | 29.1 | 22.24 | 70.02 | 88.13 | 166,668 |

Results:

- The display list stays flat. `rounded`, `clipped`, `dashed` and `controls` give the same item count from 100 to 100,000 boxes: only what is on the canvas makes items (62 items for 100,000 rounded boxes). The first version of `decor` made 10,031 items for 10,000 rounded boxes, because only rectangles were tested against the canvas. Now `decor` returns at once for a box outside the canvas.
- The list costs 27 to 40 ns for each fragment, as in topic 1 (`rounded` 100,000: 2.69 ms, the same as `boxes`). A rounded box costs 2 shapes and 2 items, and the raster of a full screen of them is 0.5 ms.
- A dashed border of the full width is 250 items for a box: 14,836 items for the 60 boxes on the canvas, 0.4 to 0.9 ms to draw.
- `stacked` has all boxes on the canvas, so the raster grows with n: 70 ms for 100,000 boxes (0.42 µs for an item, with 33,333 layers). That is the cost of what is visible, not of the page. One layer costs two items and the copy of its area (40 by 30 px here).
- `controls`: 4 controls in a row cost 9 fragments. The glyph cache holds 16 glyphs (1 KB of masks) for all the values and labels. The scratch list of the lines holds one entry for each inline-block of a parent, also the ones off the canvas: peak extra RSS is 18 MB for 100,000 rows of 4. Topic 1 has the same design. Not changed.
- Size: the stripped `bare` binary is 6,519,600 B. The same build of `main` (00f612a) is 6,431,232 B: +88,368 B (+1.4%). The fonts are 24 KB of that (+20 KB for the two Mono subsets, +4 KB for the symbols face). The rest is code (controls, stacking, shapes, strokes). The glyph cache and the memory of a frame did not grow: the font bytes are in the binary and are read when a glyph is first drawn.

### Gaps

- Images, SVG and icon fonts are not drawn: `<img>` and `<svg>` draw nothing, and the box keeps the size that layout gives it. The check mark glyphs of Symbols2 are drawn as text.
- A character that is not in the subsets (Chinese, Japanese, Korean, emoji, most of Latin Extended) is not drawn.
- Fallback glyphs sit at the pen position and use the advance of layout (0.6 em). Chrome uses the advance of the fallback font, so text after `⇒ ✓ ★` can sit a few pixels from Chrome's.
- Controls: `file`, `color`, `range`, `date` and the `datetime` types, `<select multiple>` and `size` > 1 (a list box), the open menu of a select, the resize grip of a textarea that is `resize: none`, `disabled` and `readonly` looks, focus rings, a caret and a selection are not drawn. Paint reads the `value` attribute. In the window, the host sets `DisplayList::hosted`: it draws the text, the selection and the caret of the text fields, and paint draws only the placeholder of an empty one. A `textarea` has no dirty value. The text-field width is 1 to 3 px from Chrome at font sizes other than 9, 12, 13.3333, 15, 18 and 40 px. A `select` whose page sets `padding` differs from Chrome by a few pixels. The placeholder of a `textarea` does not wrap. Percent padding on a control is read as 0 by paint.
- `text-align` on the value of a text field is ignored (left). Chrome centers the label of a button always; paint does the same.
- Mixed border colors on a rounded box draw square sides. `dashed` and `dotted` on a rounded box draw `solid`. `double`, `groove`, `ridge`, `inset` and `outset` draw `solid`. The vertical radii after `/` are dropped. `overflow` clips to the square padding box, not to the rounded one.
- A positioned box whose containing block is not its parent is in the fragment list under its containing block, not under its parent. So the `opacity` layer of the parent does not include it, and a negative `z-index` inside such a parent goes to the context of the containing block. Chrome follows the parent in both cases.
- `z-index` on an inline box does nothing. `opacity` on an inline box does nothing. `isolation`, `mix-blend-mode`, `filter` and `transform` do not make contexts.
- No scrollbars and no scroll offset (as in topic 1).

Problems seen in other crates, not changed:

- `bare --paint` always makes a canvas of 800 px height, so a Chrome capture must use a window of the same height to compare.
- `getComputedStyle` now lists `opacity`, `z-index` and the radius longhands. Two tests of the `bare` crate listed `opacity` and `zIndex` as "not supported". They are changed to match.

## Primitive wrapper objects

Found on 2026-11-06 from the live login on main 8599baf. After the token is stored, the script runs `new String(...)` or `Object(x)`. The engine stopped with `Fatal(Unsupported("wrapper object (new String/Number/Boolean)"))` from `eval_classes.rs`. The `dataset.precedence` `TypeError` of the earlier run was gone.

Design (`crates/js/src/heap_prim.rs`, `crates/js/src/eval_wrapper.rs`; read their headers first):

- A wrapper is an ordinary `Object` plus one entry in the `Heap::exotics` table: `Exotic::Prim(Value)`. This is the slot for `[[StringData]]`, `[[NumberData]]`, `[[BooleanData]]` and `[[SymbolData]]`. The `Object` struct does not grow. An ordinary object pays nothing: the table is read only when the `exotic` byte is not 0.
- A String wrapper has the flag `EXOTIC_STR`, next to `EXOTIC` (views) and `EXOTIC_NAMED` (host names). It answers `length` and each index below the length from the string: `[[GetOwnProperty]]` (`exotic_own`), `[[OwnPropertyKeys]]` (indices, own keys by index, then `length`, then the strings), `[[DefineOwnProperty]]` (`prim_define`: `ValidateAndApplyPropertyDescriptor` against the virtual property) and `[[Delete]]` and `[[Set]]` (the virtual properties are read-only and not configurable). The one-unit string for an index is made by `realize`, on the cold path of the host-named hook. Number, Boolean and Symbol wrappers have no virtual keys and no flag.
- The collector marks the `Value` of the entry. A dead wrapper frees its entry.
- Construction: `new String(x)` (ToString, a symbol is a `TypeError`), `new Number(x)`, `new Boolean(x)`, `new.target` for a subclass, `Object(x)`, `new Object(x)` and ToObject for string, number, boolean and symbol. `Object(undefined)` and `Object(null)` make a plain object.
- `thisNumberValue`, `thisStringValue`, `thisBooleanValue` and `thisSymbolValue` accept a primitive, a wrapper and the prototype itself, else a `TypeError`. `Number.prototype.valueOf` and `String.prototype.valueOf` are new.
- A primitive `this` of a sloppy function becomes a wrapper (`bind_call`). A strict function and a native method see the primitive. A primitive receiver of an Array method becomes a wrapper first (`ao_recv`), so `Array.prototype.map.call("ab", f)` passes a String wrapper to `f`.
- A property read, a method call or `length` on a primitive never boxes. `"a".length` and `(1).toFixed()` take the old path.
- Other rules: `typeof` is `"object"`; a wrapper is truthy, also `new Boolean(false)`; `==` unwraps through ToPrimitive (`new Number(1) == 1`); `===` compares identity; `Object.prototype.toString` gives `[object Number]`, `[object String]`, `[object Boolean]` and `[object Symbol]`; `JSON.stringify` unwraps a Number, String or Boolean wrapper once, after `toString` or `valueOf` have run once, in order, and a Symbol wrapper is `{}`.

Gates removed: the wrapper `Unsupported` in `new`, `Reflect.construct` and the `D::Native` match; `Array.prototype.reverse`, `copyWithin`, `concat` and `fill` on a primitive receiver; `Object.assign` with a primitive target; the String wrapper `this` of callbacks; the `Number.prototype` and `Boolean.prototype` notes in `eval_numeric.rs`, `eval_intl_num.rs`, `eval_symbols.rs` and `eval_object_more.rs`.

Not done:

- `Object(1n)` and BigInt wrappers: done in the BigInt change (`to_object` and `thisBigIntValue`).
- `structuredClone`: the engine has none.
- `String.prototype`, `Number.prototype`, `Boolean.prototype` and `Symbol.prototype` are ordinary objects with a special case in the four `this` functions. `Object.getOwnPropertyNames(String.prototype)` has no `length`.
- Two older differences remain: the read-only assignment and `delete` messages, and the `toString` radix message differ from V8. The Node cases use `e.name` to avoid them.

Validation:

- 8 tests in `crates/js/src/eval_wrapper_tests.rs` and 2 unit tests in `heap_prim.rs`. The Rust tests failed first, on the old gate. `tools/js/wrapper_check.sh` runs 32 cases against Node 22: all match.
- Memory: a wrapper costs one object and one table entry, and no string, property or element vector (a test asserts 1000 wrappers make 1001 objects and 1000 exotic entries, and at most 120 accounted bytes for each; measured: 104 bytes, with the wrapper held in an array). Dead wrappers are collected.
- Caps: the heap cap stops the creation of wrappers and cannot be caught. The key list of a String wrapper is charged before it is built.
- Interpreter cost against main 8599baf, same example binaries, `perf stat -x, -e instructions:u`, minimum of 10 runs, scale 10 (`numeric_load` at 100000), 36 rows. A first build of this change cost +0.6% to +2.1% on almost every row. The cause was inlining: `validate_apply` had one caller (`ordinary_define`) and a second caller in `prim_define` made the compiler stop inlining it, so every property definition paid a call. `#[inline(always)]` on `validate_apply` fixed it. Result: 36 of 37 rows are between -0.40% and +0.24%, and 24 rows run fewer instructions than main. `arrobj_load objects` is +0.51% (75.36 M against 75.74 M instructions, a short row), 0.01% over the limit of 0.5%. Rows:

| Row | main 8599baf | this change | Change |
| --- | --- | --- | --- |
| eval_loop | 12,676,628,068 | 12,632,641,684 | -0.347% |
| eval_calls | 8,416,848,410 | 8,392,861,070 | -0.285% |
| eval_fib | 293,460,584 | 293,287,392 | -0.059% |
| eval_closures | 1,214,534,811 | 1,216,096,652 | +0.129% |
| eval_objects | 4,128,476,156 | 4,126,023,187 | -0.059% |
| eval_webpack | 56,716,385 | 56,730,862 | +0.026% |
| eval_throws | 3,142,214,941 | 3,135,579,814 | -0.211% |
| eval_concat | 10,945,135 | 10,938,925 | -0.057% |
| binary_copy | 28,339,879 | 28,353,001 | +0.046% |
| binary_scan | 7,665,987,821 | 7,655,000,233 | -0.143% |
| binary_scanarr | 16,896,742,417 | 16,892,769,376 | -0.024% |
| binary_codec | 423,302,426 | 423,315,814 | +0.003% |
| binary_stream | 396,196,379 | 396,212,116 | +0.004% |
| binary_indexof | 493,320,489 | 493,147,417 | -0.035% |
| binary_f64from | 1,004,481,314 | 1,004,094,546 | -0.039% |
| binary_i32sort | 703,381,169 | 703,394,377 | +0.002% |
| binary_dataview | 15,334,462,435 | 15,341,483,361 | +0.046% |
| arrobj_mapfilter | 303,326,692 | 303,240,192 | -0.029% |
| arrobj_sort | 1,022,532,625 | 1,021,045,121 | -0.145% |
| arrobj_queue | 290,121,288 | 290,240,660 | +0.041% |
| arrobj_objects | 75,355,225 | 75,741,225 | +0.512% |
| arrobj_arrays | 139,199,964 | 139,097,699 | -0.073% |
| arrobj_fill | 407,958,425 | 406,756,054 | -0.295% |
| arrobj_reverse | 3,864,232,694 | 3,859,046,002 | -0.134% |
| arrobj_flat | 713,542,455 | 712,573,707 | -0.136% |
| arrobj_tosorted | 938,890,760 | 938,026,706 | -0.092% |
| arrobj_lastindexof | 4,108,955,628 | 4,112,469,716 | +0.086% |
| arrobj_pad | 24,554,129 | 24,567,655 | +0.055% |
| arrobj_repeat | 6,523,436 | 6,538,943 | +0.238% |
| arrobj_replaceall | 350,796,057 | 350,770,889 | -0.007% |
| arrobj_freeze | 2,023,799,709 | 2,024,105,653 | +0.015% |
| arrobj_descriptors | 72,268,343 | 72,140,347 | -0.177% |
| numeric_tofixed | 1,345,782,668 | 1,343,575,267 | -0.164% |
| numeric_toprecision | 1,347,159,532 | 1,344,951,965 | -0.164% |
| numeric_radix16 | 1,477,683,811 | 1,473,229,763 | -0.301% |
| numeric_parsefloat | 1,945,061,524 | 1,941,569,238 | -0.180% |
| numeric_empty | 820,679,379 | 817,374,615 | -0.403% |

- Load test: `tools/js/wrapper_load.sh`, 12 kinds at 1k, 10k and 100k iterations, one process for each row under 1 GiB, checksums from Node. Net cost for each iteration, at 100k: `new Number(i).valueOf()` 0.55 µs, `new Boolean` 0.63 µs, `Object(i)` 0.54 µs, `new String('x' + i).length` 1.04 µs, `new String('abcdef')[i % 6]` 0.72 µs, `new Number(i).toFixed(1)` 0.92 µs, `new Number(i) == i` 0.62 µs, `JSON.stringify(new String('a'))` 1.17 µs, `Object.keys(new String('abcdef'))` 1.82 µs. The control, a method call on a primitive (`(i).toFixed(0).length + 'abc'.length`), costs 0.73 µs and allocates no wrapper. The time for each iteration is flat from 1k to 100k, so the cost is linear. Peak RSS grows with the garbage until a collection (`newnumber` 4.9 MiB at 1k, 19 MiB at 100k), and does not grow with the live set.

## Scrolling

S4 topic 5. Crates: `bare-layout`, `bare-paint`, `bare-window`, `bare`. Date: 2026-11-06. Machine: archMachine, release build. Chrome: Google Chrome 155.0.8059.39, headless, 400 by 300 window, classic scrollbars.

### Design

- Layout makes fragments in page coordinates and never runs again for a scroll. `bare_layout::scroll::areas` reads the fragments once. For each box with `overflow` other than `visible`, and for the page, it gives the padding box, the bars, and the size of the scrollable overflow (`scrollWidth`, `scrollHeight`). A `position: fixed` box does not count for the page, and what a container holds does not count for the boxes around it.
- The offsets live in `Dom::scrolling` (`bare_paint::Scroll`: the page, and one pair for each container). Offsets are whole pixels. They are clamped to `0..=scrollWidth - clientWidth` when they are set, and again at each frame.
- Paint builds one display list for the whole page (`Painter::build`) and draws it with the offsets (`Painter::draw_view`). A scroll does not build the list again. `PushScroll` and `PopScroll` move what is inside a container. `PushFixed` puts a fixed box back at the window. The bars are items of the list (`Bars`) and the bars of the page are drawn after it.
- A hit test takes a point in window coordinates (`hit_test_scrolled`). It adds the page offset for the boxes of the page, and the offset of a container when it goes into one. A point on a bar hits the container.
- The page has bars when its content is bigger than the window. A bar takes 15 px, so layout runs again at the smaller size. `Dom::layout_areas` keeps the bar state, so a page whose bars do not change is laid out once.
- The window sends one `Event::Wheel` for each `wl_pointer` frame (`value120` is used when the compositor gives it; 120 units are one tick of 53 px). The wheel goes to the innermost container under the pointer that can move that way, then to the page (chaining). The window skips the hit test when the page has no container.
- Keys scroll the page when the focus is not a field that uses the key: arrow 40 px, Page keys `max(h * 0.875, h - 40)` rounded down, Space and Shift+Space one page, Home and End the ends. A key that only scrolls does not make a new frame (no cascade, no layout). Tab and Shift+Tab to a field that is out of view scroll it in: centered when none of it shows, the nearest edge when part of it shows.
- Script API: `scrollTop`, `scrollLeft`, `scrollWidth`, `scrollHeight`, `clientWidth`, `clientHeight`, `clientTop`, `clientLeft`, `getBoundingClientRect` (minus the offsets), `scrollTo`, `scroll`, `scrollBy` and `scrollIntoView` on elements. `scrollX`, `scrollY`, `pageXOffset`, `pageYOffset`, `innerWidth`, `innerHeight`, `scrollTo`, `scroll` and `scrollBy` on the window. `document.scrollingElement`. Reading an offset costs nothing. Any other read, and every write, run the cascade and one layout, like `offsetWidth`.
- The `scroll` event: a change of an offset queues its target once. One task 16 ms later (the window: right after the input) sends one event to each queued target. The event of the page goes to the document and bubbles to the window. The event of an element does not bubble.
- History of documents (`browser_session.rs`): the session keeps the URL of each document, and the scroll of each when it ends. Back and forward past the entries of one document load the neighbor with a `GET`, then put its scroll back. A new navigation drops the entries after it.
- Forms (`browser_submit.rs`): Enter in a field, a submit button and `requestSubmit()` validate the form. If it is invalid, each invalid control gets an `invalid` event, the first one gets the focus, and no `submit` event follows. If no listener cancels `submit`, the entry list is encoded (`application/x-www-form-urlencoded`) and the form loads its action: `GET` replaces the query and drops the fragment, `POST` sends the body once.

### Check against Chrome

- `tools/ui/chrome_scroll.mjs` runs the 43 steps of `tools/fixtures/ui/scroll-steps.txt` on `tools/fixtures/ui/scroll.html` in headless Chrome and writes `chrome-scroll.txt`. The test `scroll_matches_chrome` runs the same steps in `bare` and compares each result as text. All 43 match: sizes (`clientWidth` 385 and `scrollHeight` 1700 of the page, 310 by 410 for the `#box` container), rectangles before and after a scroll, clamping, `scrollIntoView`, the key sizes (40 px, 262 px for Page Down), and the counts of `scroll` events. Chrome sends one event for each target and none for a scroll that moves nothing. The test waits for the task between two steps, as the Chrome run does.
- Pixels: the test `the_page_scrollbar_has_the_pixels_of_chrome` checks the bar of a page of 1,000 px at four offsets against runs of pixels that Chrome gave. The track is `#fcfcfc` and 15 px wide, the arrows are `#8b8b8b` in 5 rows (rows 6 to 10 and 288 to 292), and the thumb is `#8b8b8b`. The start of a thumb is allowed to differ by one row and one shade (`#8e`): the round ends are drawn as a shape here and shaded in Chrome (hypothesis: curve shading, not checked). No other pixel difference was recorded.
- Not compared with Chrome: the wheel tick of 53 px (it is the Linux value from memory; a live wheel of 20 clicks moved the page by 1,060 px, which only shows that the code uses 53), the history of documents, and the validation rules (written from the HTML Standard).

### Measurements

Release build, archMachine, through `Page::handle` and `Page::paint` in a window of 800 by 600. The page is LINES lines of text in 20 blocks. Run: `cargo run --release -p bare --example scroll_load -- LINES`.

| What | 10,000 lines | 100,000 lines |
| --- | ---: | ---: |
| Parse and realm | 4.4 ms | 22 ms |
| First paint (cascade, layout twice for the bars, display list, raster) | 15.6 ms | 140 ms |
| Allocations of the first paint | 41,496 (7.4 MB) | 401,511 (68 MB) |
| RSS after the first paint | 14.5 MB | 46.1 MB |
| Wheel step with a paint, p50 / p99 / max | 0.34 / 0.39 / 0.57 ms | 0.87 / 0.98 / 1.5 ms |
| Allocations of a wheel step | 6 (829 bytes) | 6 (829 bytes) |
| Layouts during 2,000 steps | 0 | 0 |
| Arrow or Page key (press, release, paint), p50 | 0.35 ms | 0.91 ms |
| RSS after 2,000 steps | 15.5 MB | 46.2 MB |
| Idle CPU in 3 s after the steps | 0 ms | 0 ms |
| Click (move, press, release), p50 / p99 | 13.8 / 15.8 ms | 145 / 313 ms |

- Before the fix, the same step took 1.3 ms and 3.3 ms. A profile showed two passes over all the fragments for each step: the hit test of the wheel, and the search for the text fields in `Page::paint`. The window now skips the hit test when the page has no container, and finds the fields once for each built list. The 6 allocations of a step are the copy of the offsets and the event lists.
- The time of a step still grows with the page (0.34 ms to 0.87 ms for 10 times more lines): the draw walks the whole display list and skips what is out of view item by item. A list that is indexed by position would remove that. It was not built, because a step is under 1 ms at 100,000 lines.
- Keys did a new layout twice for each key (the key down and the key up) before the fix. A test (`a_scroll_key_does_not_lay_out_again`) checks that they do not now.
- A click costs a new frame: the pointer button and the key down always report a change (`ui_call(true, ...)`, from topic 4), so the next paint runs the cascade and the layout again. On 10,000 lines that is 13.8 ms, on 100,000 lines 145 ms (200 layouts for 100 clicks). It is not a scroll cost, and it was not changed. Fix: report a change only when a listener ran or a default action changed a node.

### Gaps

- `position: sticky` is not modelled: a sticky box scrolls as a normal one. `scroll-behavior`, `scroll-snap` and `overscroll-behavior` are ignored, and every scroll is instant (Chrome animates a wheel or key scroll and ends at the same place).
- The keys scroll the page only, not the container that holds the focus. A script `focus()` does not scroll; Tab does. No `wheel` event is sent to the page. Touchpad kinetic scrolling is not built.
- `scrollWidth` and `scrollHeight` of an element that does not clip are its padding box. `getBoundingClientRect` gives a plain object with eight fields. `100vh` and `vw` use the window minus the bars; Chrome uses the window.
- Scroll restoration inside one document is not done (`history.scrollRestoration` is stored only). A document that a traversal loads again gets its scroll back. Its `history.state` and its form values do not come back. `history.length` counts the entries of the document, not of the session. An entry that a form `POST` made is not loaded again (a body is never sent twice), so back and forward onto it do nothing.
- Validation: `pattern`, `step`, date types, `setCustomValidity`, `validity` and `validationMessage` are not built. `<select>` and `<textarea>` are not validated, and are sent with the value of their markup. No message bubble is drawn for an invalid field: the window focuses the first one and scrolls to it. `enctype` `multipart/form-data` and `text/plain` with `POST` are `Unsupported`. `target` is ignored.
- A window title shows the first URL only (it does not follow a navigation).
- The live staging login was not run on this build.


## Pending style sheets

Chrome 155.0.8059.39, headless, measured with `tools/ui/chrome_pending_sheet.mjs`. A local server sends each sheet after a delay (400 ms in most cases). The page reads `getComputedStyle(#t).color` and `document.styleSheets.length`. The full log is `tools/fixtures/ui/chrome-pending-sheets.txt`. "No rules" means the read gives the page without the sheet, and the sheet is not in `document.styleSheets`.

| Case | What Chrome shows |
| --- | --- |
| A1 script inserts a `<link rel=stylesheet>` | Same task, microtask and timer 0: no rules, `sheets=0`, `link.sheet` is `null`. In the `load` event of the link: rules, `sheets=1`. |
| A2 script-inserted link, 404 | No rules. `error` event. |
| A3 link inserted with a URL that an earlier stylesheet link already loaded | Rules in the same task (memory cache), `sheets=2`. |
| A4 same, but the URL was only read with `fetch()` | No rules until the link loads. `fetch` does not share the cache. |
| B1 script changes `href` of a loaded link | The old sheet stays (rules, `link.sheet` set) until the new sheet loads. Then the new rules replace it. |
| B2 script sets `href` on a link that had none | No rules until load. |
| C1, C2 parser-inserted link in `head` or in `body`, then an inline script | The script waits for the sheet (it ran at 400 ms) and sees the rules. |
| C3 inline script before a parser-inserted link | The first script sees no rules. The second waits and sees them. |
| C6, C7 external script after the link, `defer` script | Both wait for the sheet and see the rules. |
| C4 parser link `media=print` | Does not block. No rules. Chrome loads it anyway (the request is sent). |
| C5 parser link `disabled` | Does not block. No rules. No request. |
| D1 preload (`as=style`) loaded, then `rel` set to `stylesheet` on the same link | Rules in the same task, `sheets=1`. `load` fires again. |
| D2 preload loaded, then a new stylesheet link with the same URL | Rules in the same task, `sheets=1`. |
| D3 preload still loading, stylesheet link with the same URL | No rules until it loads. |
| E1 script-inserted link `media=print` | Loads (event at 100 ms), `sheets=1`, no rules. Setting `media=all` gives the rules in the same task. |
| F1 script-inserted link with `disabled` | No request, no rules, `sheets=0`. After `disabled=false` it loads, no rules until then. |
| F2 loaded link, `disabled=true` then `false` | Rules go away and come back in the same task. |
| G1 loaded link removed, then inserted again | Rules go away on removal. They return on insertion in the same task. |
| H1 link inserted with `innerHTML` | Same as A1: no rules until it loads. |
| I1 script-inserted `<style>` with `@import` | The rules of the style text apply at once. The imported rules come at 300 ms. `sheets=1`. |
| J1 two script-inserted links, slow then fast | The fast sheet applies while the slow one waits (sheet by sheet). |

Rules that follow, in `browser_computed.rs`, `browser_loader/sheets.rs` and `browser_loader/preload.rs`:

- Only a sheet that is loaded has rules. A script-inserted sheet that is not loaded adds none, and a read does not stop the page. This is `Browser::set_pending_sheets(true)`. The page loader sets it when the first pass has supplied every sheet of the parsed document. The pre-parsed document has those sheets before the first script, as Chrome has (C1 to C3, C6, C7). An embedder that never sets it keeps the old stop (`Unsupported`).
- A link keeps the sheet it had until a new `href` is supplied (B1). `style_availability` binds each link to its sheet at each pass.
- A style preload that loaded supplies its sheet under its `href` text before its `load` runs (D1, D2). A later stylesheet link with the same text has it in the same task. The key is the exact `href` text. The same URL in another text is a new sheet in bare and a cached one in Chrome (to verify: it reads as no rules for one task).
- Still a stop, on purpose: a link whose old sheet is gone, and a link with a `media` text. Chrome had loaded a link that its `media` turned off (E1, C4), bare did not fetch it, and a read cannot tell this case from a new link with `media=all` (a rare case).
- Gaps that stay: `<link disabled>` is fetched and applied (C5, F1, F2: Chrome does neither). `media=print` links are not fetched (Chrome fetches them). `document.styleSheets` does not exist. When it does, a pending sheet must not be in it (A1).

Cost: no sheet is pending in these rows, so the cost is the bookkeeping for the bindings. User instructions, `perf stat -e instructions:u`, minimum of 10 runs, main 5978d55 against this build:

| Row | main | build | change |
| --- | --- | --- | --- |
| sheets_load linked 10 | 88,268,793 | 88,593,836 | +0.37% |
| sheets_load linked 100 | 6,527,553,324 | 6,540,344,286 | +0.20% |
| sheets_load imports 100 | 9,745,707,344 | 9,748,269,309 | +0.03% |
| sheets_load dynamic 10 | 135,953,226 | 136,516,876 | +0.41% |
| sheets_load dynamic 100 | 14,794,123,128 | 14,841,571,372 | +0.32% |
| style_load setget 100 | 7,660,011,749 | 7,659,144,875 | -0.01% |
| style_load churn 100 | 7,165,624,716 | 7,166,546,657 | +0.01% |
| style_load attrs 100 | 5,749,161,487 | 5,748,833,568 | -0.01% |
| style_load shorthand 100 | 19,856,976,891 | 19,856,059,588 | -0.00% |
| browser_load mutation 100 | 11,265,490,916 | 11,265,166,000 | -0.00% |
| browser_load lookup 100 | 12,081,733,772 | 12,083,970,492 | +0.02% |
| browser_load event 100 | 8,622,850,847 | 8,621,445,263 | -0.02% |

A first version rebuilt the binding table at each pass and cost +1.4% on `dynamic`. It now changes only the bindings that differ.

The staging login chunks hold three reads of `getComputedStyle` (saved login scripts, `tools/staging_js_fetch.py`): `shouldSkipElement` of the Next.js App Router layout router (chunk `2117`: `getComputedStyle(e).position` for `sticky` or `fixed`, in `handlePotentialScroll`, which runs in the commit of a navigation), the Radix `Presence` ref callback (chunk `6792`, reads the object only), and the `direction` of `<html>` in the toaster (chunk `4438`). Hypothesis, not verified on the live page: the first one is the reader that stops the page, because React commits the route sheet in the same task. `sheets_tests.rs` has a React-like fixture of that pattern.

## Proxy

Found on 2026-11-06. The staging bundles use `Proxy` for reactive state (a `get` trap that tracks and a `set` trap that triggers) and `Reflect.ownKeys` inside an `ownKeys` trap. On main 5978d55 (the same count on 8599baf), `tools/js/builtin_inventory.sh` reported 126 of 655 built-ins missing, and `Proxy` and `Proxy.revocable` were two of them. `Reflect` was already complete (batch 1). The task note named 145 missing on 17744a9. Main had moved since then; the merges between were not traced. After this change the count is 124 of 655.

Design (`crates/js/src/heap_proxy.rs`, `crates/js/src/eval_proxy.rs`; read the headers first). The heap cannot run script, and a trap is script. So the heap never runs a trap. A proxy is an exotic object with the flag `EXOTIC_PROXY` and a payload `(target, handler)`. Every heap operation that meets a proxy changes nothing and answers `HeapError::Proxy(p)`. The evaluator then runs the internal method of `p`: it reads the trap, calls it, checks the invariants against the target and calls the heap again for the target. There is no request record, because the answer is made in the evaluator and never has to go back through the heap. This differs from the host named-property hook (`heap_named.rs`), which stores a request. An earlier note in that file said a proxy would reuse the request slot. It did not need to.

| Internal method | Where the heap answers `Proxy(p)` | Evaluator method |
| --- | --- | --- |
| `[[GetPrototypeOf]]`, `[[SetPrototypeOf]]`, `[[IsExtensible]]`, `[[PreventExtensions]]` | the flag test at the top of each entry point | `px_get_prototype_of`, `px_set_prototype_of`, `px_is_extensible`, `px_prevent_extensions` |
| `[[GetOwnProperty]]` | `exotic_own`, behind the `exotic != 0` test that views already use | `px_get_own_property` |
| `[[DefineOwnProperty]]` | inside `exotic != 0` of `define_own_property` | `px_define_own_property` |
| `[[HasProperty]]`, `[[Get]]`, `[[Set]]`, `[[Delete]]` | the chain walks (`lookup`, `has_property`) call `exotic_own` for each object | `px_has`, `px_get`, `px_set`, `px_delete` |
| `[[OwnPropertyKeys]]` | `own_keys` | `px_own_keys` |
| `[[Call]]`, `[[Construct]]` | not in the heap: `Callee::Proxy { ctor }` in the function table, set when the target is callable | `px_call`, `px_construct` |

A proxy on a prototype chain is found by the chain walk. The evaluator then redoes the operation from the first object with the generic algorithm of the specification (`o_get_slow`, `o_set` with the receiver, `o_has_slow`). Call sites use `hx!(s, heap_op, fallback)`: the third argument runs when the heap answered `Proxy`. A call site with no fallback turns the answer into `Unsupported` (`herr`), never into a wrong value. Reads that sit on hot paths (`get_prop`, `put_prop`) keep the proxy arm in a cold function (`get_miss`, `set_miss`), so they stay as small as before.

What is built: `new Proxy` and `Proxy.revocable`; all 13 traps with the invariant checks of ECMA-262 10.5, in spec order; a trap that is `undefined` or `null` forwards to the target; a trap that is not callable is a `TypeError`; revocation (a `TypeError` on every internal method, `typeof` still answers, and a revoked function proxy is still `"function"`); callable and constructable proxies (`new.target` is the proxy, `bind`, `call`, `apply`, `Reflect.apply`, `Reflect.construct`, `class extends` a proxy); proxies as prototypes (`get`, `set` with the receiver, `has`, `for-in`, `super.x` and `super.x = v` through a proxy home prototype). Routed through the dispatch: `in`, `delete`, `instanceof`, `Array.isArray` (it sees through, and `Array.prototype` methods use the proxy as an array-like), `Object.keys`, `values`, `entries`, `assign`, `fromEntries`, `getOwnPropertyNames`, `getOwnPropertySymbols`, `getOwnPropertyDescriptor(s)`, `defineProperty`, `defineProperties`, `create`, `freeze`, `seal`, `isFrozen`, `isSealed`, `preventExtensions`, `getPrototypeOf`, `setPrototypeOf`, spread, destructuring, `JSON.stringify`, `__lookupGetter__`, and all of `Reflect`. A `for-in` over a proxy follows V8: `ownKeys`, then `getPrototypeOf`, then a `getOwnPropertyDescriptor` at each step (not all up front).

Caps. A proxy of a proxy, a trap that touches its own proxy and a handler that is a proxy all end as `Limit`, never as a native stack overflow. The walks use `descend` (traversal depth 512), the calls use the call depth (100) and each internal method charges one step. A 10,000-deep proxy chain gives `Limit(TraversalDepth)` on `get`, `set`, `has`, `delete`, `ownKeys`, `getPrototypeOf`, `setPrototypeOf`, `isExtensible`, `preventExtensions`, `defineProperty`, `getOwnPropertyDescriptor`, `JSON.stringify`, `for-in`, `freeze`, `Reflect.ownKeys` and spread. `Array.isArray` over a 10,000-deep proxy chain, and `instanceof` and `for-in` over a 10,000-deep prototype chain of proxies, are loops, so they finish with the right answer. A runaway trap ends as `Limit(Steps)` and `try` does not catch it. The key list of an `ownKeys` result has the cap of `array_like` (1,048,576).

Gates and gaps (no wrong answer). The receiver of a proxied `[[Set]]` that is a typed array or a host object follows the generic algorithm, not the special `[[Set]]` of that object, except that an invalid integer index ends the walk. `new Proxy(new Uint8Array(2), {}).length` is a `TypeError`, as in Node. `Number(new Proxy(new Number(5), {}))` is a `TypeError`, as in Node, and a proxy of a `String` object reads `length`, indexes and keys as in Node (18 wrapper cases in the corpus). A native that reads the heap with `h!` and meets a proxy answers `Unsupported: Proxy: this operation has no trap dispatch yet`. A probe of about 130 operations against Node found none left that is not also `Unsupported` for a plain object (`with`, `super` in an object literal) or wrong for a plain object too (`new Error(m, { cause })` ignores `cause`). The messages of the `TypeError`s differ from V8 in some places (for example a falsish `set` trap in strict mode). The tests compare names, not messages.

Validation:

- `tools/js/proxy_check.sh`: 68 cases against Node 22.18 (`tools/fixtures/js/proxy-cases.txt`), run again after a collection. 7 of them are the trap-order matrix: they log the traps of `get`, `set`, `has`, `delete`, `keys`, `values`, `entries`, `getOwnPropertyNames`, `getOwnPropertyDescriptors`, `assign` (source and target), spread, `JSON.stringify`, `for-in`, `getPrototypeOf`, `setPrototypeOf`, `isExtensible`, `preventExtensions`, `freeze`, `seal`, `isFrozen`, `isSealed`, `instanceof`, `Array.isArray`, `defineProperty`, `hasOwnProperty`, `propertyIsEnumerable`, every `Reflect` function, `set` with a receiver, an inherited `get`, `has` and `set`, `call`, `new`, `bind`, `Function.prototype.call`, `apply` and `typeof`, and the order matches Node. The others cover the invariant of each trap (accept and reject), revocation in every method and inside a trap, trap arguments, callable and constructable proxies, `new.target`, classes, `super`, `Array` methods on a proxied array, `JSON.stringify`, `for-in`, a handler that is a proxy, a reactive store and a validation proxy.
- 13 tests in `crates/js/src/eval_proxy_tests.rs`: the corpus again with a collection between the two reads; the 10,000-deep chain; the 10,000-deep prototype chain; 12 shapes of trap recursion; a runaway trap; a huge `ownKeys` answer; proxies, handlers and revokers across a collection; a revoked proxy gives its target and handler back; 1,000 dropped proxies leave no object; the builtin surface; the typed-array and wrapper gates; a checksum through a forwarding proxy.
- `cargo test --workspace --release` passes. 30 existing `tools/js/*_check.sh` scripts still match Node. `regexp_check.sh` prints timing and does not compare. `parse_check.sh` fails in Node with `ERR_MODULE_NOT_FOUND` on this machine. `browser_check.sh` and `adapter_check.sh` were not run. Two older tests asserted that `Proxy` was missing. They now assert `Atomics`.
- Interpreter cost against main c3127aa (its `crates/js` is the same as on 5978d55). Each binary was built in its own `CARGO_TARGET_DIR`; the example binaries differ by `md5sum`. `perf stat -e instructions:u`, the minimum of 3 runs, AMD Ryzen 9 5900HX, rustc 1.96.0. 62 rows: 47 at scale 10 (`eval_load` 8, `binary_load` 9, `arrobj_load` 15 and `numeric_load` 15 with n = 10,000, without `random`) and the 15 `arrobj_load` rows at scale 1. The goal is below +0.5% on each row, and all 62 meet it. The 47 rows at scale 10 run from -0.82% to +0.41% (mean +0.10%). All 62 run from -1.53% to +0.43% (mean +0.07%; 13 rows are below zero). The table is at the end of this section.
- Why the first build cost up to +1.15%. It was found with a single-step counter: a ptrace program that runs a binary without ASLR, steps each instruction and counts it by address. It matches `perf stat` within 400 instructions. The counts per function of the two binaries (`arrays`, `descriptors`, `repeat` at scale 1 and `numeric_load empty` at n = 1,000) then show where the extra instructions are. `perf record` was not usable: sample skid moved the counts between a function and its callers. `size_of::<HeapError>()` is 24 bytes and `size_of::<Result<Value, HeapError>>()` is 24 bytes on main and on this branch, so the new variant did not change a layout. The causes are all code generation, not new work:
  1. `ao_search` (`indexOf` and `includes`). On main the compiler split the loop on `includes`. After the change it did not, and the merged loop ran 46 instructions for each element instead of 36 (it also copied the `index_key` answer through the stack). This was 80% of the `arrays` cost. The fix is two loops in the source. `arrays` at scale 10 is now 138,385,629 against 139,096,994 on main (-0.51%).
  2. `define_own_property`. A third `exotic == ...` test (`NAMED`, `STR`, `PROXY`) made the compiler build a jump table that every plain `define` paid for (4 instructions). `EXOTIC_PROXY` is now the bit 8, tested with `&`. This was the cost of `descriptors` and of the `numeric_load` loops over global variables.
  3. The `o_*` dispatch methods (`o_peek`, `o_get_own_property`, `o_has_own`, `o_has_property`, `o_define_own_property`, `o_delete`, `o_own_keys`) were outlined and called. Each is now `inline(always)` with one heap call and a test. The loop for a host fill, a proxy and an error moved to `heap_miss` and `heap_miss_w` (`cold`).
  4. The collector (`collect`) asked the heap for the proxy payload of every object. It does so only after the first proxy is made (`px.ready`).
  The start-up of a realm costs about +14 k instructions (`repeat` at scale 1, 4.5 M instructions: +0.30%). It is the `Proxy` constructor and `revocable` (function objects, interned names and their share of the first collection). The trap names are still interned on the first `new Proxy`.
- Rows near the limit. `replaceall` (+0.43% at scale 1, +0.40% at scale 10) is +360 k instructions in `build_replacement` and +120 k in `search`. Neither function is changed by this PR. The hot loop has the same instructions with other registers and 4 more spills. `numeric_load` `empty`, `clz32`, `log`, `pow`, `sin` and `atan2` (+0.35% to +0.41%) are 2 to 5 instructions in each of `get_name`, `assign_name` and `machine`, again with different registers. To verify: a later change to unrelated code can move these rows by the same amount in either direction.
- Drift. `arrays` at scale 10 was 138,539,132 on 17744a9 and 139,097,968 on main 5978d55 (+0.40%). With the two loops of cause 1 it is 138,385,365 on this branch, so the split also recovers that drift. Whether main lost it by the same merge of the two loops was not traced; to verify.

  Instructions of the 62 rows (`perf stat -e instructions:u`, minimum of 3):

  | Row | main c3127aa | Proxy | Change |
  | --- | ---: | ---: | ---: |
  | eval_load loop | 12,632,641,617 | 12,672,656,081 | +0.32% |
  | eval_load calls | 8,392,861,276 | 8,402,885,410 | +0.12% |
  | eval_load fib | 293,286,535 | 292,850,320 | -0.15% |
  | eval_load closures | 1,216,097,406 | 1,217,221,806 | +0.09% |
  | eval_load objects | 4,126,022,534 | 4,128,967,180 | +0.07% |
  | eval_load webpack | 56,730,283 | 56,788,598 | +0.10% |
  | eval_load throws | 3,135,577,683 | 3,144,496,124 | +0.28% |
  | eval_load concat | 10,938,490 | 10,969,813 | +0.29% |
  | binary_load copy | 28,352,461 | 28,367,887 | +0.05% |
  | binary_load scan | 7,654,999,903 | 7,681,021,567 | +0.34% |
  | binary_load scanarr | 16,892,769,031 | 16,927,786,192 | +0.21% |
  | binary_load codec | 423,315,583 | 423,328,444 | +0.00% |
  | binary_load stream | 396,211,871 | 396,407,064 | +0.05% |
  | binary_load indexof | 493,147,333 | 493,354,622 | +0.04% |
  | binary_load f64from | 1,004,094,075 | 1,003,106,894 | -0.10% |
  | binary_load i32sort | 703,393,589 | 703,408,516 | +0.00% |
  | binary_load dataview | 15,341,483,159 | 15,383,497,503 | +0.27% |
  | arrobj_load mapfilter x1 | 34,187,545 | 33,960,534 | -0.66% |
  | arrobj_load sort x1 | 91,939,147 | 91,731,746 | -0.23% |
  | arrobj_load queue x1 | 24,449,851 | 24,494,065 | +0.18% |
  | arrobj_load objects x1 | 11,517,824 | 11,521,429 | +0.03% |
  | arrobj_load arrays x1 | 20,917,533 | 20,598,390 | -1.53% |
  | arrobj_load fill x1 | 44,551,437 | 44,660,269 | +0.24% |
  | arrobj_load reverse x1 | 388,428,095 | 389,322,982 | +0.23% |
  | arrobj_load flat x1 | 75,182,530 | 75,124,081 | -0.08% |
  | arrobj_load tosorted x1 | 83,628,708 | 83,703,382 | +0.09% |
  | arrobj_load lastindexof x1 | 413,800,790 | 414,028,408 | +0.06% |
  | arrobj_load pad x1 | 6,324,076 | 6,338,509 | +0.23% |
  | arrobj_load repeat x1 | 4,512,269 | 4,525,941 | +0.30% |
  | arrobj_load replaceall x1 | 38,942,300 | 39,110,670 | +0.43% |
  | arrobj_load freeze x1 | 206,427,517 | 205,886,384 | -0.26% |
  | arrobj_load descriptors x1 | 11,179,510 | 11,196,018 | +0.15% |
  | arrobj_load mapfilter x10 | 303,240,486 | 300,825,565 | -0.80% |
  | arrobj_load sort x10 | 1,021,044,512 | 1,018,796,699 | -0.22% |
  | arrobj_load queue x10 | 290,240,166 | 290,453,414 | +0.07% |
  | arrobj_load objects x10 | 75,740,659 | 75,118,420 | -0.82% |
  | arrobj_load arrays x10 | 139,096,994 | 138,385,629 | -0.51% |
  | arrobj_load fill x10 | 406,755,801 | 407,694,054 | +0.23% |
  | arrobj_load reverse x10 | 3,859,045,578 | 3,867,680,320 | +0.22% |
  | arrobj_load flat x10 | 712,573,316 | 711,833,889 | -0.10% |
  | arrobj_load tosorted x10 | 938,027,116 | 938,639,969 | +0.07% |
  | arrobj_load lastindexof x10 | 4,112,468,816 | 4,114,496,512 | +0.05% |
  | arrobj_load pad x10 | 24,567,188 | 24,582,331 | +0.06% |
  | arrobj_load repeat x10 | 6,538,528 | 6,552,435 | +0.21% |
  | arrobj_load replaceall x10 | 350,770,480 | 352,191,099 | +0.40% |
  | arrobj_load freeze x10 | 2,024,105,569 | 2,018,595,531 | -0.27% |
  | arrobj_load descriptors x10 | 72,139,190 | 72,155,087 | +0.02% |
  | numeric_load empty | 138,037,129 | 138,602,459 | +0.41% |
  | numeric_load clz32 | 157,063,709 | 157,655,319 | +0.38% |
  | numeric_load log | 159,854,931 | 160,445,722 | +0.37% |
  | numeric_load radix16 | 202,249,483 | 202,782,949 | +0.26% |
  | numeric_load radix36frac | 192,393,209 | 193,033,058 | +0.33% |
  | numeric_load radix2sub | 731,537,079 | 732,131,063 | +0.08% |
  | numeric_load pow | 156,648,642 | 157,200,381 | +0.35% |
  | numeric_load sin | 160,408,199 | 160,998,607 | +0.37% |
  | numeric_load atan2 | 162,726,723 | 163,292,696 | +0.35% |
  | numeric_load tofixed | 191,696,794 | 192,231,085 | +0.28% |
  | numeric_load toprecision | 191,499,494 | 192,050,263 | +0.29% |
  | numeric_load toexp | 253,692,368 | 254,496,745 | +0.32% |
  | numeric_load parsefloat | 254,073,574 | 254,480,660 | +0.16% |
  | numeric_load encodeuri | 272,809,393 | 273,214,361 | +0.15% |
  | numeric_load escape | 241,417,824 | 241,877,503 | +0.19% |
- Load test: `tools/js/proxy_load.sh`, 8 cases at 1x, 10x and 100x (1,000, 10,000 and 100,000 iterations), one process for each row under 1 GiB of address space. The checksum comes from Rust. All 24 rows pass.

| Case (what one iteration does) | 1x | 10x | 100x |
| --- | --- | --- | --- |
| `get_trap` (a `get` trap that computes) | 1.0 ms | 8.3 ms | 80 ms |
| `set_trap` (a `set` trap that writes the target) | 1.3 ms | 10.7 ms | 104 ms |
| `noop` (empty handler, `set` and `get`) | 1.1 ms | 9.7 ms | 95 ms |
| `forward` (`Reflect.get` and `Reflect.set` in the traps) | 1.9 ms | 16.6 ms | 163 ms |
| `array_mapfilter` (1,000 elements, `get` and `has` traps; `map` then `filter`) | 19.5 ms | 190 ms | 1,852 ms, 29 MiB |
| `reactive` (store with tracking and triggering, two keys, one effect) | 3.8 ms | 35 ms | 348 ms |
| `chain` (10 proxies, no traps) | 3.0 ms | 29.7 ms | 297 ms |
| `keys` (`Object.keys` with `ownKeys` and 50 `getOwnPropertyDescriptor` calls) | 6.2 ms | 59 ms | 588 ms, 177 MiB |

Time grows in a straight line. Peak memory is 4 to 7 MiB, except in two cases. `array_mapfilter` makes 2,000 result arrays. `keys` makes one descriptor object for each key and call: 500,000 objects at 100x. The engine collects only when the host calls `collect` (the load example calls it after each stage), so that garbage stays until then. This is the rule of the engine and not new. Cost of one operation, counted in instructions on a global-variable loop (`promise_check -e`, 200,000 iterations, the cost of the empty loop removed): a plain `p.v` read 823, the same read through a proxy with no traps 1,781, with a `get` trap that reads the target 4,048, and with `Reflect.get` in the trap 5,670. A plain `p.v = i` is 4,763 and with a `set` trap 8,685.

Boa compared (boa_engine 0.22.0, `builtins/proxy/mod.rs`, 1,248 lines). Boa gives every object a table of function pointers for the 13 internal methods (`InternalObjectMethods`: `BASIC`, `CALLABLE`, `CONSTRUCTOR`), and a proxy has its own table. bare-js has one flag in the object header and no table, and the evaluator does the dispatch. Boa limits the depth with its VM recursion limit. bare-js uses the limits it already had. No Boa code was copied, so `docs/open-source.md` has no new entry.

Not tested: a real page on this build (the live login was not run), `Proxy` over a host object (an element, `dataset`, a `NodeList`) and a proxy passed to a host function (`addEventListener`, `fetch` options), typed arrays and `DataView` behind a proxy beyond the cases above, `with` (it is `Unsupported` on a plain object too), and Node versions other than 22.18. Node runs authored cases only.

## BigInt

This topic adds the BigInt primitive to `bare-js`: literals, `typeof`, all operators, comparison, `BigInt()`, `BigInt.asIntN`, `BigInt.asUintN` and `BigInt.prototype`. It removes the BigInt gates of the typed arrays (`BigInt64Array`, `BigUint64Array`) and of `DataView` (`getBigInt64`, `setBigInt64`, `getBigUint64`, `setBigUint64`). `tools/js/builtin_inventory.sh` now prints 119 of 655 missing, down from 126 on main 862e424. The 7 BigInt items are the constructor, `asIntN`, `asUintN`, and `toString`, `valueOf`, `toLocaleString` and `@@toStringTag` of the prototype.

Design:

- `Value::BigInt(i64)` is the inline form. `Value::BigHeap(BigId)` is a heap entry for a value outside `i64`. A value has exactly one form.
- The heap keeps one entry for each value (like strings). `new_bigint` looks the value up in a hash table first. So two handles are equal exactly when the values are equal. `===`, `Object.is`, SameValueZero, `Map` keys and `Set` members need no heap and no limb comparison.
- Two `i64` operands never reach the limb code. `+ - * / %` run in `i128`, which is exact for them. Only a result outside `i64` makes a heap entry.
- The limb code is in `bigint.rs`: `u64` limbs, sign and magnitude. Multiplication switches to Karatsuba above 32 limbs. Division is Knuth D with a Möller–Granlund reciprocal for the quotient digit. `toString` is divide and conquer above 32 limbs. Bit operations use the two's complement form. `>>` rounds toward minus infinity. `to_f64` is correctly rounded. Comparison with a Number is exact (`cmp_f64`).
- A heap entry costs about 54 bytes at 2 limbs, measured (the slot, the entry, the limbs and the table share). Entries are traced and swept by the collector. The table is weak and is rebuilt after a sweep.
- `BigInt` is installed lazily like `Intl` (see the cost section). The wrapper object of a BigInt is the `Exotic::Prim` of the primitive wrapper change, with `BigInt.prototype`.
- A BigInt typed array keeps its elements in the byte buffer. A read gives `Value::BigInt`. A `BigUint64Array` element above `i64::MAX` needs a heap entry, and the read path has no `&mut`. So `view_own_big` returns `BigUnsigned(raw)`, and `get_own_property` and `lookup` make the entry. A write converts with ToBigInt and wraps to 64 bits. A path that calls `heap.set` or `heap.define_own_property` with a value that is not a BigInt gets `NeedsBigInt`. `put_prop`, `Reflect.set` and `Object.defineProperty` convert and repeat. Any other path is an `Unsupported` gate.
- Limits: a BigInt of more than `max_big_limbs` limbs is `Limit::BigLimbs`, a fatal limit (never a catchable error). The size of a result is checked before the operation allocates it, with an upper bound from the operand sizes (for a power: bits of the base times the exponent). So a result close to the cap can be refused though it would fit. An operation charges steps for its work (`work / 64`). `toString` checks the string cap first.
- Errors use the V8 messages: `Cannot mix BigInt and other types, use explicit conversions`, `Division by zero`, `BigInts have no unsigned right shift, use >> instead`, `Do not know how to serialize a BigInt`, `The number 1.5 cannot be converted to a BigInt because it is not an integer`, `Cannot convert abc to a BigInt`, and `toString() radix argument must be between 2 and 36`. A negative exponent gives `undefined must be positive`. That text is a quirk of V8 12.4 (Node 22) and it is matched.

### Crate decision

I measured num-bigint 0.4.8 against the own code. Both pass the same tests. The own code was written first, then compared on the same machine, release profile.

| Measure | Own | num-bigint 0.4.8 |
|---|---|---|
| Binary size over a 346,536 B baseline | +57,952 B | +160,504 B |
| 2-limb `+` and `*`, 1M each | 29–31 ms | 38–50 ms |
| 1k digits: `*`, `/`, `toString` (time ÷ num-bigint) | 1.02, 1.10, 1.00 | 1.0 |
| 10k digits: `*`, `/`, `toString` (time ÷ num-bigint) | 1.14, 1.19, 1.17 | 1.0 |
| 100k digits `*` | 3.14 ms | 2.26 ms (own is 1.39×) |
| 100k digits `/` | 6.17 ms | 2.16 ms (own is 2.85×) |
| 100k digits `toString` | 14.06 ms | 5.91 ms (own is 2.38×) |
| Parse, 1k and 100k digits (time ÷ num-bigint) | 0.66, 0.88 | 1.0 |
| Memory of a 2-limb value | about 54 B in the heap | `Vec<u64>` and a sign, with no shared entry |

Boa uses num-bigint for its `JsBigInt`. Its value is a reference-counted heap object for every BigInt, small or not. Here a small value is inline and costs no allocation, which matters more for a script than the large-number speed.

Decision: the own code. The reasons are 102,552 fewer bytes of binary, faster small values, and a `to_f64` that is correctly rounded (the `num-bigint` conversion is not; it differs from Node and Python in the last bit for some inputs). The cost is speed above about 10k digits: `/` and `toString` are 2.4× to 2.9× slower than num-bigint at 100k digits. That has no use in a page script today. Known gap: no Toom-3 and no Burnikel–Ziegler division. No crate was adopted, so `docs/open-source.md` has no change.

Differential check: a scratch program (not in the repository, because num-bigint is not a dependency) compared the own code with num-bigint over 200k to 300k random operations for each operation. All match, except the `to_f64` rounding above. `bigint_tests.rs` checks the code against plain models and against known values.

### Load test

`tools/js/bigint_load.sh` runs `bigint_load` (7 rows at scales 1, 10 and 100, one process each). The host checks each result against a formula or a Rust model that does not use the engine's BigInt code.

| Row | Scale 1 | Scale 100 |
|---|---|---|
| `small` (inline `*`, `%`, `+`, 150M steps at scale 100) | 60.2 ms | 5.93 s, peak 5 MiB |
| `heapmix` (64-bit LCG, values above `i64`) | 25.6 ms | 3.03 s, 5.0M entries, peak 474 MiB |
| `mul` (a product of 100k digits at scale 100) | 0.1 ms | 114 ms |
| `divmod` | 0.1 ms | 74 ms |
| `tostring` (7^k and back) | 0.0 ms | 24 ms |
| `i64sort` (50k × scale elements) | 66 ms | 6.17 s, peak 80 MiB |
| `u64sort` (values above `i64::MAX`) | 110 ms | 10.1 s, peak 502 MiB |

All 21 rows pass. A collect after the run frees every heap BigInt but the last one held (`bigints_live_after` is 0 or 1). The memory per live entry is 54 B (272,197,800 B ÷ 5,000,727 entries at `heapmix` scale 100). The sort rows spend their time in the comparator and the typed-array read (a `BigUint64Array` read above `i64::MAX` goes through the table lookup). I did not measure a Number-only baseline for `small`.

### Cost on the Number path

The brief says a row without BigInt must regress less than 0.5% in instructions. I measured instructions (`perf stat`, user mode, one core) of this build and of main 5978d55 on `eval_load`, `arrobj_load` and `binary_load` at scales 1, 10 and 100 (96 rows), and on `numeric_load` at 1,000 and 10,000 iterations (38 rows). The first version of this change missed the limit on 17 rows.

Result: no row is over +0.5%. The worst is `arrobj_load` `tosorted` at scale 100 (+0.42%), then `repeat` at scale 1 (+0.32%), `pad` at scale 1 (+0.24%) and `copy` at scale 1 (+0.22%). 112 of 134 rows have fewer instructions than main. The largest falls are `arrays` (−7% to −8%), `lastindexof` (−3.8%), `loop` (−3.5%) and `objects` (−2.8%). The table is at the end of this section.

Causes of the first regression, and the fix for each. I found them with `perf record`, `perf annotate`, `objdump`, `nm` and `readelf -r`, and a per-realm instruction probe:

- A realm built `BigInt` and its prototype at setup: +36k instructions for each realm. `BigInt` is now installed like `Intl`. The realm gets a global accessor, and the getter builds the constructor and the prototype on the first read, then replaces the accessor with a data property. A realm that never reads `BigInt` pays +5k. The code that needs the prototype (`get_prop` on a BigInt, `Object.getPrototypeOf`, `ToObject`, the sloppy `this`) calls `big_proto`, which builds it once.
- The collector function `Marker::value` was no longer inlined once it handled `BigHeap`: +27k instructions in each `collect`. The `BigHeap` arm is now a `#[cold]` function and `Marker::value` inlines again.
- `bigint.rs` called `log2` and `pow`. Each is a versioned libm symbol that the dynamic loader resolves at every process start, about 33k instructions each. That was +66k on every run, and it was the whole gap of the rows that run under 10M instructions (`pad`, `repeat`, `copy`, `concat`). A bound on the digits of a result now comes from the exponent bits of the `f64` (`pow_estimate`, `digits_bound`), with no libm call.
- `get_prop` carried the BigInt path inline. The BigInt receiver is now a `#[cold]` function (`get_prop_big`), and `get_prop` ends in one `lookup`. `#[inline]` is back on the small helpers of `Value` and the typed arrays (`is_bigint`, `strict_equals`, `same_value`, `same_value_zero`, `ta_conv`, `ta_write`, `ta_bytes`) that the first version had left to the compiler.

I did not change the order of the tests in the operators. They test `Value::Number` first and go to the BigInt code only after that check fails.

Cycles (minimum of 10 runs, this build and main interleaved in each round, `taskset -c 3`, on a machine with a load of 7 to 9): the gap is real and is not fixed. At scale 1 the mean is +1.5% and 18 of 32 rows are over +0.5%. At scale 10 the mean is +2.1% and 24 of 32 rows are over +0.5%. The largest are `arrays` (+5.4% at scale 1, +31% at scale 10), `objects` at scale 1 (+14.6%), `dataview` (+8.0%), `scanarr` (+5.8% to +7.0%) and `flat` at scale 10 (+6.8%). `mapfilter` at scale 10 is −25% and `fill` at scale 10 is −11%, so a row can move either way. Rows with fewer instructions have more cycles (`arrays`: −8% instructions and +31% cycles). A scale 100 run of `arrays` gave +33% to +40%, from 3 runs. I did not find the cause. Hypothesis: stalls from store-to-load forwarding. The 16-byte `Value` is written in two parts and read back with one wide load on the path through `lookup`, `get_prop` and `ao_search`, and the code layout of the build decides if that happens. Instruction counts cannot show it. I tried a cold `big_equals`, `#[repr(C, u8)]` on `Value` and a split of the `ao_search` loop. None of them changed the cycles, and I reverted them. To verify: a quiet machine, `perf stat` with the back-end stall counters, and `perf annotate` of `ao_search` on both builds.

Percent of instructions and cycles against main 5978d55 (`+` is more in this build). Instruction columns: scales 1, 10 and 100. Cycle columns: scales 1 and 10.

| Row | Instr. 1 | Instr. 10 | Instr. 100 | Cycles 1 | Cycles 10 |
|---|---|---|---|---|---|
| `eval_load` `loop` | -3.47% | -3.45% | -3.44% | +2.4% | +2.1% |
| `eval_load` `calls` | -2.61% | -2.62% | -2.62% | +1.9% | +2.1% |
| `eval_load` `fib` | +0.02% | -0.03% | -0.03% | +1.9% | +2.7% |
| `eval_load` `closures` | -1.66% | -1.73% | -1.73% | +1.6% | +3.2% |
| `eval_load` `objects` | -2.75% | -2.81% | -2.81% | +3.1% | +3.5% |
| `eval_load` `webpack` | -0.61% | -1.12% | -1.31% | +2.8% | +2.9% |
| `eval_load` `throws` | -2.18% | -2.10% | -2.11% | +0.0% | +1.0% |
| `eval_load` `concat` | +0.12% | -0.53% | -0.41% | +1.1% | -2.5% |
| `arrobj_load` `mapfilter` | -2.93% | -3.35% | -3.39% | -4.2% | -25.0% |
| `arrobj_load` `sort` | -0.76% | -0.36% | -0.01% | -2.5% | +4.4% |
| `arrobj_load` `queue` | -2.28% | -1.98% | -0.51% | +0.2% | +1.8% |
| `arrobj_load` `objects` | -0.75% | -2.07% | -1.53% | +14.6% | +4.3% |
| `arrobj_load` `arrays` | -7.10% | -8.18% | -8.35% | +5.4% | +31.1% |
| `arrobj_load` `fill` | -2.67% | -2.97% | -3.00% | +0.1% | -11.2% |
| `arrobj_load` `reverse` | -3.05% | -3.08% | -3.08% | -1.2% | +5.6% |
| `arrobj_load` `flat` | -1.74% | -1.85% | -1.86% | -2.3% | +6.8% |
| `arrobj_load` `tosorted` | -0.29% | +0.10% | +0.42% | +0.8% | +2.2% |
| `arrobj_load` `lastindexof` | -3.81% | -3.84% | -3.83% | +1.8% | +1.4% |
| `arrobj_load` `pad` | +0.24% | +0.06% | +0.01% | -3.2% | +0.8% |
| `arrobj_load` `repeat` | +0.32% | +0.21% | +0.05% | -0.6% | +2.0% |
| `arrobj_load` `replaceall` | -1.28% | -1.48% | -1.50% | -1.7% | -2.1% |
| `arrobj_load` `freeze` | -2.49% | -2.55% | -2.53% | +1.6% | +3.7% |
| `arrobj_load` `descriptors` | -0.51% | -1.08% | -1.16% | -0.6% | +2.8% |
| `binary_load` `copy` | +0.22% | +0.05% | +0.01% | -0.3% | -0.5% |
| `binary_load` `scan` | -0.44% | -0.44% | -0.44% | +4.9% | +4.6% |
| `binary_load` `scanarr` | -1.00% | -1.01% | -1.01% | +5.8% | +7.0% |
| `binary_load` `codec` | +0.03% | +0.00% | +0.00% | +4.9% | +5.9% |
| `binary_load` `stream` | -0.10% | -0.15% | -0.15% | +0.9% | +1.2% |
| `binary_load` `indexof` | +0.03% | +0.01% | +0.01% | +0.7% | +0.2% |
| `binary_load` `f64from` | -1.41% | -1.47% | -1.48% | -0.9% | -1.1% |
| `binary_load` `i32sort` | +0.02% | +0.00% | +0.00% | -0.5% | -0.8% |
| `binary_load` `dataview` | -0.17% | -0.18% | -0.18% | +8.0% | +7.3% |

| Row (`numeric_load`) | 1,000 | 10,000 |
|---|---|---|
| `atan2` | -0.69% | -1.06% |
| `clz32` | -0.73% | -1.16% |
| `empty` | -0.64% | -1.13% |
| `encodeuri` | -0.30% | -0.38% |
| `escape` | -0.38% | -0.53% |
| `isnan` | -1.38% | -1.93% |
| `log` | -0.68% | -1.09% |
| `parsefloat` | -0.86% | -1.15% |
| `parseint` | -0.70% | -0.99% |
| `pow` | -2.23% | -3.46% |
| `radix16` | -0.58% | -0.82% |
| `radix2sub` | -0.15% | -0.18% |
| `radix36frac` | -1.12% | -1.77% |
| `random` | -0.84% | -1.30% |
| `sin` | -0.67% | -1.09% |
| `toexp` | -1.01% | -1.27% |
| `tofixed` | -1.30% | -1.85% |
| `toprecision` | -0.59% | -0.88% |
| `uricomponent` | -0.36% | -0.52% |

### Validation

- 58 Node cases (`tools/fixtures/js/bigint-cases.txt`, `tools/js/bigint_check.sh`) match Node 22.18. They cover literals in 4 bases with separators, every operator and the error messages, comparison and equality with Numbers and Strings, `BigInt()`, `asIntN`, `asUintN`, `toString` in every radix, Map, Set, JSON, templates, property keys, the typed arrays and `DataView`, the wrapper object (`Object(1n)`, ToObject, the sloppy `this`) and `toLocaleString` with locales and options. 8 matrix cases (`arith_matrix`, `div_matrix`, `radix_matrix`, `parse_matrix`, `number_matrix`, `shift_matrix`, `pow_matrix`, `bigint_typed_matrix`) give a count and a hash of many random results for every operator and radix, so a difference in one value shows.
- 74 parse cases of BigInt literals match Acorn (`tools/js/parse_check.sh`, 2,207 cases in all).
- 20 Rust tests (`bigint_tests.rs` and `eval_bigint_tests.rs`): arithmetic against plain models, the corpus, one entry for equal values, collect with survivors, a held result, the limb cap before allocation, the string cap, steps, the wrapper object and the `toLocaleString` gate. One test of the console (`a_bigint_prints_with_an_n`) checks `console.log` and `%d` of a BigInt.
- `cargo test --workspace --release` passes. The 3 old tests that expected a BigInt gate (`gates_are_explicit`, `unsupported_is_found_before_anything_runs`, `unsupported_class_shapes_are_found_before_anything_runs`) now expect the new behavior. The other `*_check.sh` tools (all 35 in `tools/js`, with `TMPDIR` on a disk that has room; `parse_check.sh` needs `ACORN`) still match Node. `tools/js/bigint_load.sh` passes all 21 rows again after the rebase.

### Gates

Each is `Unsupported`, never a wrong result:

- `BigInt.prototype.toLocaleString`, `Intl.NumberFormat.prototype.format` and `formatToParts` of a value of more than 32,767 digits. They pass the decimal digits of the BigInt to ICU4X as a string, so the result is exact. The ICU4X decimal has a 16-bit magnitude, so it cannot hold more digits. Any option or locale that `Intl.NumberFormat` does not support is also a gate for a BigInt (for example `notation`, `style: "unit"` and `roundingIncrement`; `de-DE` and `fr-FR` use the `en` data).
- A BigInt in the state of `history.pushState` (structured clone): the browser layer stops with a gate.
- A value that is not a BigInt written to a BigInt typed-array element by a path that calls `heap.set` or `heap.define_own_property` directly.

Gates lifted in this change: `Object(1n)`, `new Object(1n)`, ToObject of a BigInt and the sloppy `this` of a BigInt now make an `Exotic::Prim` wrapper. `BigInt.prototype` methods accept a BigInt or a wrapper of one as `this`. `console.log(1n)` prints `1n`, and `%d` and `%i` with a BigInt print `1n`. `toLocaleString` with arguments or with more than 3 digits runs.

Not tested: `structuredClone` of a BigInt, a locale other than `en` for a BigInt, cycles on a quiet machine, and the live staging login.

Problems noticed, not changed:

- `Object.getOwnPropertyDescriptor(globalThis, 'BigInt')` is an accessor until the first read of `BigInt`. Node gives a data property.
- A frozen wrapper object gives `#<Object> is not extensible` from `Object.setPrototypeOf`. V8 gives `[object BigInt]` for a BigInt wrapper. This is the wrapper message of the earlier change and I did not change it.
- `Intl.NumberFormat` gaps seen with a BigInt: `notation`, `style: "unit"`, `roundingIncrement`, locales other than `en`, and the text of the invalid-locale `RangeError` (`Incorrect locale information provided`; V8 says `Invalid language tag: ...` from `toLocaleString`).
- The `WeakSet` error message has a `: 1` suffix that V8 does not have.
- `ta_sort_in_place` calls `coerce_number` twice for each element.
- `/` and `toString` of very large values are slower than num-bigint (see the table).

### BigInt after Proxy

The BigInt change was rebased onto main f0ea006, which added `Proxy`. Both changes edited the write paths of the evaluator.

- `set_miss`, the slow side of `put_prop` and of `o_set_value`, now also answers `NeedsBigInt`: it converts the value with ToBigInt and writes again. The old `heap_set` is gone.
- `o_define_own_property` sends `NeedsBigInt` to `define_big`, which converts the `value` and defines again. The old `heap_define` is gone.
- `o_set` converts a view element with `ta_conv`, so a BigInt view gets ToBigInt there too.
- `get_prop_big` uses `get_miss`, as `get_prop` does.

Checks: 8 cases in `tools/fixtures/js/proxy-cases.txt` (`bigview_1` to `bigview_8`) write to `BigInt64Array` and `BigUint64Array` through `Object.defineProperty`, `Reflect.set`, a proxy and a proxy on the prototype chain. All 76 proxy cases and all 58 BigInt cases match Node. 2,288 workspace tests pass. `tools/js/builtin_inventory.sh` prints 117 of 655 missing.

Cost against main f0ea006. `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR`. 40 rows at scale 10: `eval_load` 8, `arrobj_load` 15, `binary_load` 9 and `proxy_load` 8. All 40 are below +0.5%. They run from -8.34% (`arrobj_load arrays`) to +0.37% (`proxy_load chain`).

The first rebased build missed the limit on two rows. Both causes were code generation:

1. `proxy_load chain` (+1.22%). The first merge sent every error of `o_define_own_property` to a new cold function, which then called `heap_miss_w`. Each proxy level of the define path took one more call. `NeedsBigInt` now has its own arm, and the other errors go to `heap_miss_w` as on main. The row fell to +0.37%.
2. `arrobj_load arrays` (+1.94%). A `perf record` profile showed `index_key` as a function of its own, where main inlines it into the loops of `indexOf`, `includes`, `slice` and `concat`. `index_key` is now `#[inline(always)]`, and its string path (`index_key_str`) is cold. The row fell to -8.34%.

Not tested: cycles, rows at scale 1 and 100, `numeric_load`, and the live staging login.

## Browser JavaScript APIs

This topic adds five APIs that the staging app uses: `AbortController` and `AbortSignal`, `structuredClone`, `btoa` and `atob`, `Intl.Segmenter`, and `String.prototype.matchAll`. The ports follow Boa for the shape of the classes (see [open-source.md](open-source.md#ported-from-boa-for-the-browser-javascript-apis-2026-11-06)). The specifications decide the behavior. Each part has its own case file, and `tools/js/web_apis_load.sh` runs the load matrix for all five.

### AbortController and AbortSignal

What runs: `new AbortController()`, `controller.signal`, `controller.abort(reason)`, `signal.aborted`, `signal.reason`, `signal.throwIfAborted()`, `signal.onabort`, `AbortSignal.abort(reason)`, `AbortSignal.timeout(ms)` and `AbortSignal.any(signals)`. `{signal}` works for `addEventListener` (abort removes the listener) and for `fetch`. A rejected `fetch` carries the reason. A `Request` follows `init.signal`. The default reason is a `DOMException` named `AbortError`, and a timeout gives `TimeoutError`.

Design: a signal is an object in the table of event targets, so listeners, `once`, `capture` and `dispatchEvent` use the one event code. The state of a signal is in `abort.s.sigs`. The abort algorithms are small values (`Remove(target, id)` and `Fetch(promise)`), not closures, so the collector can trace them. `AbortSignal.timeout` uses the scheduler (`Task::AbortTimeout`), so a virtual clock moves it. The collector drops the state of a dead signal. Both globals are lazy accessors, like `Intl`, so a page that never reads them pays nothing.

Gates: `structuredClone` of a signal is `DataCloneError`. `AbortSignal.any` of 100,000 signals ends as a `Limit`.

Checks: 91 cases in `tools/fixtures/js/dom-cases.txt` match headless Chrome (`tools/js/dom_check.sh`, recorder `tools/ui/chrome_cases.mjs`). 36 are structured clone cases, 8 abort, 7 `any`, 7 static, 10 listener, 2 `throwIfAborted`, 2 `onabort`, 9 `atob` and `btoa`, and a few shape cases. Two differences from Chrome are in the fixtures on purpose: Chrome removes a listener that was added again during an abort (bare follows the DOM Standard and keeps it), and the enumerability of some DOM methods differs. 8 unit tests are in `browser_abort_tests.rs`.

### structuredClone

What runs: plain objects, arrays, `Date`, `RegExp` (`lastIndex` is 0 in the clone), `Map`, `Set`, the `Error` types (`name`, `message` and `stack` as a string; `cause` as an own property), `Boolean`, `Number`, `String` and `BigInt` wrappers, BigInt (inline and heap), `ArrayBuffer`, typed arrays, `DataView`, and cycles and shared references. `history.pushState` and `replaceState` use the same code, so there is one copy.

Design: two phases in `browser_navigation_state.rs`. The first phase walks the value and builds a tree of `SKind` nodes in Rust, with a memo by `ObjId` for cycles and sharing. It runs no user code except getters, which the engine runs as it reads a property. The second phase builds the objects. A view is built after its buffer. A failure in phase one leaves nothing in the heap. The bounds are 64 MiB, depth 256 and 2^20 nodes for a clone (history keeps its own `NavLimits`). The engine has no Error brand, so an object with `Error.prototype` in its chain is cloned as an Error.

Gates: a transfer list answers `Unsupported("structuredClone with a transfer list")`. A function, a symbol, a DOM node, a proxy, a signal and an `Intl.Segmenter` throw `DataCloneError`. `MessagePort.postMessage` of an object is still `Unsupported` (it does not call this code yet). bare lacks `cause` in the Error constructor, so the cases set it with `defineProperty`.

Checks: 36 cases (`sc_*`) match Chrome. The history tests changed: byte views and Maps were gates before, and now they clone.

### btoa and atob

`btoa` throws `InvalidCharacterError` for a code unit above U+00FF. `atob` follows the HTML forgiving-base64 decode: it removes ASCII whitespace, accepts a missing padding, and throws for a bad length or character. Both charge steps by the length of the input. 9 cases match Chrome. The load case runs 1 to 300 bytes per round and the native code takes about 2% of the time. The rest is the engine loop that builds the test string (`Heap::intern` is 14%), so there is nothing to optimize in `btoa` or `atob`.

### Intl.Segmenter

What runs: `new Intl.Segmenter(locales, {granularity})` with grapheme (default), word and sentence. `segment(string)` returns a `%Segments%` object with `containing(index)` and `[Symbol.iterator]`. The iterator has `next` and the tag `Segmenter String Iterator`. A segment has `segment`, `index`, `input` and, for words, `isWordLike`. `resolvedOptions` gives `locale` and `granularity`. `supportedLocalesOf` works as for the other `Intl` classes. The constructor needs `new`.

Binary size: the stripped `bare` is 7,875,320 B without it and 7,963,096 B with it, so +87,776 B (+1.1%). We keep it. The decision rule from the brief was "skip if too large". `icu_segmenter` uses rule data only: `WordSegmenter::new_for_non_complex_scripts`, with no LSTM and no dictionary data.

Design: the break list of a string is made once by the first `containing` or iteration, and it is shared by every iterator of that `%Segments%` object (`Rc<Brk>`). It costs steps by length. The collector prunes `intl.segs`, `intl.segments` and `intl.seg_iters`. A segmenter counts in `max_formatters`. A `%Segments%` object and an iterator count in `max_iterators`.

Gates: words of Chinese, Japanese, Thai, Lao, Khmer and Burmese are cut by the rules only (Node uses a dictionary for them). The locale does not change the rules, since there is no tailoring in the five English locales.

Checks: 26 cases in `tools/fixtures/js/segmenter-cases.txt` match Node (`tools/js/segmenter_check.sh`): emoji and combining marks, `\r\n`, regional indicators, apostrophes, `3.14` and `1,000`, sentences, `containing` out of range and with `NaN`, receivers, errors and the shape of the prototypes. 2 unit tests cover collection, the caps and a 15,000-unit string. A bug that the cases found: the word iterator of ICU4X yields the break at 0, which shifted `isWordLike` by one.

### String.prototype.matchAll

What runs: `matchAll(regexp)` for a `RegExp` with the `g` flag, `RegExp.prototype[Symbol.matchAll]`, a string argument (made into a `g` regexp), and the RegExp String Iterator (`next`, the tag `RegExp String Iterator`). A `RegExp` without `g` throws `TypeError`. `lastIndex` of the source is copied and not changed. Empty matches advance by one code point with the `u` flag and one unit without it. The iterator is `IterState::ReStr`, with a lazy prototype that is made on first use.

Gate: a regexp whose `exec` was replaced answers `Unsupported("RegExp with a replaced exec method")`. Node runs the replaced `exec`, so this case is not in the Node corpus.

Checks: 27 cases in `tools/fixtures/js/matchall-cases.txt` match Node (`tools/js/matchall_check.sh`). Unit tests are in `eval_batch1_tests.rs`, `eval_builtins_tests.rs` and `eval_interop_tests.rs`. The built-in inventory goes from 117 to 110 of 655 missing.

### Load test of the browser JavaScript APIs

`tools/js/web_apis_load.sh` runs 5 cases at 1x, 10x and 100x (1,000 rounds per scale unit), each in its own process under a 45 s and 1 GiB cap. Each case checks its sum against Rust and checks that the live object count is back to the baseline after the last collect. All 15 rows pass.

| Case | 1x run | 10x run | 100x run | 100x peak RSS |
| --- | ---: | ---: | ---: | ---: |
| abort | 17.5 ms | 175.4 ms | 1,988 ms | 9.5 MiB |
| clone | 30.8 ms | 291.7 ms | 3,011 ms | 10.4 MiB |
| base64 | 101.5 ms | 1,035.9 ms | 10,027 ms | 16.7 MiB |
| segmenter | 21.0 ms | 217.7 ms | 2,367 ms | 10.1 MiB |
| matchall | 15.8 ms | 156.8 ms | 1,780 ms | 9.8 MiB |

Memory stays flat: the peak RSS grows by about 1 MiB from 1x to 100x for every case. The collector freed 1.4 M objects in `abort` at 100x and the live count at the end equals the start. Per round (100,000 rounds at 100x): abort 20 µs, clone 30 µs, segmenter 24 µs, matchall 18 µs, base64 100 µs (the loop that builds the string, see above).

### Cost of the browser JavaScript APIs

Method: `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR`: main 9c5358f against this build. 41 rows: `eval_load` 8, `arrobj_load` 15, `binary_load` 9 and `proxy_load` 8 at scale 10, and the startup of an empty realm (`eval_check -e 0`). All 41 are below +0.5%. The worst is startup at +0.41% (2,967,274 to 2,979,473 instructions, 12 k more). The worst loop row is `arrobj_load repeat` at +0.25%. The best is `eval_load throws` at -0.24%. The other rows are between -0.01% and +0.15%.

The new globals are lazy where the spec allows: `AbortController`, `AbortSignal` and `Intl.Segmenter` cost nothing until they are read. `structuredClone`, `btoa` and `atob` are native functions that the install makes. Hypothesis: they are most of the +12 k at startup. Not measured by itself. `IterState::ReStr` adds a variant and no hot-path branch.

Not tested: cycles, rows at scale 1 and 100, `numeric_load`, the dictionary words of CJK and Thai, `MessagePort.postMessage` with a clone, `structuredClone` with a transfer list, `fetch` against a real network with `{signal}`, a build on another machine, and the live staging login.

### crypto, navigator, Blob, File and FormData

The staging chunks use `crypto.randomUUID`, `crypto.getRandomValues`, `FormData` and `navigator`. All four did not exist. They are in `crates/bare/src/browser_web.rs`.

Design:

- The five globals (`crypto`, `navigator`, `Blob`, `File`, `FormData`) are accessors until a script first reads or writes one, as `Intl` and `BigInt` are in the engine. The first read makes all objects of the file once and replaces its own accessor with a data property. A page that uses none of them pays for five functions.
- The host cannot run a JavaScript prelude when a global is read: `run_script` refuses to run inside a call, and the engine has no `eval`. So the objects are native Rust, in side tables, with the same GC hooks as `URLSearchParams` (`roots`, `trace`, `prune`).
- `crypto.getRandomValues` and `randomUUID` read `bare_tls::random::fill`. There is no other source. If it fails the call throws `OperationError`. `bare-tls` was a dev-dependency of `bare`. It is now a normal dependency. `bare-net` already links it, so no crate and no code is added to the binary.
- `Blob` keeps a copy of its bytes (at most 64 MiB each, 256 MiB for all live Blobs, 65,536 objects). `File extends Blob`. `FormData` stores strings and Files. `append(name, blob)` stores a File named `blob`, as the XHR specification says.
- `navigator` values are getters on `Navigator.prototype`. The user agent string is the constant `USER_AGENT`.
- `fetch` with a Blob or a `FormData` body is a gate (`Unsupported`). It never sends `[object FormData]`. A multipart writer is a follow-up. The old gate text of `Response.blob()` still says "no Blob or FormData". It is the same gate for the other direction.

Checks: 7 tests in `browser_web_tests.rs`. 3 of them run 73 scripts and compare with Node 22.18 (crypto 17, Blob and File 25, FormData 31; the tables hold the Node results). Node differs from the specification on `slice(Infinity)` and `lastModified: 1.9`. The tests follow the specification there, in `spec_cases_that_node_gets_wrong`. 4 tests check `navigator`, the lazy accessors, the specification cases, the caps and the GC. One test in `eval_wrapper_tests.rs` checks `String.prototype.length`, `Function.prototype.length` and `Function.prototype.name` against Node. It failed on the old engine first.

Engine fix: `String.prototype.length` is 0 (not writable, not enumerable, not configurable, as for every String object). `Function.prototype.length` is 0 and `Function.prototype.name` is the empty string (not writable, not enumerable, configurable). An anonymous class now has the name `""` from the prototype.

Cost against main 61a3962. `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. 40 rows at scale 10 and the startup of an empty realm. All are below +0.5%. The worst is `arrobj_load repeat` at +0.153% (6,591,749 to 6,601,858 instructions, a fixed 10 k). Startup is +5,278 instructions (+0.177%). The host rows `browser_load lookup`, `mutation` and `event` at scale 1 (minimum of 5 runs) are -0.19%, +0.12% and +0.20%. The first version of the host hooks measured +0.5% to +0.9% on those rows. `Host::trace` runs for every object of a collect, and a RefCell borrow and two map lookups for each object were 0.8 M instructions. `Dom::trace` now reads one `Cell<bool>` first and skips the call until a `FormData` exists.

| Row | main 61a3962 | this change | Change |
| --- | --- | --- | --- |
| eval_load loop | 12,194,696,954 | 12,194,702,428 | +0.000% |
| eval_load calls | 8,169,917,182 | 8,169,924,290 | +0.000% |
| eval_load fib | 293,067,049 | 293,074,868 | +0.003% |
| eval_load closures | 1,198,027,595 | 1,198,034,188 | +0.001% |
| eval_load objects | 4,013,608,117 | 4,013,616,713 | +0.000% |
| eval_load webpack | 56,494,889 | 56,507,376 | +0.022% |
| eval_load throws | 3,064,700,904 | 3,064,707,739 | +0.000% |
| eval_load concat | 10,920,556 | 10,926,960 | +0.059% |
| arrobj_load mapfilter | 292,896,008 | 292,906,075 | +0.003% |
| arrobj_load sort | 1,002,664,090 | 1,002,667,796 | +0.000% |
| arrobj_load queue | 285,786,698 | 285,790,707 | +0.001% |
| arrobj_load objects | 74,637,118 | 74,646,236 | +0.012% |
| arrobj_load arrays | 124,811,842 | 124,820,209 | +0.007% |
| arrobj_load fill | 393,988,342 | 393,996,747 | +0.002% |
| arrobj_load reverse | 3,756,010,661 | 3,756,019,210 | +0.000% |
| arrobj_load flat | 703,315,374 | 703,324,264 | +0.001% |
| arrobj_load tosorted | 926,419,798 | 926,426,613 | +0.001% |
| arrobj_load lastindexof | 3,965,774,647 | 3,965,783,924 | +0.000% |
| arrobj_load pad | 24,622,625 | 24,631,993 | +0.038% |
| arrobj_load repeat | 6,591,749 | 6,601,858 | +0.153% |
| arrobj_load replaceall | 345,611,453 | 345,622,359 | +0.003% |
| arrobj_load freeze | 1,969,378,590 | 1,969,388,406 | +0.000% |
| arrobj_load descriptors | 71,629,361 | 71,640,901 | +0.016% |
| binary_load copy | 28,396,282 | 28,401,571 | +0.019% |
| binary_load scan | 7,675,055,524 | 7,675,060,504 | +0.000% |
| binary_load scanarr | 16,855,308,013 | 16,855,312,316 | +0.000% |
| binary_load codec | 423,361,094 | 423,367,558 | +0.002% |
| binary_load stream | 396,610,656 | 396,616,973 | +0.002% |
| binary_load indexof | 493,368,571 | 493,374,149 | +0.001% |
| binary_load f64from | 989,339,538 | 989,345,371 | +0.001% |
| binary_load i32sort | 703,446,976 | 703,453,688 | +0.001% |
| binary_load dataview | 15,426,526,607 | 15,426,533,821 | +0.000% |
| proxy_load get_trap | 92,687,157 | 92,693,403 | +0.007% |
| proxy_load set_trap | 125,548,026 | 125,552,387 | +0.003% |
| proxy_load noop | 116,661,765 | 116,668,277 | +0.006% |
| proxy_load forward | 173,751,742 | 173,638,732 | -0.065% |
| proxy_load array_mapfilter | 1,964,996,513 | 1,965,008,166 | +0.001% |
| proxy_load reactive | 369,776,866 | 369,663,705 | -0.031% |
| proxy_load chain | 362,992,807 | 362,998,908 | +0.002% |
| proxy_load keys | 627,724,329 | 627,738,125 | +0.002% |
| startup (`eval_check -e 0`) | 2,979,037 | 2,984,315 | +0.177% |

Not tested: cycles, rows at scale 1 and 100 of the engine matrix, a real `fetch` with a multipart body (not written), `new FormData(form)` (a gate), `Blob.stream()` and `bytes()` (absent), `userAgentData.getHighEntropyValues` (absent), the OS random source failing (no hook to make it fail), a build on another machine, and the live staging login.

## Computed style inside display:none

Problem: the live login stopped with `Unsupported("an element inside display:none has no computed style in the engine")`. The cascade skips a `display:none` subtree, so `Styles::style` was `None` there. Chrome 155 gives the computed values (`tools/ui/chrome_hidden_style.mjs`, log in `tools/fixtures/ui/chrome-hidden-styles.txt`, 94 cases).

What runs: `RuleSet::compute_for(doc, Some(el))` styles the nodes from the root down to `el` (the flat ancestors), and nothing else, inside a hidden subtree. The cost of a hidden read is the cost of the walk plus the depth of `el`. `Dom::snap` in `browser_computed.rs` first runs the normal walk. Only when `el` has no style does it run the second walk with `Some(el)`. A shown element never pays for the second walk. `Snap.hidden` says that the element, or an ancestor, is `display:none`. For such an element `width`, `height` and `margin-*` are computed values, as in Chrome. The content of a `<template>` reads as empty (length 0), as in Chrome. A light child of a shadow host still has no style (no slots), and is still a gate.

Chrome cases: the fixture covers an element that is hidden itself against one whose ancestor is hidden, nested `display:none`, a chain 40 levels deep, `display:contents`, the `hidden` attribute, `<details>`, `content-visibility:hidden`, `<select>`, an SVG subtree, an iframe document, `<template>` content, inherited `color` and `font-size`, `var()` and custom properties, `em`, `rem` and percent values, and a read after the style changed.

Cost: `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. Main 61a3962 (with the new `style_load` cases copied in) against this build. 37 rows: `style_load` (setget, churn, attrs, shorthand, hidden, shown), `sheets_load` and `loader_load` at 1x, 10x and 100x, and `scroll_load 10000 2000`. All are within +0.5%. The worst is `loader_load dynamic 100` at +0.31%. `style_load shown` is -0.01%, -0.00% and -0.00% at 1x, 10x and 100x. `hidden` has no main row: main stops at the first read.

The first build cost `shown` +0.83% (1x), +1.84% (10x). The causes, in order of effect:
1. `compute` and `compute_for` both called the walk. A function with two callers is not inlined, and so `candidates` and `style_of` stopped being inlined into the loop. Fix: the walk is `walk<const WANT: bool>`, `compute` is `walk::<false>`, and `candidates` and `style_of` are `#[inline(always)]`. The walk for layout has none of the `want` code.
2. `Document::flat_parent` was no longer inlined into the loop (a call for each element, about 23 instructions). Fix: `#[inline(always)]`.
3. The code for a hidden element and for the way to `want` is in `Way::visit` and `Way::step`, which are `#[cold]`.

Hidden reads: `style_load hidden` makes 500 elements for each scale unit in a `display:none` `<div>` and reads 4 for each unit. It runs 38.3 M, 333.4 M and 17.29 G instructions at 1x, 10x and 100x. A hidden read runs the walk twice. Hypothesis: the first walk could be skipped if the caller knew that the element is hidden. Not measured.

Not tested: the second walk on a page with 100,000 elements and a hidden read in a loop, a shadow tree with a `display:none` host, a read inside a `display:none` `<iframe>` from another document, `display:contents` (the engine has no such value), cycles and memory of the second walk, and the live staging login.

## Element.outerHTML

Problem: `el.outerHTML` was `undefined`. Pages (and React) read it to copy or replace an element.

What runs: `outerHTML` is an accessor on `Element.prototype`, next to `innerHTML` (`browser_fragment.rs`). It adds no new algorithm.

- Getter: the same iterative serializer, with a flag that starts at the element and closes its tag on the way up. No recursion, so a 30,000-deep tree works.
- Setter: `replace(...)` is the old `innerHTML` setter with two changes. The context is the parent, and the commit puts the new nodes before the next sibling of the element, then detaches the element. The old element is never freed, so held wrappers stay valid. The input is converted first (`null` is the empty string), and the parent is read after that, so a `toString` that removes the element is seen.
- Parent rules: no parent does nothing. A Document parent throws `NoModificationAllowedError` (code 7). A shadow root is a DocumentFragment, so the context is `body`. A table, select, template, html, frameset or foreign parent is `Unsupported`, as for `innerHTML`, before anything changes.
- Text joins: Chrome joins a text node before or after the result with its neighbour (`a<b></b>c` with `outerHTML = "X"` leaves one node `aXc`). The standard does not say it. The commit does the same at the two seams, and the text cap counts the copy.

Chrome cases: 27 new cases (`outer_*`) in `tools/fixtures/js/dom-cases.txt`, recorded with `tools/js/dom_check.sh --update`. They cover the getter (attribute escaping, void elements, raw text parents, SVG, depth), the setter (position, identity of siblings, first and last child, `null`, `undefined`, numbers, no parent, Document parent, shadow root, form context, `p` context, inert scripts, text joins, a `toString` that detaches or throws) and the property shape (descriptor, brand check). 118 cases match.

Cost: `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. Main d6d9b92 against this build. 27 rows: `fragment_load` (parse, replace, deep at 1x and 10x), `browser_load` (6 cases at 1x, 3 at 10x), `fetch_load`, `web_apis_load`, `style_load` and `loader_load` at 1x. All are below +0.5%. The worst is `fragment_load replace 10` at +0.377% (231,666,780 to 232,540,591). The best is `fragment_load parse 10` at -0.574%. `browser_load` rows are between -0.046% and +0.138%.

Not tested: `outerHTML` on an element in an iframe document, on a node with a shadow root, a parent that is a `template` content (gated), and the live staging login.

## requestAnimationFrame and the rendering step

Problem: React and the staging app call `requestAnimationFrame`. The host has no display and no vsync, and a session must still settle.

What runs: `requestAnimationFrame(cb)` gives a handle (a number from 1, never reused). `cancelAnimationFrame(handle)` cancels it. A frame is one `Task::Render` of the scheduler. It is due at the next multiple of 1000/60 ms of the scheduler clock. It runs the callbacks that were queued before it started, in call order, then `ResizeObserver`, then `IntersectionObserver`. Every callback of one frame gets the same timestamp. A callback added during a frame waits for the next frame. A throw is reported (type `animationframe`) and the frame goes on. A microtask checkpoint follows each callback.

Design: a frame is queued only when a callback or an observer wants one, so an idle page queues none and `Browser::advance_time` has nothing to run. A page that asks for a frame in each frame gets one frame for each 16.67 ms of virtual time. Bound: `animation-frame-callbacks`. Gates: none. Not built: `requestIdleCallback`, `document.timeline`, `Animation`.

Checks: 21 cases in `tools/fixtures/ui/raf-cases.txt` match headless Chrome (`tools/ui/chrome_topic.mjs raf`, answers in `chrome-raf.txt`). 6 Rust tests cover what Chrome cannot show: the frame grid of the virtual clock, the bounds and the step order with `ResizeObserver`.

## MutationObserver

Problem: the live staging login stopped at `ReferenceError: MutationObserver is not defined`, thrown from a React effect.

What runs: `new MutationObserver(cb)`, `observe(node, options)` (`childList`, `attributes`, `characterData`, `subtree`, `attributeOldValue`, `characterDataOldValue`, `attributeFilter`), `disconnect()`, `takeRecords()`, and `MutationRecord` with `type`, `target`, `addedNodes`, `removedNodes`, `previousSibling`, `nextSibling`, `attributeName`, `attributeNamespace` and `oldValue`. The delivery is one microtask for the realm. `textContent`, `innerHTML`, `replaceChild` and a many-node `append` or `prepend` make one record. A node that is removed stays watched by the `subtree` observers of its old ancestors until the next delivery (transient registered observers).

Design: the native document reports each change to one watcher (`Document::set_watcher`, `Change::{Link, Unlink, Attr, Text}`). The watcher is set by the first `observe` and removed with the last registration. A page with no `observe` pays one `Option` check at a change and one `Cell` read at a host call (`Dom::mo.armed`). Registrations are in a table by node, and a change walks the ancestors of its target. The microtask is queued after the host call returns, because the watcher cannot reach the interpreter. A change made by the host itself (a loader) is delivered after the next host call.

Deviations: record fields are own data properties, not prototype getters. `addedNodes` and `removedNodes` are arrays, not `NodeList`. An observer with a registration or a queued record is a GC root until `disconnect`. `replaceChild` with a node from another parent queues the removal record of that parent after the record of the old child. Not built: `insertData` and `Range` edits as text changes. Bounds: `mutation-observers`, `mutation-registrations`, `mutation-records`.

Checks: 52 cases in `mutation-cases.txt` match Chrome. 5 Rust tests run besides the Chrome cases.

## HTMLElement members

What runs: `hidden` (boolean, or `until-found`), `inert`, `tabIndex` (also on `SVGElement`), `click()`, `offsetParent`, `offsetTop`, `offsetLeft` and `innerText`. `offsetWidth` and `offsetHeight` already existed. `innerText` follows the rendered text collection steps over the native cascade: `display:none`, `visibility`, `white-space`, `text-transform`, `br`, block breaks, `p` double breaks, table cell tabs and row line feeds. The setter splits at line breaks into text nodes with `<br>`. `offsetParent` is `null` for `html`, `body` and `position:fixed`, else the nearest positioned ancestor, or `td th table` for a static element, or `body`. `click()` sends one `click` event (not trusted, bubbles, cancelable, composed) and a submit button submits its form.

Gates: `click()` on a link, a `label`, a `summary`, a checkbox or a radio (their default action is built only for the window). `innerText` over a shown `select` or `textarea`. `offsetTop` and `offsetLeft` when the element or its offset parent has no fragment or more than one. A connected shadow tree.

Deviations: `click()` sends a plain `Event` (there is no `MouseEvent`). `innerText` does not know soft line wraps, `capitalize` looks at one text node at a time, and `display:contents` is not modelled.

Checks: 46 cases in `element-cases.txt` match Chrome. 4 Rust tests run besides the Chrome cases.

## IntersectionObserver

What runs: `new IntersectionObserver(cb, {root, rootMargin, threshold})`, `observe`, `unobserve`, `disconnect`, `takeRecords`, and the entry with `time`, `rootBounds`, `boundingClientRect`, `intersectionRect`, `intersectionRatio`, `isIntersecting`, `target`. One cascade and one bounded layout compute all observations in a frame. An observation reports when its threshold index or `isIntersecting` changes, and the first frame always reports. A `Task::Notify` task delivers the entries. The rectangle is cut by the `overflow` clip of each box on the containing block chain. `rootMargin` px values are floored, as in Chrome, and thresholds are `f32`, as in Chrome.

Gates: `trackVisibility: true`, `position: sticky` on the target or its chain, and the gates of the cascade and layout (a missing sheet, a connected shadow tree). Deviations: entries are plain objects with own data properties, `thresholds` is not frozen, `delay` and `scrollMargin` are not read. Bounds: `intersection-observers`, `intersection-observations`.

Checks: 48 cases in `intersection-cases.txt` match Chrome. 8 Rust tests run besides the Chrome cases.

## TreeWalker and NodeFilter

What runs: `document.createTreeWalker(root, whatToShow, filter)`, `TreeWalker` (`root`, `whatToShow`, `filter`, `currentNode`, `parentNode`, `firstChild`, `lastChild`, `previousSibling`, `nextSibling`, `previousNode`, `nextNode`) and `NodeFilter` (the constants). A filter is a function or an object with `acceptNode`, read at each call. A call from inside the filter into the same walker is `InvalidStateError`. A throw in a filter leaves `currentNode` as it was.

Gates and deviations: `NodeIterator` and `createNodeIterator` are not built. A `DocumentFragment` root is not possible yet. `nextNode()` from a detached node with no later node gives `null`, as Chrome does. Bound: `tree-walkers`.

Checks: 46 cases in `walker-cases.txt` match Chrome. 3 Rust tests run besides the Chrome cases.

## Cost of the DOM observers and the frame step

Method: `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. Main d6d9b92 against this build. 84 rows: `browser_load` (mutation, event, lookup), `style_load` (five cases), `sheets_load` and `loader_load` at 1x, 10x and 100x, `scroll_load 10000 2000`, `eval_load` (8 cases at scale 10), startup of an empty realm (`eval_check -e 0`), and `arrobj_load` (15), `binary_load` (9) and `proxy_load` (8) at scale 10. All are below +0.5%. The worst is `eval_load throws` at +0.19% (3,064,704,309 to 3,070,455,909). `browser_load` falls: event -9.3% to -9.7%, mutation -5.3% to -5.7%, lookup -2.6% to -3.0%. `sheets_load` and `loader_load` at 1x fall 4% to 9%. Start of a page (`browser_load event 0`) is 9,333,137 instructions, down from 10,073,813 (-7.4%). Clippy reports 49 warnings on main and 49 on this build.

The first build failed the rule on 20 rows: `loader_load mixed 1` +1.39%, `browser_load event 100` +1.16%, `sheets_load imports 1` +1.11%, `browser_load mutation 100` +0.79%. Two causes:
1. The collector asks the host `trace` for every object, and one `trace` call costs about 4 k instructions (about 40 table lookups). 30 new function objects cost that in each collect. Fix: the collector does not ask the host about a host function (`Callee::Host`), because such a function owns no host state (`Interp::collect` in `eval.rs`). This made `browser_load event` 9-10% lower than main.
2. A fixed start cost of +133 k instructions: about 4.5 k for each new method and 5.7 k for each new accessor (the function object, a new interned name, and the property). Fix: `Heap::init_function` stores `length` and `name` of a new function in one step, not in two `define_own_property` calls. All host and native functions use it. Start-up fell from +133 k to -740 k against main. Every `bare` row that is dominated by start-up fell 4% to 9%.

Hypothesis: the same two changes explain the small drops on `style_load shown` at 1x (-3.5%). Not measured by themselves.

Load matrix (`tools/js/observer_load.sh`, 6 processes, 1 GiB address space and 45 s each, checksums pass). `mo`: SCALE*1000 elements made, appended, given a `class` and removed in blocks, with one observer on the list (`childList`, `attributes`, `subtree`). `raf`: SCALE*500 frames, each sets a style on 20 elements and asks for the next frame.

| case | scale | run | peak RSS | peak live | live after |
| ---- | ----- | --- | -------- | --------- | ---------- |
| mo | 1x | 16 ms | 8.9 MiB | 1.8 MiB | 1.3 MiB |
| mo | 10x | 215 ms | 10.3 MiB | 3.8 MiB | 2.6 MiB |
| mo | 100x | 2.17 s | 17.3 MiB | 11.4 MiB | 10.2 MiB |
| raf | 1x | 29 ms | 8.3 MiB | 1.1 MiB | 1.0 MiB |
| raf | 10x | 288 ms | 9.4 MiB | 3.1 MiB | 3.0 MiB |
| raf | 100x | 2.85 s | 19.6 MiB | 18.0 MiB | 17.9 MiB |

These are single runs on a quiet machine, taken before `init_function` (a run under load 15 took 2.5 times longer for the same instructions). Growth with scale has known causes that are not new. `mo`: removed nodes stay in the DOM arena (200,005 nodes at 100x in `browser_load mutation` too). `raf`: the attribute text buffer never frees, and 1,000,000 `style` writes keep 11.25 MiB (`stats().text_bytes`). A page that writes a style on each frame grows without a bound; this is a follow-up for the DOM text buffer.

Not tested: a page with 10,000 observed targets for `IntersectionObserver` at 100x, `MutationObserver` with `characterData` at 100x, a frame loop with `IntersectionObserver` and `ResizeObserver` together, the live staging login on this build, and `perf` rows for `svg`, `svgparse` and `rel` of `browser_load`.
## XMLHttpRequest

Problem: pages that call `new XMLHttpRequest()` threw `ReferenceError`. Many libraries still use it for login and API calls.

What runs (`browser_xhr.rs`): the object follows the XHR Standard on the network path of `fetch`. There is no second network stack.

- Request: `send()` calls `queue_xhr` in `browser_fetch.rs`. The queue entry is a `Pending` that names the XHR object and a sequence number. It uses the same origin gate, queue cap, byte caps and body extraction as `fetch`, and `run_network_tasks` runs it with the same client and redirects. The pending entry roots the object. `abort_fetches` (a page that leaves) aborts it with the `abort` events.
- Events: the reply arrives whole, so `Browser::xhr_deliver` fires the events in three steps, each with a microtask checkpoint after each listener. Step 1: the upload `progress`, `load` and `loadend` (a body and an upload listener), then `readystatechange` at state 2. Step 2: state 3, `readystatechange`, `progress` (only for a body that is not empty). Step 3: state 4, `readystatechange`, `load`, `loadend`. Before each step the request is checked again, so a listener that calls `abort()` or `open()` ends the delivery. An error, an abort or a timeout run the request error steps of the standard. No event object is made when nobody listens.
- Response: `text` and `""` decode UTF-8 (a BOM is removed). A charset other than UTF-8 is `Unsupported` unless every byte is ASCII. `json` parses at each read, and a parse error gives `null`. `arraybuffer` and `blob` are made once and cached. `getAllResponseHeaders` gives lower case names, sorted, joined with `\r\n`. `set-cookie` is hidden.
- `ProgressEvent`: `loaded`, `total` and `lengthComputable` live in a side table keyed by the event object.
- Globals: four lazy accessors, like `AbortSignal`. A realm that never reads them makes no prototype, no function and no object.
- GC: the side tables are pruned at `collect`. An XHR object keeps its `upload` object and its cached response alive. `Dom::trace` skips the XHR table while no XHR object exists.

Gates (`Unsupported`, before anything changes): `open` with `async = false`, a user name or a password, `responseType = "document"`, a Document body, a Blob or FormData body (as for `fetch`), a token method or a lower-case `patch` (`bare_net::http::Method` has seven), another origin.

Deviations from Chrome: `timeout` is checked when the reply arrives, because the blocking read cannot be stopped, so a late reply is a `timeout` and not a `load`. `statusText` is the standard phrase of the code and not the phrase of the server (the client drops it). `withCredentials` is stored only, as there is no cookie jar. `fetch` with a URLSearchParams body now sends `application/x-www-form-urlencoded;charset=UTF-8`, as the standard says (this shared code is used by both).

Chrome cases: `tools/ui/chrome_xhr.mjs` starts a local server and runs `tools/fixtures/ui/xhr-cases.txt` in headless Chrome. It records event order, headers, statuses and bodies in `tools/fixtures/ui/xhr-expected.txt`. The 68 cases cover GET text and JSON, POST with a body and a header, 404, a network error, abort in flight, timeout, the four response types, the header format, a redirect, the ready state sequence, `upload` events, handler properties, the constants and error cases. The Rust test `every_recorded_chrome_case_gives_the_same_result` runs the same cases on a Rust server with the same endpoints. All 68 match.

Cost: `perf stat -e instructions:u`, minimum of 3 runs, each binary in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. Main 665c141 against this build. 27 rows: `fragment_load` (6), `browser_load` (9), `fetch_load` (4), `web_apis_load` (3), `style_load` (2), `loader_load` (3). All are below +0.5%. The worst is `loader_load blocking 1` at +0.322% (17,803,608 to 17,860,935). The best is `fragment_load deep 10` at -0.058%.

Load rows (`tools/js/xhr_load.sh`, release build, 1 GiB address space cap): sequential GETs to a loopback server, one `XMLHttpRequest` per round, a collect every 100 rounds.

| GETs | time | peak RSS | wrappers / events / listeners after collect |
| ---: | ---: | -------: | ------------------------------------------- |
| 1 | 0.3 ms | 8.5 MiB | 2 / 0 / 0 (baseline) |
| 100 | 3.5 ms | 8.8 MiB | 2 / 0 / 0 (baseline) |
| 1,000 | 31.8 ms | 9.9 MiB | 2 / 0 / 0 (baseline) |

Not tested: a synchronous request (gated), `responseType = "document"` (gated), a response in a charset other than UTF-8, a request to another origin, `HEAD`, `PUT` and `DELETE` (the net layer has GET and POST), a Blob or FormData body (the parallel task has not landed), and the live staging login.


## Range and Selection

Problem: React, editors and focus libraries read `getSelection()` and `document.createRange()`. Both were missing.

What runs: `document.createRange()`, `new Range()`, `Range` with `startContainer`, `startOffset`, `endContainer`, `endOffset`, `collapsed`, `commonAncestorContainer`, `setStart`, `setEnd`, `setStartBefore`, `setStartAfter`, `setEndBefore`, `setEndAfter`, `selectNode`, `selectNodeContents`, `collapse`, `cloneRange`, `detach` and `toString`. The five boundary getters are on `AbstractRange`, as in Chrome. Offsets are UTF-16 units in a Text or a Comment and child indexes elsewhere. A start after the end, or a point in another tree, moves the other point (the standard). `window.getSelection()` and `document.getSelection()` give one `Selection` per realm with `rangeCount`, `addRange`, `removeAllRanges`, `empty`, `removeRange`, `getRangeAt`, `collapse`, `setPosition`, `collapseToStart`, `collapseToEnd`, `selectAllChildren`, `isCollapsed`, `anchorNode`, `anchorOffset`, `focusNode`, `focusOffset`, `type`, `direction` and `toString`. The selection holds the range object that `addRange` was given, as Chrome does, so changing that range changes the selection. A range whose root is not the document is not added.

Design (`browser_range.rs`): a range is an object with an entry `{start node, start offset, end node, end offset}` in a table, like a `TreeWalker`. The globals are lazy (`browser_lazy.rs`), and the state is made on the first read of a global or call of `getSelection` or `createRange`. A page that uses none pays the accessors only (`getSelection`, `createRange` are two host functions). The order of two points is found from the paths to the root, so it costs the depth. `toString` walks the text nodes under the common ancestor. The selection object is a GC root, it traces its range, and the collector drops the table entries of dead ranges. Bound: `ranges` (65,536 live).

Gates (`Unsupported`): `deleteContents`, `extractContents`, `cloneContents`, `insertNode`, `surroundContents`, `createContextualFragment`, `getBoundingClientRect`, `getClientRects`, `compareBoundaryPoints`, `comparePoint`, `isPointInRange`, `intersectsNode`, and on `Selection`: `extend`, `setBaseAndExtent`, `containsNode`, `modify`, `deleteFromDocument`.

Known gaps: a range does not update when the DOM changes. After `removeChild`, `textContent` or a text edit a boundary point keeps its node and offset, and a stale offset is clamped when it is read (a test covers it). `Selection.toString()` is the text of the Text nodes, not rendered text (Chrome adds line breaks between blocks). There is no caret, focus, `selectionchange` event or mouse selection, and the selection of an `<input>` (`browser_selection.rs`) is separate. Cutting a surrogate pair in `toString` gives U+FFFD. `Selection.direction` is `forward` for a range and `none` for a caret (a backward selection cannot be made yet).

Checks: 79 cases in `tools/fixtures/ui/range-cases.txt` match Chrome 155 (`node tools/ui/chrome_topic.mjs range`, answers in `chrome-range.txt`). Two Chrome cases that depend on live ranges (a text edit under a range) were left out because the host has the gap above. 7 Rust tests cover the lazy globals, the gates, the gap, a collect, the bound, a page that does not use ranges and the UTF-16 cut.

Cost: `perf stat -e instructions:u`, minimum of 3 runs, main c376437 (with PR #160) against this build, the same 52 rows as in the observer section. All below +0.5%. The worst is `loader_load mixed 1` at +0.17% (18,035,243 to 18,066,160), then `loader_load dynamic 100` +0.13% and `browser_load event 100` +0.12%. Startup is +54 instructions (2,745,040 to 2,745,094). `HTMLElement` `innerText`, `offsetParent`, `offsetTop`, `offsetLeft` and `click()` already ran from the previous section; `offsetWidth` and `offsetHeight` existed before.

Not tested: the live staging login on this build, a range over a 100,000-node tree, `toString` of a very large range under the step bound, and a perf row that uses ranges at 1x, 10x, 100x (no load example was added for this API).

## HTTP methods

Problem: the staging app uses `method:"PUT"` 28 times, `"DELETE"` 29, `"PATCH"` 5 and `"HEAD"` 4, and `bare_net::http::Method` had `Get` and `Post` only.

What runs: `Method` has `Get`, `Post`, `Put`, `Patch`, `Delete`, `Head` and `Options`. `fetch`, `new Request` and `XMLHttpRequest` use the same `wire_method`, so there is one path.

- Body: PUT, PATCH, DELETE and OPTIONS send their body with `Content-Length`. POST and PUT without a body send `Content-Length: 0`. PATCH, DELETE and OPTIONS without a body send no length. Chrome does the same (the task text said PATCH sends `0`; Chrome and the Fetch Standard do not, and the fixture follows Chrome). A body on GET or HEAD is a `TypeError` in `fetch` and is ignored by `XMLHttpRequest.send`. The client refuses it as well.
- HEAD: `send` sets the framing of the reply to "no body" whatever `Content-Length` says, in `get`, `send` and `send_stream`. The connection goes back to the idle list, and a GET after it reads right. 204 and 304 had no body before this change. HEAD counts as safe: the client may send it again on a new connection, as GET.
- Names: `put`, `delete`, `get`, `head`, `options` and `post` become upper case. `patch` stays as written. The error for forbidden `CONNECT`, `TRACE` and `TRACK` was already there (`TypeError` in `fetch`, `SecurityError` in `open`).
- Redirects: 301 and 302 turn a POST into a GET and drop the body and the body headers. 303 turns every method but GET and HEAD into a GET. 307 and 308 keep the method and the body. The same rule is in `Client::send` and in the `fetch` queue.
- Gates (`Unsupported`): a token method such as `FOO`, and a lower-case `patch` (the enum cannot hold it, and a wrong wire form is worse than a gate).
- CORS: there is no CORS and no preflight. Every request to another origin is `Unsupported` before a socket, whatever the method, so a non-simple method needs no preflight in this build. `docs/staging-network.md` does not say if the API host is another origin (open point at the end of that file). If it is, `fetch` needs CORS with a preflight (`OPTIONS` with `Access-Control-Request-Method` and `-Headers`, and a check of `Allow-Origin`, `-Methods`, `-Headers` and `-Credentials`) as a separate topic.

Chrome cases: `tools/ui/chrome_methods.mjs` runs `tools/fixtures/ui/methods-cases.txt` (39 cases) in headless Chrome against a raw TCP server and records the request line, `Content-Length`, `Content-Type` and body of each request in `methods-expected.txt`. The Rust test `the_methods_send_what_chrome_sends` runs the same cases and compares. 38 match; `patch-lower` is the gate above.

Rust tests: `each_method_sends_its_exact_bytes`, `a_head_reply_has_no_body_and_the_connection_stays_in_step`, `every_method_follows_every_redirect_code_like_fetch` (net); the HEAD, forbidden, body-on-GET, cross-origin and Chrome tests (fetch); `each_method_runs_and_the_forbidden_ones_are_a_security_error` (XHR).

Cost: `perf stat -e instructions:u`, minimum of 3 runs, main 71ed495 against this build, each in its own `CARGO_TARGET_DIR` under `/data/bare-tmp`. 23 rows: `fetch_load` (8), `loader_load` (5), `browser_load` (8), `xhr_load` (2). The worst is `loader_load mixed 1` at +0.084% (18,075,497 to 18,090,754). The lowest is `browser_load event 1` at -0.009%. All are below +0.5%.

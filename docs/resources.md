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

- `:nth-child` and `:nth-of-type` count the siblings for each element. 5,000 siblings take 50 ms, 50,000 take 5 s.
- Many classes on one element and many rules on them (5,000 and 5,000: 126 ms, 50,000 and 50,000: 13 s). The class list is scanned for each test.
- The cap of 256 on the Noah's Ark scan changes the tree only for a page with more than 256 formatting elements open between two identical ones.
- The three timeouts of `net-silent-server`, `net-drip-server` and `net-huge-content-length` are the wait of the client (30 s for each read, 120 s for the whole reply), not a bug. They are longer than the 20 s of the test.
- The 100x runs of `css-rules`, `css-selector-list` and `html-decode-bytes` stop at 2 GB because the input has 130 MB to 500 MB. Memory grows in line with the input.

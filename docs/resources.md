# Resource use

This file records how much time and memory each part of the browser uses. Our priorities are speed, efficiency and low memory use. Update the tables when a module changes, and add a row for each new module.

## How to measure

Run `cargo run --release -p bare-bench`. It builds one page of 100,000 `<div>` and 100,000 `<span>` elements (5.2 MB) and runs each step five times.

- `ms` is the median time of the five runs.
- `peak MB` is the most heap memory alive at once during the step.
- `kept MB` is the heap memory that the result still holds, for example the DOM.
- A counting allocator measures the memory. It counts bytes that the program asks for, so it is lower than the RSS that `top` shows.

Machine: archMachine, release build, 2026-10-29.

## bare, by module

| Module | Step                         | ms   | peak MB | kept MB | Note                           |
| ------ | ---------------------------- | ---- | ------- | ------- | ------------------------------ |
| 1      | URL parse, 100,000 URLs      | 17   | 0.0     | 0.0     | about 170 ns for each URL      |
| 1      | URL join, 100,000 links      | 29   | 0.0     | 0.0     |                                |
| 2      | Bytes to text                | 8    | 15.6    | 5.2     | one copy of the page           |
| 2      | Tokenizer                    | 59   | 126.0   | 105.1   | 600,006 tokens, 175 bytes each |
| 2      | Tree builder (with tokens)   | 87   | 135.2   | 66.9    | 400,006 nodes, 167 bytes each  |

Not measured:

- DNS, TCP and HTTP. They wait for the network, so a CPU benchmark says little. `parse_response` is private.
- The `bare` binary is 2.1 MB (release build).

What the numbers say:

- The tokenizer returns a `Vec` of all tokens. For the tree builder this is the peak: 135 MB peak against 67 MB kept. A tokenizer that gives one token at a time would remove about 100 MB of peak memory. This is the first fix to make.
- Each token owns its `String`s. A token that points into the page text would avoid most of the 175 bytes.
- Each DOM node costs 167 bytes. Chrome's number (below) suggests that this can be much lower. To verify: shrink `NodeData`.

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

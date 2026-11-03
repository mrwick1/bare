# Decisions

This file records the current decisions for `bare`. When a decision changes, update its section.

## Purpose

`bare` is a web browser written from scratch. Its purpose is to learn how the core parts of a browser work.

## Language

The project uses Rust. Servo shows that Rust works for browser engines. Safe Rust has no memory-safety crashes.

## Dependencies

`bare` uses no crates. Every part is written by hand: URL, HTTP, TLS, HTML, CSS, layout, paint, window, fonts and JavaScript.

Allowed:

- The Rust standard library (`std`).
- Direct calls to OS functions (for example `mmap`, `memfd_create`), declared by hand with `extern "C"`. This is `unsafe` code. Keep it small and in per-OS modules.
- Data files from standards bodies, for example the Unicode Character Database. These are data, not code.

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
| `bare`         | `crates/bare/`    | the app (`main.rs`)         | `bare-net`, `bare-url`, `bare-html` |

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
  - Linux: the Wayland protocol over a Unix socket, with a shared-memory pixel buffer.
  - Windows: Win32 functions (`CreateWindowExW`, `StretchDIBits`).
  - macOS: Cocoa through the Objective-C runtime (`objc_msgSend`).
- Other OS calls (for example memory with `mmap`) also go behind small per-OS functions.
- Each port is built and tested on its own OS.

## TLS

`bare` implements TLS 1.3 and its crypto by hand (SHA-256, HKDF, X25519, AES-GCM, X.509 certificate checks). This is for learning only. Hand-written crypto is not safe against real attackers. Do not use `bare` for sensitive sites.

## Fonts

Text starts with a built-in bitmap font. A TrueType parser and rasterizer come later.

## Module plan

Tabs and the event loop come before JavaScript, because JavaScript needs the event loop.

The modules, their order and their state are in [progress.md](progress.md).

## Open questions

- A separate process for sensitive sites, for example banking.
- When to replace the bitmap font with TrueType.

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

## Unicode data

`data/unicode/` holds the Unicode data files and their license. `tools/gen_unicode_tables.py` turns them into `src/unicode_tables.rs`. The generated file is committed, so a build needs no network and no Python.

To update Unicode: download the new files over the old ones, run the script, and run `cargo test`.

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

| #   | Module                                                 | State   |
| --- | ------------------------------------------------------ | ------- |
| 1   | URL, DNS and HTTP (`url.rs`, `dns.rs`, `http.rs`)      | now     |
| 2   | HTML tokenizer and DOM tree                            | next    |
| 3   | CSS parser and style cascade                           | planned |
| 4   | Layout (box model, text flow)                          | planned |
| 5   | Window (Wayland) and paint, bitmap font                | planned |
| 6   | Input and navigation (links, scroll)                   | planned |
| 7   | HTTPS (TLS 1.3 and crypto, by hand)                    | planned |
| 8   | Tabs and event loop (one engine, a task queue per tab) | planned |
| 9   | JavaScript (interpreter, no JIT)                       | planned |
| 10  | Tab isolation and memory accounting (heap per tab)     | planned |
| 11  | Resource guard (watchdog, freeze, discard, reload)     | planned |

## Open questions

- A separate process for sensitive sites, for example banking.
- When to replace the bitmap font with TrueType.

# Teams gap census (Phase 1)

Status: first run 2026-11-25, main 96e3edd7 and the branch `gap-census`. Input: the offline Teams replay (`docs/internal/design-replay-sessions.md`, tape3). Reference: the Chrome 155 capture of the same account (`tools/ref/chrome-teams-ref.mjs`).

## Method

1. `BARE_CENSUS=FILE` (Vex, `crates/vex/src/census.rs`) logs each name that a script reads, or tests with `in`, on a host object where the name is not there. A line is `TAG NAME get|has COUNT`. The tag is the `Symbol.toStringTag` of the object. A line is written at the counts 1, 2, 4, 8, and so on.
2. `node tools/ref/chrome-has.mjs FILE` asks Chrome the same names on the same kind of object (a worker for `DedicatedWorkerGlobalScope`). It prints the names that Chrome has. Untagged objects (`?`) and modules are not asked.
3. The page itself: element boxes, `getComputedStyle` and the style sheets, read with `expect js` steps on the replay and compared with the Chrome boxes (`at-main-area.boxes.json`).

The census gives no source location. Vex updates its frame pointer only at native calls (`publish!` in `interp.rs`), so the location at a plain property read can be wrong. To find the code that reads a name, search the scripts on the tape.

## Result of the first run

- Replay: 0 misses on the tape, 19.6 to 22.6 s.
- Census: 185,516 lines and 41,808 distinct (tag, name, kind). Of these, 41,278 are on untagged objects (plain objects and functions whose chain reaches a host object) and 150 are on modules. Hypothesis: these are the page's own feature and expando checks, not browser gaps. Not checked against Chrome.
- 45 names that Chrome has and bare does not are in the table below.

## Gaps, by subsystem, most important first

### 1. Layout: `display: contents` (blocks the Teams shell)

- bare does not support `display: contents`. The CSS engine computes `block`, and layout makes a box.
- Teams puts the areas `header`, `contextual-notifications` and `main` in a wrapper with the class `f1bxpd7w` (`.f1bxpd7w{display:contents;}` in `griffel-dfe43dc1df2d2f4a.css`). In bare the wrapper is a grid item. It goes to an extra column at the right, and the chat pane is 183 px wide.
- Needs a design document (layout and style: box tree, events, hit tests, `getBoundingClientRect`, `getComputedStyle`).

### 2. CSS object model

- Fixed in the PR `cssom-grid-template`: `el.style.gridTemplate = …` and `setProperty('grid-template', …)` did nothing. The Teams grid had no areas. With the fix the grid has its 6 rows of areas, and the title bar is at (0, 0).
- `getComputedStyle(el).gridArea` throws `Unsupported("this shorthand has no single computed text …")`. It must give a string.
- `el.style.gridTemplateAreas` (and the rows and columns) read `""` after `el.style.gridTemplate` is set.
- `getComputedStyle(el).display` is `block` for `display: contents` (part of item 1).

### 3. Web platform names (Chrome has them; Teams reads them)

| Area              | Names (times read, to a factor of 2)                                                                                                                              |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Scheduling        | `scheduler` on the window (2) and in workers (256), `navigator.scheduling` (2), `IdleDeadline` (1)                                                                |
| Device and screen | `screen.orientation` (256), `navigator.connection` (256 `in`, 16 reads), `navigator.windowControlsOverlay` (8), `navigator.permissions` (2)                       |
| Performance       | `PerformanceNavigationTiming` `workerStart` (8), `serverTiming` (8), `redirectStart` (4)                                                                          |
| Security          | `trustedTypes` on the window (8) and in workers (2)                                                                                                               |
| Events            | `CompositionEvent`, `TextEvent`, `AnimationEvent`, `TransitionEvent` (2 `in` checks each)                                                                         |
| DOM               | `document.all` (128), `reportError` (2), `document.ariaNotify` (1); in workers `DOMStringList` and `SourceBufferList` (2 each)                                    |
| Media             | `AudioContext` (2)                                                                                                                                                |
| Browser identity  | `window.chrome` (2), `external` (2), `launchQueue` (2). bare sends a Chrome user agent. Hypothesis: some code takes another path when `window.chrome` is missing. |

### 4. JavaScript (Vex)

- `SuppressedError` (2), `Intl.getCanonicalLocales` (2), `Intl.ListFormat.prototype.format` (1).

### 5. Harness

- WebSocket replay is missing: 7 connection errors on each replay (`trouter`, `augloop`).

## Open

- The app bar has no app entries and the chat list is hidden (`app-layout-area--start` is `display: none`). Chrome has the app entries already when the main area shows. To check after item 1: whether the 147 lazy scripts that only Chrome loads then load.
- Hypothesis (performance): misses on plain objects go through `hs_walk`, because the chain of a plain object reaches a host object. The top untagged names were read up to 524,288 times in one run. To measure.

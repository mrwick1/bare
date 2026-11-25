# `display: contents`

Status: proposed 2026-11-25, for review by the owner. Phase 2 of the Teams plan (`docs/internal/teams/gap-census.md`, item 1).

## Problem

- bare does not parse `display: contents`. The declaration is dropped, so the element keeps its other `display` (`block` for a `div`), and layout makes a box for it.
- Teams puts the grid areas `header`, `contextual-notifications` and `main` in a wrapper with `display: contents`. In bare the wrapper is one grid item. It goes to an extra column, and the chat pane is 183 px wide at the right (offline replay, 2026-11-25). Chrome places the 3 areas in the grid of the page.
- The spec (CSS Display 3, "display: contents"): the element makes no box. Its children, and its `::before` and `::after`, take its place in the box tree, as children of its parent's box. The element keeps its style, and its children inherit from it.
- Special cases in the spec:
  - The root element: `contents` computes to `block`.
  - The "unusual" elements of Appendix B (`img`, `video`, `canvas`, `iframe`, `embed`, `object`, `audio`, `input`, `textarea`, `select`, `br`, `wbr`, `meter`, `progress`, `frame`, `frameset`) act as `display: none`. A `legend` and a `button` have their own rules.
  - Blockification: a child of a contents element is an item of the flex or grid container above it, so it is blockified.
- WPT: `css/css-display` has 88 tests on `display: contents` (12 script tests, 74 reftests, 2 crash tests). Pinned checkout: `/data/bare-tmp/wpt` at f4d3f2e, sparse (`css/css-display`, `css/css-grid/grid-items`).

## Current design

- bare has no box tree (`crates/layout/src/boxes.rs`, module comment). Layout walks the flat DOM tree (`Document::flat_children`, `flat_next`, `flat_parent`: the shadow DOM is composed, and a slot is replaced by its assigned nodes). `Tree::kind` says what box a node makes. `Tree::children` gives the child boxes of a block: `Child::Block(node)`, or `Child::Lines(first, last)`, a run of flat siblings that makes lines.
- The run walks:
  - `Children` (`boxes.rs`) and the runs in `grid.rs`, `table.rs` and `flex.rs` step with `flat_next` until `last`.
  - `inline::Events` steps with `flat_next`, and keeps a stack of the open inline boxes. At the end of the children of an inline box it closes the box.
- The parent tests use `flat_parent`: `kind` (the root, and a child of a flex or grid container) and `block.rs` (a new block formatting context, the containing block).
- Style uses `flat_parent` too, for inheritance (`cascade.rs`).
- The cost of a walk is O(children). There is no memory for boxes. When the page changes, layout runs the walk again: there is no box tree to update.

## Options

### 1. A contents element is transparent in the walks (proposed)

- CSS: `display: contents` parses to `Keyword::Contents`. The computed value is `block` on the root and `none` on the unusual elements. `Styles` gets one flag: some element has `display: contents`.
- Layout: `Kind::Contents`. `Tree` gets 3 walk functions. With the flag off, each is the `flat_*` function it replaces, behind one branch:
  - `box_first(parent)`: the first flat child. A contents child is replaced by its own first child; an empty one is skipped.
  - `box_next(node)`: `flat_next`, with the same replacement. At the end of the children of a contents element it climbs to the element and goes on with its next sibling. It climbs only through contents elements, so it stops at the end of a block or of an inline box as now.
  - `box_parent(node)`: `flat_parent`, but it skips contents elements.
- The walks above use these. `Child::Lines(first, last)` stays a pair of nodes: `last` comes after `first` in the order of `box_next`. `inline::Events` needs no stack change.
- Style is unchanged: inheritance still uses `flat_parent`, so a child inherits from the contents element.
- Fragments: a contents element makes none. `getBoundingClientRect` then gives an empty rectangle and `getClientRects` an empty list, as in Chrome. Hit tests find the child boxes. The event target is the DOM node of the box hit, and the event goes up through the contents element as a normal DOM parent.
- Complexity: the same O(children) walk, plus O(depth of nested contents elements) to climb. No memory.
- Cost with the flag off: one predictable branch for each step of a walk. Pages without `display: contents` should not change (to measure: the layout rows).

### 2. A side table of box children for the parents of contents elements

- Before layout, one pass over the elements with `display: contents` builds `HashMap<NodeId, Vec<NodeId>>`: for each box parent, its box children in order. The walks read the table first.
- `Child::Lines` must become a range in that vector, not a pair of nodes. Every user of `Lines` and the inline walk change.
- Cost: an allocation for each such parent on each layout, and a hash probe for each `children` call while the flag is on. This is a partial box tree. It is the memory that the module comment of `boxes.rs` chose not to spend.

### 3. Skip contents elements in the flat tree of the DOM (as a slot is skipped)

- The style pass sets a bit on each contents element. `FlatChildren` then replaces it by its children, as it does a slot. Layout, paint and the rectangle code get the change without an edit.
- But style inherits through `flat_parent`. It needs the tree with the contents elements, and layout needs it without them. So the DOM would need 2 modes of the flat tree, and the bit would tie the DOM to the result of style. The bit must also change when a style changes.

Option 1 adds no data structure and no new pass. It follows the rule of `boxes.rs`: the box rules are functions over the DOM. Options 2 and 3 add state that must be kept in step with the DOM and the styles.

## Other engines (to verify in their source before the build)

- Chrome: no `LayoutObject` for the element. Layout tree traversal (`LayoutTreeBuilderTraversal`) skips contents ancestors to find the layout parent.
- Firefox: no frame for the element. The frame constructor puts the children's frames in the parent's frame.
- Servo (layout 2020) and Ladybird: the box tree builder walks the children of a contents element with the parent's handler. The Ladybird checkout at `/data/bare-tmp/ref/ladybird` has only part of LibWeb, so this was not checked there.

All of them build a box tree and handle `contents` while they build it. Option 1 is the same rule for a design with no box tree: the rule runs in the walk, not in a build.

## Build plan

1. CSS: parse `contents`; the computed value rules; the flag in `Styles`. `getComputedStyle(el).display` gives `contents`.
2. Layout: `Kind::Contents` and the 3 walk functions in `boxes.rs`. Change the walks of `boxes.rs`, `inline.rs`, `grid.rs`, `flex.rs`, `table.rs` and `block.rs`, and `kind`'s parent tests.
3. Browser: the rectangle code (`browser_rects.rs`, `browser_used.rs`) and the hit test give no box for a contents element. Remove the `display: contents` part of the `checkVisibility` workaround in `browser_element.rs` (`unmodelled`, `OddRule`), which counts rules that the engine did not parse.
4. Not in this build: `contents` on SVG elements (10 WPT tests), multicol (3), `::first-letter` and `::first-line` (5). Each needs a check of the support that bare has today.

## Check

- A WPT runner for this subset (new):
  - Script tests: open the test page over a local HTTP server and read the results of `testharness.js` from the page.
  - Reftests: take a screenshot of the test and of its `-ref` in bare, and compare the pixels. Both pages render in bare, so this checks bare against itself, not against Chrome.
  - The count today and after the build, for the 88 tests minus the ones left out above.
- New layout tests in `boxes.rs`: a contents element in a block, in an inline, in a flex and in a grid container, nested, empty, and with `::before`.
- Teams offline replay: the boxes of the `data-tid` elements of the shell are within 2 px of Chrome's `at-main-area.boxes.json`. The screenshots go in the progress file.
- Cost: the layout rows of `cost_rows.txt` (`bare-stress`, 176 rows) under the +0.5% rule. One new `bare-stress` case with nested contents elements, to measure the climb.

## Not known yet

- Whether the app bar entries and the chat list then show. They were missing in the replay. This build only places the grid areas.

# Generated content: `::before` and `::after`

Status: proposal, for owner review. No code yet. Base: origin/main `8e958c22`. Source ticket: OFS-50.

## Problem

Bare does not draw `::before` and `::after`. A rule such as `p::before { content: "x"; color: red }` has no effect. Many pages put labels, icons, quotes, counters and clearfix boxes in these pseudo-elements. In Bare those parts are missing, and an agent cannot read them or check them on a screenshot.

`getComputedStyle(el, '::before')` also gives the wrong values. It returns the inherited values of the element and the initial rest, and `content` is empty.

## Current design

- Selectors (`crates/css/src/selector.rs:39`, `:512`): `Parser::pseudo_element` reads `::before`, `::after` and the old one-colon forms. It returns `Pe::Never`, and the compound gets `Simple::Never`. So the rule is valid but never matches.
- Cascade (`crates/css/src/cascade.rs`): `Styles` keeps one style index for each node in `of_node`, by `NodeId::index` (`:2513`). `RuleSet::walk` styles the whole tree. `RuleSet::restyle` styles changed roots again. `content` is not a property.
- DOM (`crates/html/src/dom.rs`): one arena of nodes. Two side tables already keep nodes that are not children of their owner: `shadow_of` (shadow roots) and `template_content`. `flat_children` (`:1515`) gives the children of a node in the flat tree. It has a fast path when no shadow root exists.
- Layout (`crates/layout/src/boxes.rs`): no box tree, by design. Layout walks the DOM through `flat_children` and `flat_parent` (24 uses in `crates/layout`). `Tree::kind` decides the box of a node from its style. List markers are the only generated boxes. They are a special case in `block.rs`, `inline.rs`, `flex.rs` and `table.rs` (`cx.marker`).
- Paint and hit testing (`crates/paint`): they walk fragments, and each fragment names a `NodeId`. `hit::element_of` (`hit.rs:422`) maps a node to its nearest element.
- Host (`crates/bare`): `restyle` (`browser_ui.rs:1221`) runs the cascade and gives an `Rc<Styles>`. The host has `&mut Document` there. `browser_computed.rs:72` documents the missing pseudo-element cascade.

## How other engines do it

- Chrome: a `PseudoElement` is a real `Element` subclass. It is not in the DOM child list. The element keeps it, and the layout tree builder adds its box as the first or last child.
- Firefox: generated content is "native anonymous content". The frame constructor makes element and text nodes that scripts do not see.
- Servo (layout 2020): box construction reads the pseudo-element style of each element and makes the boxes. No node exists.

Chrome and Firefox both make nodes. Servo can skip nodes because it has a box tree. Bare has no box tree, so nodes fit Bare better.

## Options

### A. Pseudo nodes in the flat tree (recommended)

- The cascade matches `::before` and `::after` rules for each element and computes a pseudo style with the element style as parent. It returns the list of `(element, which, style)` where `content` makes a box.
- The host then makes or updates the nodes. For each pseudo-element it makes one element node and one text node with the `content` text. A new side table `Document::pseudo_of` (element index to `[Option<NodeId>; 2]`) keeps them, like `shadow_of`. The nodes are never in a child list. So `children`, `querySelectorAll`, `textContent` and serialization do not see them.
- `flat_children` gives the `::before` node first and the `::after` node last. `flat_parent` of a pseudo node gives its element. The fast path stays: it checks one more empty table.
- `Styles::of_node` gets the pseudo style for the pseudo element node. The text node takes its style from its parent, as now.
- Layout, paint and the inline code then handle the boxes with no change per site. `display`, `position`, `float`, sizes and backgrounds work as for any element.
- Hit testing, events, `elementFromPoint` and focus map a pseudo node to its element in `hit::element_of`.
- `getComputedStyle(el, '::before')` reads the pseudo style.

Cost (hypothesis, to measure in step 2): one test of an empty table in `flat_children`. One more match pass for each element only when the sheet has a `::before` or `::after` rule. Memory: two nodes and one style for each pseudo-element that has `content`.

### B. A generated-child hook in layout

- The cascade gives pseudo styles as in A. No nodes are made.
- `boxes::Tree::children`, the inline item walk, flex, grid and table item lists, intrinsic sizes and paint learn a new child kind that is not a `NodeId`.

Cost: no DOM change. But the 26 `flat_children` and `flat_parent` sites in layout and paint change, `Fragment` needs a new owner kind, and hit testing needs its own mapping. More code, and each new layout feature must remember the pseudo boxes.

### C. A stored box tree

Bare chose not to store a box tree (`boxes.rs` header). Generated content alone does not justify that change. Rejected.

## Recommendation

Option A. It reuses the pattern of `shadow_of` and `template_content`, keeps layout unchanged, and matches Chrome and Firefox.

## Scope of the first version

- `::before` and `::after` only. `::marker`, `::first-line`, `::first-letter` and `::placeholder` stay as now.
- `content`: `normal`, `none`, strings, `attr(name)`, and a list of these. `content: ""` makes an empty box, so clearfix and icon boxes work.
- Out of scope: `counter()`, `counters()`, `open-quote` and `close-quote`, `url()` images, `content` on normal elements, and animations of pseudo-elements.
- `getComputedStyle(el, '::before').content` gives the serialized value, as in Chrome.

## Plan

1. Selectors and cascade. Keep `::before` and `::after` in `Selector` instead of `Simple::Never`. Index these rules apart. Compute pseudo styles in `walk` and `restyle`. Parse `content`. `getComputedStyle` reads the pseudo style. Tests: cascade unit tests, `browser_computed` tests.
2. Nodes and layout. Add `Document::pseudo_of` and its use in `flat_children` and `flat_parent`. The host makes and updates the nodes after each restyle. Tests: layout tests for inline text, `inline-block` boxes and `display: block`. A screenshot test for the case of the ticket.
3. Hit testing and script views. Map pseudo nodes to their element in `hit::element_of` and in the `crates/bare` uses of `flat_children` (20 uses). Tests: `elementFromPoint`, a click on a `::before` box, `getBoundingClientRect` of the element.

Each PR runs the style and layout rows of `tools/js/cost_rows.txt`. A page with no `::before` or `::after` rule must stay within the cost rule.

## Open questions

1. To verify: does making a node bump `Document::revision` or call the mutation hooks? Pseudo nodes must not. They may need a separate maker that skips both.
2. To verify: does the arena reuse removed nodes? If not, a page that changes `content` often grows the arena. A free list for pseudo nodes may be needed.
3. To verify: which of the 20 `flat_children` and `flat_parent` uses in `crates/bare` must skip pseudo nodes (for example `innerText`, which the spec builds from DOM children).
4. Accessibility: Chrome adds `::before` and `::after` text to accessible names. Should the snapshot of `bare-drive` do the same in step 3, or later?
5. Should `restyle` with a changed `content` value only update the text node, or always make the nodes again?

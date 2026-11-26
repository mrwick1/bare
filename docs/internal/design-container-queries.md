# Container queries: `@container` and `container-type`

Status: proposal. The owner reviews it before the build.

## Problem

- bare drops `@container` blocks (`crates/css/src/at_rules.rs`, module comment: "it needs the size of a box, so it waits for
  layout"). `container-type`, `container-name` and `container` are not parsed.
- T3 Code (`client/assets/main-DYJdcEdq.css`, v0.1.12) has 2 containers and 7 queries:
  - `composer-surface` (`container-type: inline-size`): `(width>=36rem)` and `not (width>=320px|420px|480px|560px|640px)`.
  - `diff-header`: `not (width>=30rem)`.
- Effect in bare: the composer footer shows the narrow layout at every width. The rules for the wide layout
  (`@xl/composer-surface:flex`, `ml-auto`, `hidden`) never apply. The `not (...)` rules always apply.
- Tailwind 4 makes `@container` rules from `@sm:` and similar classes, so more pages will have them.
- The spec (CSS Conditional 5, "container queries"):
  - A container has `container-type: size` or `inline-size`. It gets size containment in that axis. So its size does not depend on
    its children, and the browser can know it before it styles them.
  - A query tests the nearest ancestor container with a matching name (or any container when there is no name).
  - Size features: `width`, `height`, `inline-size`, `block-size`, `aspect-ratio`, `orientation`. Lengths in a query use the
    container's own font (`em`). Units `cqw`, `cqh`, `cqi`, `cqb`, `cqmin`, `cqmax` are 1% of the container.
  - No container that matches: the query is unknown, and the rule does not apply.

## Current design

- The cascade (`crates/css/src/cascade.rs` `add_rules`) decides `@media` and `@supports` once, when it builds the rule set. A rule
  in a block that says no is not added.
- A frame: `cascade_frame` (all styles), then `layout_areas`. `refresh` restyles only the elements that a change can reach (marks),
  then lays out again. Layout keeps fragments (`kept`) that the next layout can use again.
- Layout never calls the cascade. Styles are done before layout starts.

## Options

### 1. Style, lay out, then restyle the containers whose answer changed (proposed)

- Data:
  - `container-type` (`normal`, `size`, `inline-size`) and `container-name` (a short list of names) as new properties.
    `container` as a shorthand.
  - Each rule in an `@container` block gets a condition: an index into `RuleSet::conditions`. A condition is a name and a query.
  - `Styles::container_sizes`: `NodeId -> (w, h)`, from the last layout. Only for elements with a `container-type`.
- Algorithm:
  1. The cascade matches a conditional rule only when its condition is true. It finds the nearest ancestor container with the name,
     and reads its size in `container_sizes`. No size yet: false.
  2. Layout runs.
  3. After layout, read the content box of each container. Compare each condition answer with the one the cascade used. Keep a
     bit per (container, condition) pair that the cascade read.
  4. A changed answer marks the container's subtree for restyle. The existing `refresh` path restyles the marked elements and lays
     out again. The `kept` fragments make the second layout cheap where nothing changed.
  5. Repeat for nested containers. The loop ends: each pass fixes the outermost changed containers, because a container's size
     does not depend on its children. The bound is the nesting depth of containers (2 on T3).
- Complexity:
  - Pages with no `@container` pay nothing: no conditions, no loop.
  - A conditional rule costs one ancestor walk per element it is tested on. A cache per container element makes it O(1) after the
    first walk.
  - A second layout pass runs only when an answer changes: the first frame, and a resize across a breakpoint.
- Memory: one map entry per container (T3: 2). A few bytes per conditional rule.
- Expected gain: the T3 composer footer matches Chrome at all widths. On resize, only the subtree under a changed container is
  styled again.

### 2. Style inside layout, as Chrome does

- Layout calls the cascade for the children of a container when it knows the container's size. The style of a subtree waits for
  layout.
- Chrome does this (`StyleEngine::UpdateStyleAndLayoutTreeForContainer`, to verify). It does each element once.
- bare would need the cascade to run per subtree, called from layout, with the borrow of the document split between them. This is a
  large change to the pipeline for 7 rules on T3. Rejected now. Keep it in mind if pages use many nested containers.

### 3. Guess the container size before layout

- Use the parent's width minus margins to answer queries in the cascade.
- Wrong for flex and grid items, whose width layout decides. The T3 composer is in a flex column, so this would be wrong in the
  case we need. Rejected.

## Other engines (to verify in their source before the build)

- Chrome: interleaved style and layout (option 2). `ContainerQueryEvaluator`.
- WebKit: style, layout, then restyle the descendants of containers, with a loop. Close to option 1. To verify in
  `Style::ContainerQueryEvaluator` and `Document::updateStyleIfNeeded`.
- Firefox: Stylo evaluates the query at style time with the size from the last layout, and restyles after layout when it changed.
  Close to option 1. To verify.
- Servo: not supported (to verify).

## Build plan

1. Parse `container-type`, `container-name` and `container`. Size containment in the inline axis for `inline-size`: the container's
   width does not use its content. Check how far bare's layout already honours `contain: inline-size`.
2. Parse `@container` preludes: an optional name, `not`, `and`, `or`, and the size features. Reuse the `@media` feature reader.
3. Conditional rules in the rule set. The cascade tests them with `container_sizes`.
4. The loop after layout in `render`, `render_with` and `refresh`, with the depth bound.
5. Later step, only if a page needs it: the `cq*` units.

## Check

- New test file `crates/css/src/at_rules_container_tests.rs`: prelude parse and the feature answers.
- New test file `crates/bare/src/browser_container_query_tests.rs`:
  - A rule under `@container (width >= 400px)` applies in a 500 px container, and not in a 300 px one.
  - A named query skips a nearer container with another name.
  - Nested containers: the inner one changes width because of the outer query. Both answers are right after one frame.
  - A resize across the breakpoint restyles only the container's subtree.
  - No container: the rule does not apply.
- T3 composer footer in the window (offstage), at 1280 and 600 px, compared with Chrome.
- Cost rows: the `css` and `layout` rows of `cost_rows.txt`. Pages without `@container` must stay inside +0.5%.

## Not known yet

- How much of size containment bare's layout does now. A container whose width depends on its content needs it.
- Whether the `kept` fragments make the second layout cheap enough on T3. Measure the instruction count of the first frame with and
  without the loop.

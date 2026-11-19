# CSSOM, inline style and computed style through the host interface

Status: proposal, for the owner (plan step S0.4, CSSOM). Nothing is built. Base: origin/main `37df5bd1`.

## Problem

The CSSOM has three parts in the browser: `el.style` (`browser_style.rs`, `browser_style_block.rs`), the sheet, rule, list and media objects (`browser_cssom.rs`), and `getComputedStyle` (`browser_computed.rs`, `browser_used.rs`, `browser_css_ns.rs`). Six files, 5,147 lines. Their native state is in the DOM (the `style` attribute text, the cascade). Their engine references are objects that point at each other and at element wrappers. Today they go through the old per-object hook: three `roots`, three `trace` and three `prune` calls.

This page covers what is left after step 3 of `docs/design-lazy-properties.md`, and one cost that the measurement found: the property accessors of `style` and `getComputedStyle` are made on every page start.

## Scope and assumptions

- Another agent builds step 3 now (host-filled objects become owners). I assume it covers the two kinds named in the brief: `Source::Style` (the `CSSStyleDeclaration` objects of `browser_style`, with their `names` and `by_el` edges) and `Source::RuleList` (the `CSSRuleList` objects made at `browser_cssom.rs:389` and `:744`). After it, `browser_style::{trace, prune}` and the list branch of `browser_cssom::{trace, prune}` have left the old hook. If it also does `rules`, `sheets` or `medias`, Option A below shrinks.
- Not covered by that step, so covered here: rule objects (`Rule`), sheet objects (`Sheet`), `MediaList` objects, the declaration-to-rule table (`style_rules`), `by_node`, `adopted`, `doc_list`, and the computed declarations (`Computed::objs`, `kinds`).
- The handler type `H` stays until one PR at the end moves all subsystems (forms design, open question 1). The base set of handler methods (`type_error`, `charge`, conversions, `define_accessor`, `intrinsic`, prototype calls, `proto_from_new_target`) is in `docs/design-host-forms.md`. This page lists only what is more.
- The lazy member method (`Host::lazy_member`, lazy-properties decision 2) exists before this PR.

## Current design

- `Cssom` (`browser_cssom.rs:104`): `State` (`:83`) holds `rules`, `lists`, `sheets`, `style_rules` and `medias` as `IdMap<ObjId, _>`, `by_node: HashMap<NodeId, ObjId>` (the sheet of a `<style>` or `<link>`), `adopted: HashMap<NodeId, Vec<ObjId>>`, `doc_list`, and the prototypes (`protos`, one for each `Kind`). A rule that no script has read is a 12-byte `Entry::Chunk`. A rule object costs about 0.5 KB (585 B measured on a 16 MiB sheet, 187,852 rules, `browser_cssom.rs:21`).
- `roots` (`:114`): the prototypes, the `adopted` sheets, and each `by_node` sheet whose node is in a document or a shadow tree (`in_tree` walks to the root, `:129`). So the cost of `roots` is the number of sheets times the depth, at each collection.
- `trace` (`:137`) probes up to 5 tables for each marked object: rule (its sheet, `style`, `list`, `media`, `parent`), list, sheet (its list), `style_rules` and `medias` (the rule). `prune` (`:165`) does one `retain` with `live` over each table.
- `Style` (`browser_style.rs:112` to `:145`): `objs`, `by_el`, `names` (holds `StrId` values, so strings go through `trace`), `blocks`. Hooks at `:718` to `:743`. The `el.style` object and its wrapper trace each other.
- `Computed` (`browser_computed.rs:203`): `objs: IdMap<ObjId, NodeId>`, `kinds`, and `fixed`. `getComputedStyle` (`:1168`) makes a new object for each call, with no cap other than `max_objects` (`1 << 16`, `:124`). `trace` (`:1337`) gives the element wrapper. `prune` (`:1343`) is a `retain`.
- Limits: `Limit(Host("computed-objects"))` and `Limit(Host("style-objects"))` throw an uncatchable error when 65,536 objects are live (`browser_computed.rs:1181`, `browser_style.rs:447`). The tables are pruned only at a collection, and the old engine collects only between tasks (`gc_due` at depth 0). So one task that calls `getComputedStyle` 65,537 times stops. Hypothesis: no test or row does this. `browser_computed_tests.rs:397` tests the cap with a small value. This is a limit that guards a table, which the repo rules call a design problem.
- Engine calls outside the base set: `heap.peek_own_property` (4 sites, `browser_cssom.rs:255`, `:332`, `:341`, `:349`: "define the interface only if the prototype has no such key"), `heap.set_prototype_of` (`:250`, `:356`), `host_new_promise`, `host_resolve_promise`, `host_reject_promise` (`:1203`, `CSSStyleSheet.replace`), `heap.define_own_property` with a descriptor (`browser_css_ns.rs:37`, the `CSS` accessor), and the `Slot` and `HeapError::NeedsHost` result of the list reads (`:1344` to `:1362`; these go to `Host::indexed_*` with `Item` and `Pending`, already in the interface).

## Uses of the engine

Over the 6 files (import occurrences outside `use` lines, and `it.*` calls by name).

| Item           | Uses | Item              | Uses |
| -------------- | ---: | ----------------- | ---: |
| `value::Value` |  264 | `eval::Error`     |   17 |
| `eval::Interp` |  139 | `heap::HeapError` |   10 |
| `value::ObjId` |   92 | `heap::Slot`      |    7 |

| `it.*` call   | Sites | `it.*` call                                                                          |  Sites |
| ------------- | ----: | ------------------------------------------------------------------------------------ | -----: |
| `charge`      |    41 | `global_object`                                                                      |      4 |
| `type_error`  |    33 | `heap.peek_own_property`                                                             |      4 |
| `define_data` |    14 | `object_prototype`                                                                   |      3 |
| `get_value`   |     9 | `array_prototype`, `heap.set_prototype_of`, `to_js_number`                           | 2 each |
| `heap_error`  |     8 | the 3 promise calls, `array_from`, `define_own_property`, `to_boolean`, `to_js_bool` | 1 each |

Shared helpers add 65 sites: `def_get` 35, `def_method` 18, `add_fn` 9, `define_accessor` 3.

## Measurements

archMachine, origin/main `37df5bd1`, callgrind on a release build with symbols, one run per row. The per-function numbers are self cost of the function that the old hook calls.

| Row                    |   Total | `collect` (incl.) | Old `Host::trace` | `computed::trace` | `style::trace` | `style::prune` | `cssom::trace`, `prune`, `roots` |
| ---------------------- | ------: | ----------------: | ----------------: | ----------------: | -------------: | -------------: | -------------------------------: |
| `style_load churn 1`   |  43.6 M |             18.6% |             2.54% |      49 K (0.11%) |  158 K (0.36%) |  555 K (1.27%) |                                0 |
| `style_load setget 1`  |  56.9 M |             16.8% |             2.80% |      69 K (0.12%) |  268 K (0.47%) |   60 K (0.10%) |                                0 |
| `style_load hidden 1`  |  25.4 M |             16.5% |             2.09% |      43 K (0.17%) |   36 K (0.14%) |            162 |                                0 |
| `form_load gc 1`       | 124.9 M |             17.1% |             2.04% |     123 K (0.10%) |  176 K (0.14%) |            810 |                      640 (roots) |
| `sheets_load linked 1` |  10.2 M |     no collection |                   |                   |                |                |                                  |

Install cost on every page (inclusive, one call at start):

| Row                     |   Total | `browser_style::install` | `browser_computed::install` |       Sum |
| ----------------------- | ------: | -----------------------: | --------------------------: | --------: |
| `sheets_load dynamic 1` | 9.93 M | 1.12 M (11.3%) | 1.08 M (10.8%) | 22.1% |
| `sheets_load linked 1` | 10.2 M | 1.12 M (11.0%) | 1.06 M (10.4%) | 21.4% |
| `style_load hidden 1`   |  25.4 M |            1.14 M (4.5%) |               1.09 M (4.3%) |      8.8% |
| `form_load submit 1` | 44.6 M | 1.12 M (2.5%) | 1.07 M (2.4%) | 4.9% |
| `form_load gc 1` | 124.9 M | 1.12 M (0.9%) | 1.07 M (0.9%) | 1.8% |

Findings:

- After step 3 the per-object probe of this subsystem is `computed::trace`: 0.07% to 0.31% of a row. The `cssom` hooks cost 0 on the rows, because they exit when no CSSOM object exists (`browser_cssom.rs:140`). The gain of moving the rest is small on these rows. It counts for the removal of the old hook at the end (the old hook is 1.5% to 6.4% of the rows of the forms and navigation designs).
- No row makes a CSSOM object. No example reads `cssRules`, `insertRule`, `styleSheets` or iterates a sheet (grep of `crates/bare/examples`). `getComputedStyle` appears only in `style_load` and `sheets_load`, a few times. The cost rule cannot see a change in rule lists or in `getComputedStyle` loops. Hypothesis: styled-components and emotion pages make many rule objects, and each marked object then pays up to 5 probes.
- `style::install` and `computed::install` together are about 2.2 M instructions at the start of every page. `style::install` defines about 250 properties (the comment at `browser_style.rs:644` says 250 names), each with 2 host functions and 1 to 3 accessor keys (`keys_of`, `:172`; its own comment says it "runs for every property when a page starts"). `computed::install` has the same loop (`browser_computed.rs:1303`). A page that never reads `style` or `getComputedStyle` pays all of it. It is 21% to 22% of the three `sheets_load` rows, 8.8% of `style_load hidden` and 1.8% of `form_load gc`. The repo rule puts time to ready first.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine        | Sheet and rule objects                                                                                                         | `getComputedStyle`                                                                       | Accessors for properties                                                                           |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Blink with V8 | GC objects. `CSSStyleSheet` holds its rule wrappers (`child_rule_cssom_wrappers_`) as traced members; a rule holds its parent. | A new `CSSComputedStyleDeclaration` for each call; it holds the node as a traced member. | Generated from the property list; V8 installs them on the template once per isolate, not per page. |
| WebKit        | Reference-counted `CSSStyleSheet` and `CSSRule`; wrappers live through opaque roots (the sheet is the root).                   | A new object for each call.                                                              | Generated attribute table in the class, no per-page work.                                          |
| Gecko         | Cycle-collected `Rule` and `StyleSheet` with wrapper caches.                                                                   | A new `nsComputedDOMStyle` for each call (cached by element in some cases).              | WebIDL bindings: a static property table shared by all pages.                                      |
| Servo         | Few CSSOM objects.                                                                                                             | A DOM struct that holds `Dom<Element>`.                                                  | WebIDL: generated static table.                                                                    |
| Ladybird      | GC cells; each cell visits its edges.                                                                                          | A GC cell with the element.                                                              | Properties are generated; no per-page loop.                                                        |

The common rule: the parent owns or visits the child through the collector. A deleted rule is garbage. The property accessors are a table that is made once, not by a loop at the start of each page.

## Options

### Ownership of rule, sheet, media and declaration objects

#### A. Each object is an owner with `Traced` edges (recommended)

- A `Rule`, `Sheet`, `MediaList` and a rule declaration each get a host key at creation. The owner variant holds what the `IdMap` holds now. The edges (`sheet`, `style`, `list`, `media`, `parent`, `items`) become `Traced` fields. `Host::trace(key)` visits them. `Host::prune(dead)` removes the entries. The five-probe `trace` and the five `retain` loops go.
- `by_node` and `adopted`: the element wrapper owner of PR #700 visits the sheet of its node, and the root owner visits its adopted sheets. The `roots` loop with `in_tree` goes. A sheet that a script holds stays alive and `el.sheet === el.sheet` holds while the entry lives.
- Cost: about 90 instructions for each object made (measured for wrappers), plus 16 bytes in the owner table. A rule object is 0.5 KB, so that is 3% of its memory. The `Rule` record can move into the owner entry, so the table is not larger.
- Keeps today's rule: a rule that leaves its list is garbage unless a script holds it. No fixed cap is needed.

#### B. One owner for each sheet (opaque root)

- A sheet has one key. Its objects (rules, lists, media, declarations) are listed in the sheet's own table. `trace(sheet key)` visits them all. Objects get the key of their sheet.
- Fewer owner entries (one for each sheet). A deleted rule stays alive until the sheet dies, because the sheet visits the whole table. A page that uses `insertRule` and `deleteRule` in a loop (styled-components in speedy mode) then grows without bound. To avoid this, the sheet must drop the entry at `deleteRule`, and that is the state that A keeps for each object anyway.

#### C. Leave `trace` and `prune` of these objects on the old hook, last

- Cost nothing now. The old hook stays, and the 5-probe `trace` stays on every marked object of a page that has any CSSOM object.

### Computed declarations

#### D. An owner for each `getComputedStyle` call

- The object gets a key. Its `trace` visits the element wrapper. `prune(dead)` removes the `objs` and `kinds` entries.
- Cost: about 90 instructions for each call. A call is followed by reads that cost far more than that: `computed_value` is 0.89 M inclusive on `style_load hidden 1` (3.5% of the row) and 0.79 M to 0.93 M on the three `sheets_load` rows. Hypothesis: the registration is under 0.1% of a call with a read. Not measured.
- With the registration, the table no longer needs the 65,536 cap in the engine that can collect in a task (the hybrid collector, design 6.8). In the old engine the cap stays.

#### E. The computed object holds the element in an engine slot

- The object stores the wrapper in an internal slot that the engine traces. The host keeps no table, no owner, no `prune`.
- Needs a new engine feature: host data in an object (`Scope::set_slot(obj, i, v)` and `slot(obj, i)`). The 104-byte header of the old engine has no room (lazy-properties design). The new engine can have it. Cost in the old engine: a side table, which is what `objs` is.

### Install of the property accessors

#### F. Make the accessors at first use through `Host::lazy_member` (recommended)

- `CSSStyleDeclaration.prototype` and the computed prototype are marked lazy. The 250 properties are made when a script first reads a key that is not there, or lists the own keys. A page that never uses them pays nothing. Observable behaviour does not change.
- Expected gain: about 2.2 M instructions at the start of each page, as measured. Hypothesis: a page that uses both pays the same as today plus one fill call.
- This depends on the lazy member PR and on the rule that `Object.getOwnPropertyNames` realizes the members (to verify: the lazy design states this rule only for the `on*` members).

#### G. Keep the eager install

- Nothing to do now. The 2.2 M per page stays.

### Comparison

|     | Work per collection                                              | Work per object                                    | New interface                          | Size   |
| --- | ---------------------------------------------------------------- | -------------------------------------------------- | -------------------------------------- | ------ |
| A   | one trace call for each CSSOM owner; no probes for other objects | about 90 instructions (hypothesis for these kinds) | `has_own` (below)                      | M      |
| B   | one trace call for each sheet                                    | entry in the sheet table; a deleted rule stays     | `has_own`                              | M      |
| C   | up to 5 probes for each marked object when CSSOM exists          | none                                               | none                                   | none   |
| D   | one trace call for each computed object                          | about 90 instructions for each call                | none                                   | S      |
| E   | none from the host                                               | a slot write                                       | `set_slot`, `slot`; new engine feature | L      |
| F   | none                                                             | none                                               | none (uses `Host::lazy_member`)        | S to M |

A is the same design as the owners of wrappers (#700), listeners (#697) and, after step 3, style and rule lists. It is one mechanism for the whole subsystem. B is cheaper in entries but breaks the garbage rule for deleted rules. D and A together remove the last three hook calls of this subsystem. E is the better long-term design for the new engine. It is not needed in S0.

## New interface methods

Beyond the base set (`docs/design-host-forms.md`):

| Method    | Sketch                                                           | Semantics                                                                                | Sites |
| --------- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------- | ----: |
| `has_own` | `(&mut self, obj: Local, key: Local) -> Result<bool, Error<'s>>` | Does the object have an own property with this key? It runs no getter and no proxy trap. |     4 |

`define_own_property` with a descriptor (`browser_css_ns.rs:37`) becomes `define_accessor`. The promise calls (`browser_cssom.rs:1203`) belong to the event-loop design. `Limit(Host(name))` is `Abort::Limit(name)` and needs no method. The `Slot` and `HeapError::NeedsHost` results are `Item` and `Pending`, already in the interface (`browser_sources.rs`).

## Leaves the old hook

After step 3, at `37df5bd1` line numbers:

| Call                                                                      | Line in `browser.rs` | After                                                                     |
| ------------------------------------------------------------------------- | -------------------- | ------------------------------------------------------------------------- |
| `browser_style::roots`, `browser_cssom::roots`, `browser_computed::roots` | `:908` to `:910`     | persistent handles for the prototypes and constructors (about 30 objects) |
| `browser_cssom::trace` (rest after step 3), `browser_computed::trace`     | `:942`, `:943`       | `Host::trace(key)`                                                        |
| `browser_cssom::prune` (rest), `browser_computed::prune`                  | `:969`, `:970`       | `Host::prune(dead)`                                                       |

## Risk to the cost rule

Rows that touch these files: `style_load` (8 rows), `sheets_load` (6), `attr_load`, `platform_load`, and every row with a collection through the 3 hook calls. The measured hook cost of this subsystem is under 0.5% of a row, so the move can only gain on them, and the risk is the cost of registration: `style_load churn` makes 400 style objects (step 3, not here) and `getComputedStyle` is called a few thousand times in `style_load hidden`. Option F changes the start of every row (gain, up to 22% on `sheets_load`); the matrix must also show that no row that uses the accessors loses.

The matrix does not cover rule lists, sheets, media lists, adopted sheets or a `getComputedStyle` loop. Option A is a change to a path with no row. The first PR must add rows.

## Plan

1. Add a `cssom_load` example and rows: rules made and deleted (`insertRule`, `deleteRule`, `cssRules[i]`, `replaceSync`), `document.styleSheets`, adopted sheets, and 10,000 `getComputedStyle` calls. Record the base numbers with callgrind and `instr.sh`. No browser change.
2. `has_own` in `bare-js-api` and the adapter (after the base set PR).
3. Option D: owners for computed objects. A/B on the new rows and on `style_load hidden`.
4. Option A: owners for rules, lists (items), sheets, media and declarations; the wrapper owner visits `by_node` sheets; the root owner visits adopted sheets; remove `roots` loops and the last hook calls. Tests: a deleted rule is collected when no script holds it; a held rule keeps its sheet; `el.sheet === el.sheet` after a collection while a script holds the sheet; an adopted sheet lives with its root; a key that is reused answers for the new object only.
5. Option F, after the lazy member PR: lazy accessors for the style and computed prototypes. A/B on every `sheets_load`, `style_load` and form row.
6. Gates for each: Linux and the Windows VM; the cost matrix within +0.5% under both allocators.

## Open questions for the owner

1. Option A for rule, sheet, media and declaration objects (proposed), B or C?
2. Option D (proposed) or E for `getComputedStyle` objects? E needs an engine feature and fits the new engine better.
3. Is Option F (lazy property accessors) in S0.4 (proposed, after the lazy member PR), or a separate performance step? It is the largest measured gain on this page: about 2.2 M instructions on every page start.
4. The caps `computed-objects` and `style-objects` (65,536 each): keep them in the old engine, and drop them when the new engine can collect in a task (proposed)?
5. Add the `cssom_load` rows before any CSSOM change (proposed: yes, as PR 1)?
6. Does step 3 include rule, sheet and media objects? If it does, this page shrinks to Options D and F.

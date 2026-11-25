# Live `childNodes` and `children`

Date: 2026-11-25. Base: main 18c1f964.

## Problem

- `childNodes` and `children` gave a new snapshot array at each read. The DOM Standard says they are live and `[SameObject]`.
- CKEditor 5 (`HtmlDataProcessor._toDom`) moves the body children of a parsed document into a fragment:
  `while (bodyChildNodes.length > 0) fragment.appendChild(bodyChildNodes[0])`. With a snapshot, `length` never drops, so the loop never ends.
- Microsoft Teams runs this code. A fuel probe put the step-budget aborts in `_toDom` < `toView` < `parse` < `_runPendingChanges`.
- Each read also built a new array of wrappers, so `for (i = 0; i < e.childNodes.length; i++) e.childNodes[i]` was quadratic in allocations.

## Change

- `childNodes` and `children` give the one live list of their node: `Src::Of(node, CHILD_NODES)` and `Src::Of(node, CHILDREN)` in `browser_nodelist.rs`.
- `childNodes` has the `NodeList` prototype. `children` has the `HTMLCollection` prototype.
- The list reuses the cache of the live lists: its ids are valid for one `Document::revision`. A read after a change walks the children again (`kids_s`), and the steps are charged.
- The `of` table keeps one list for each node while the list is alive, so `e.childNodes === e.childNodes`. The collector prunes it.
- `querySelectorAll` stays a snapshot.
- A `NodeList` no longer sets `Dom::wrapper_edges` (`browser_sources.rs`). No node wrapper traces a list. Before, the first list made every node wrapper trace its side tables at each collection. That cost `range_load iter 10` +11.8% in the first version of this change.

## Cost

- `tools/js/cost_matrix.sh -a`, base 18c1f964, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`.
- `attr_load|dataset_load|fragment_load|browser_load|nodelist_load|range_load`: 46 rows. None is over the rule with the final change.
- `fragment_load parse` -3.613% (1) and -4.623% (10): `childNodes` no longer builds an array of wrappers.
- `range_load iter 10`: +11.839% with the first version, -0.014% after the `wrapper_edges` change. The profile showed `browser_frames::trace_node` and `trace_owners` in the collector.
- The largest growth of the final rows: `fragment_load replace 10` +0.216%. The `attr_load` and `dataset_load` rows ran on the first version only (max +0.012%).

## Tests

- New: `crates/bare/src/browser_domparser_move_tests.rs`. Both tests fail on main and pass here.
  - The CKEditor loop over a `DOMParser` document ends after 3 moves, and the fragment has the 3 nodes.
  - `childNodes` and `children` are live and the same object, with the right prototype, `item`, spread and `Array.prototype.slice`.
- Changed: `caps_return_distinct_limit_errors` reads `document.body.childNodes.length`. The list is lazy now, as in Chrome, so the `dom-results` cap applies at the first read.
- `t`: all pass after that change.
- `BARE_RECLAIM_STRESS=1` on `browser_nodelist`: 2 tests fail (`a_held_list_sees_changes_made_to_the_native_document`, `a_borrowed_document_stops_the_script_instead_of_an_empty_list`). They fail on main 18c1f964 too, so this change does not cause them.

## Not tested

- A live Teams run with this change: not yet.
- A page that holds many child lists at once: each alive list keeps its ids, and all ids count against `dom-nodelist-nodes` (4 × `max_results`). Hypothesis: real pages keep few lists alive. Not measured.
- test262 counts: not run. The change has no JS engine code.

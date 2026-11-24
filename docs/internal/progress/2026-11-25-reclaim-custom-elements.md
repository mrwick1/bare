# Reclaim dead trees on a page with custom elements

Date: 2026-11-25. Base: main after #1030.

## Problem

- The reclaim (`browser_reclaim.rs`) did not run on a page that had called `customElements.define`. The custom element module did not list the nodes that it keeps, so no node could be freed safely.
- So on such a page no dead tree went back to the arena. Its text stayed live in the `dom-text` cap.
- Teams defines custom elements. It rewrites the text of style elements again and again. A probe run showed 1 664 detached text nodes with about 67 MB of text, no reclaim run (`runs=0`), and then `Limit("dom-text")` after the shell started to render.

## Change

- `browser_custom::held` gives the nodes that the module keeps as roots:
  - the elements of the construction stacks of the definitions;
  - the element queues of the reactions stack, the backup element queue and the elements that wait for the parser (`held`);
  - the elements with a reaction queue;
  - the form owners of form-associated elements, and the element of each `ElementInternals`;
  - the inert document of the template contents, and the script that runs while the page loads.
- `browser_custom::forget` drops the entries of freed elements from the weak maps: `is`, `unk`, `inert` and `face`.
- `reclaim_walk` no longer returns early when custom elements are on.

## Cost

- `tools/js/cost_matrix.sh -a -g 'attr_load|browser_load mutation|focus_load gc|fragment_load|nodelist_load churn|scheduler_load|style_load churn'`, base a854e2c7, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`.
- 25 rows, mean -0.025%, none over the rule. The range is -0.346% (`scheduler_load timers 10`) to +0.074% (`attr_load churn 10`).
- No row defines a custom element, so no row runs the new walk path. Hypothesis: the scheduler drop is code layout. Not verified.

## Tests

- New: `crates/bare/src/browser_reclaim_custom_tests.rs`.
  - A page with a definition rewrites the text of a style element in 400 tasks. It fails on main with `Limit("dom-text")` and passes here.
  - Custom elements, customized built-ins and their callbacks under reclaim stress with poison. It fails on main (no reclaim runs) and passes here.
- `BARE_RECLAIM_STRESS=1` on the tests that match `custom` or `face`: 57 pass.
- `t`: all pass.

## Not tested

- A live Teams run with this change: not yet.
- An element with `ElementInternals` stays for the life of the page: the `ints` map has no entry removal. This is unchanged, and it is small.
- test262 counts: not run. The change has no JS engine code.

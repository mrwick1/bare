# CSSOM edits of a style element without DOM text

Date: 2026-11-25. Base: main after #1032. Design: `docs/internal/design-cssom-style-sheets.md`, option A.

## Problem

- `insertRule`, `deleteRule` and rule edits on the sheet of a `<style>` element wrote the whole sheet text back into the element (`set_text_content`).
- Each write added the full sheet to the DOM text list. Fela on Teams calls `insertRule` about 1 530 times in one task. That made about 66 MB of dead text, O(n²) work, and then `Limit("dom-text")`.
- The writes were also visible to the page: `textContent` changed and a `MutationObserver` got records. Chrome, Firefox and Ladybird do not do this.

## Change

- A `Sheet` of a `<style>` element now has `owned: Option<Vec<(NodeId, Span)>>`. It holds the text key of the element when the CSSOM took over the rules.
- Insert, delete and rule edits edit the sheet in place, as for a constructed sheet (`in_place`). They do not write the element text.
- `publish_rules` records the rule spans and sets `owned`. It does not write the element.
- `refresh` keeps the edited rules while the element text is unchanged. A new element text replaces the edits, as in the specification.
- The style planner (`browser_computed.rs`) reads the sheet text through `browser_cssom::owned_text` when `owned` matches the element.
- The reclaim keeps the owned nodes (`held`) and moves their spans (`pins`).
- Other readers of `<style>` text: `cascade::sources` in `layout/src/scroll.rs` and `paint/src/hit.rs` is used only by tests on parsed HTML. `main.rs` is the static CLI without JS. No CSS web-font loader reads the text.

## Cost

- `tools/js/cost_matrix.sh -a -g 'cssom_load|sheets_load|style_load'`, base 43e724f0, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`.
- 22 rows, none over the rule. Mean -4.906%.
- `cssom_load rules 1` -52.417% and `cssom_load rules 10` -56.980%: the inserts no longer write and parse the element text.
- The largest growth is `style_load attrs 10` +0.316% and `cssom_load sheets 10` +0.268%. Hypothesis: code layout, because these rows do not insert rules into a style element. Not verified.

## Tests

- New: `crates/bare/src/browser_cssom_owned_tests.rs`. All 4 tests fail on main and pass here.
  - `insert_rule_keeps_the_element_text_and_applies_the_rule`: also checks that no MutationObserver record comes.
  - `delete_rule_and_rule_edits_keep_the_element_text`.
  - `a_new_element_text_replaces_the_edited_rules`.
  - `many_inserts_in_one_task_add_no_dom_text`: 2 000 inserts, each reads `cssRules[i]`. DOM text grows by less than 2 000 × 64 bytes. Debug build: 24 s on main, 0.07 s here.
- Changed old tests:
  - `a_rule_declaration_is_made_when_a_script_needs_it` checks computed style and an unchanged `textContent`.
  - `a_style_read_during_serialization_keeps_pending_rule_spans_valid` expects `0|true|.other|rgb(255, 0, 0)`. The insert serializes no rule now, so the page getter does not run (as in Chrome).
  - `keyframes_font_face_and_import_rules_have_interfaces` and `media_and_supports_rules_expose_nested_rules` read `cssText` instead of `textContent`.
- `cargo test -p bare browser_cssom`: 24 pass, also with `BARE_RECLAIM_STRESS=1` and `BARE_RECLAIM_STRESS=lazy`.
- `t`: all pass.

## Not tested

- A live Teams run with this change: not yet.
- test262 counts: not run. The change has no JS engine code.

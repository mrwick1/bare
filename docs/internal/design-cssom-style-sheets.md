# CSSOM edits of a `<style>` sheet without DOM text

Date: 2026-11-25. Status: option A approved by the owner on 2026-11-25.

## Problem

- Teams stops with `Limit("dom-text")` after its shell starts to render. Its CSS-in-JS library (Fela) calls `sheet.insertRule(rule, index)` once for each new rule of a `<style>` element.
- In bare, each `insertRule`, `deleteRule` and rule edit of a `<style>` sheet calls `publish_rules` (`crates/bare/src/browser_cssom.rs`). It serializes the whole sheet and sets it as the `textContent` of the `<style>` element.
- Measured on a live Teams run (one login, probe build):
  - About 1 530 `insertRule` calls in one task. Each call wrote the whole sheet again, up to 334 KB.
  - 66.4 MB of text went into replaced text nodes in that task. 1 529 of the 1 530 nodes have no wrapper.
  - The DOM text list reached the 64 MiB cap in that task, before a reclaim could run between two tasks.
  - The Rust backtrace at the cap: `set_text_content_s` ← `publish_rules` ← `browser_cssom::insert`.
- The cost of n inserts into one sheet is O(n²) in CPU and in memory:
  - `publish_rules` builds the text of all rules: O(n) for each edit.
  - `refresh` reads the element text and compares it with `source` before each edit: O(n) for each edit.
  - `set_text_content` copies the text into the DOM text list: O(n) bytes for each edit, dead at once.
  - The style planner reads and parses the whole new text at the next restyle.
- It is also wrong. In the CSSOM spec, `insertRule` changes the CSS rule list only. The text of the `<style>` element stays the same. Chrome, Firefox and Ladybird give the old `textContent` after `insertRule`. In bare, `textContent` changes and a `MutationObserver` on the `<style>` element gets a record for each `insertRule`. A page can see the difference. Verified on main a854e2c7 with a test that was not kept: `<style>p{color:red}</style>`, one `insertRule('a{color:blue}', 1)`, then `textContent` is `"p{color:red}\na{color:blue}\n"`, and the observer got 1 record.

## Current design

- `State::sheets` maps each `CSSStyleSheet` object to a `Sheet`: the owner node, the rule list object, `source` (the text that the rules were cut from) and `text_key`.
- A rule that no script has read is a span of `source` (`Entry::Chunk`, 12 bytes). A rule that a script read is a rule object (`Entry::Obj`).
- Constructed sheets (`new CSSStyleSheet()`) do not use the DOM:
  - `source` is the text that is in use. An edit marks the sheet `dirty` and moves `sheet_gen` (`chunks_changed`).
  - `flush_chunks` builds the text again only when a reader needs it.
  - The planner reads the text through `browser_cssom::adopted`.
- `<style>` sheets use the DOM text as the text that is in use:
  - The planner reads `Source::Inline(text)` from the element (`browser_computed.rs`).
  - So each edit must write the element text (`publish_rules`).

## How other engines do it

- Ladybird (`Libraries/LibWeb/CSS/StyleSheetState.cpp`, read 2026-11-25):
  - `insert_rule` parses one rule and inserts it into the rule list of the native sheet (`CSSRuleList::insert_a_css_rule`).
  - `rule_inserted` drops the rule cache of the scope. For a style rule or a keyframes rule in a `<style>` sheet, it does no more than that.
  - The element text is not written.
- Chrome (to verify in source):
  - `CSSStyleSheet::insertRule` changes the `StyleSheetContents` of the sheet. A sheet that is shared by more than one element is first copied (copy on write).
  - The `RuleSet` of the sheet is built again at the next style update, not at each insert.
  - The element text is not written.
- Firefox (Stylo, to verify in source):
  - The rule goes into the `CssRules` of the sheet. The stylist marks its cascade data as stale.
  - The data is built again at the next restyle. The element text is not written.
- So the three engines do the same thing:
  - The parsed rule list of the sheet is the truth after a CSSOM edit, not the element text.
  - An edit costs O(rule) plus a mark. The cost of the index is paid once at the next style update.

## Options

### A. The CSSOM sheet is the text that is in use (recommended)

- An edit of a `<style>` sheet takes the path of a constructed sheet:
  - It changes `list.items` and `source` in place (`chunks_changed`), marks the sheet `dirty`, and moves `sheet_gen`.
  - It does not write the DOM.
- A new flag on `Sheet`, `owned: Option<TextKey>`. After the first CSSOM edit, it holds the `text_key` of the element at that time.
- The planner (`browser_computed.rs`, `Source::Inline`):
  - If the `<style>` element has an owned sheet and its `text_key` is unchanged, the planner uses `source` (after `flush_chunks`).
  - Else it uses the element text.
- When a script changes the element text, the `text_key` changes. As the spec says ("update a style block"), the element gets a new sheet from its text. The CSSOM edits are gone. `refresh` drops the owned flag and cuts the new text.
- `refresh` of an owned sheet compares the `text_key` only: one compare for each text child, not the whole text.
- Cost:
  - n inserts in one task cost O(n) spans plus one `flush_chunks` of O(text) at the next restyle.
  - No DOM text, no dead text nodes, and no mutation records.
  - The planner still parses the whole sheet text once for each restyle after an edit. This is the same as now.
- Memory: one copy of the sheet text in `source`. Today there are two (`source` and the DOM text list) plus the dead copies.
- Expected gain on Teams: about 66 MB less DOM text, and about 1 530 fewer full serializations and text compares. The `dom-text` stop goes away (hypothesis: it is the only producer of the growing text in that task).

### B. Incremental rule insertion into the parsed sheet

- The planner keeps the parsed rules of each sheet (a `bare_css` sheet), not only its text. `insertRule` parses one rule and inserts it at its index. The cascade index of the sheet is built again at the next restyle, as in Chrome.
- Cost: O(rule) for each insert, and no text parse of the whole sheet at the next restyle.
- It needs the planner to keep parsed sheets across restyles, with a cache key. Today it works from text (`Plan.parts`). This is a larger change in `bare_css` and `browser_computed.rs`.
- It can follow A: A makes the edit cheap and correct. B then also removes the parse of the whole sheet for each restyle.

### C. Keep the DOM text, append only

- `insertRule` at the end appends one text node to the `<style>` element, and does not set `textContent`.
- Rejected: it still writes the DOM, so the mutation records and `textContent` stay wrong. An insert in the middle (Fela inserts by score index) still needs the whole text.

## Plan for A

1. Tests first, in a new file `crates/bare/src/browser_cssom_owned_tests.rs`:
   - After `insertRule`, `textContent` of the `<style>` element is unchanged, and a `MutationObserver` gets no record.
   - The inserted rule applies: `getComputedStyle` sees it.
   - 2 000 `insertRule` calls in one task stay under a small `dom-text` cap. Test mode counts `publish` work (the `WORK` counter) and checks that it stays O(n).
   - A script that sets `textContent` after `insertRule` gets a sheet from the new text. The CSSOM rule is gone, as in Chrome.
   - `deleteRule`, `selectorText` and `style` edits of a `<style>` sheet: the same checks.
2. Build A. Remove the `<style>` branch of `publish_rules` that writes the DOM.
3. Cost rows: `cssom_load` (all), `sheets_load dynamic`, `style_load`. Write the progress file.
4. A live Teams run: check the shell renders and the `dom-text` stop is gone.

## Not covered

- `<link>` sheets: an edit stays a gate (`editing an external stylesheet through CSSOM`). No change.
- Option B: a later design, after A is measured.

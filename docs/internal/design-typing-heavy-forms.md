# Design: typing into heavy React forms

Status: option B is built (branch `fill-insert-text`). The owner chose it on 2026-11-16. Results: section 8.

## 1. Problem

The `type` step costs 100 to 250 ms of CPU for each character on the editor page of a private React dashboard. A simple form on the same app costs 7 to 8 ms. An agent that fills 4 fields waits about 15 s. The page is busy for that time, so other work waits too.

## 2. Method

- Machine: archMachine, shared. Main `00e701b0`. A release build with symbols and frame pointers (`/data/bare-tmp/target-typing`).
- Journey: a step file outside the repository (it holds private names). It logs in, opens the editor, picks an option, and types into 2 large fields (230 keys in total).
- Corpus: a private corpus outside the repository, recorded live once. Replay with `BARE_NET_REPLAY`, `BARE_CLOCK=virtual`, `BARE_SEED=1`. 1 miss for each run: a dashboard poll after login. The form does not use it.
- Instructions: `perf stat -e instructions:u`. Subsystem split: `perf record --call-graph fp` and `/data/bare-tmp/r2/a/classify.py`. JS split: the R2-A counting profiler, rebased on main in `/data/bare-tmp/typing-prof` (a prototype, not for the repository).
- Live page checks: bare MCP `eval` on the live dashboard.

## 3. Measured

| Item | Value |
|---|---|
| Whole journey | 77.455 to 77.458 G instructions over 3 runs (spread 0.004%) |
| Same journey, first large field of 1 key instead of 47 | 54.455 to 54.457 G over 2 runs |
| Cost of one key | (77.456 - 54.456) / 46 = 500 M instructions (estimate from measured numbers) |
| CPU for one key in the step report | 102 to 248 ms |
| Fibers on the editor page | 1,232. The input is 110 levels deep. DOM elements: 271 |
| React render passes (`workLoopSync`) for the journey | 341: about one for each key |
| `performUnitOfWork` calls for the journey | 241,528: about 1,000 fibers for each key |
| Events for one key | `keydown keypress beforeinput input selectionchange keyup`: the same list as Chrome |
| DOM mutations for one key | 15 |

Subsystem split of the journey:

| Subsystem | Share |
|---|---|
| JS interpreter (tree walker) | 88.6% |
| GC | 5.8% |
| JS builtins | 1.9% |
| `for-in` | 1.4% |
| JS parse | 0.7% |
| CSS, DOM bindings, HTML, net, layout | 1.4% together |

JS split: React reconcile and commit functions take the cost. No single function takes more than 8.4% (`markUpdateLaneFromFiberToRoot`, 6.5 G). The cost is flat.

Interpreter speed check on the live page:
- A walk of the 110 fiber parents costs about 350 ns for each level (2 reads and an add).
- A walk over plain objects costs the same.
- So the fiber objects have no shape or IC problem.

## 4. Cause

React renders once for each key, as it does in Chrome. Each render visits about 1,000 fibers, because the form state lives high in the tree. bare does not add events or renders. The cost is the base speed of the tree walker: about 500 K instructions for each fiber for each key.

Hypothesis, not measured: Chrome does the same render in 1 to 3 ms. The gap is about 50 to 100 times, the normal gap between a tree walker and a JIT.

## 4a. How Obscura does it

Source: github.com/h4ckf0r0day/obscura, commit `0d9a2f7` (2026-11-14). Read at `/data/bare-tmp/ref/obscura`.

Obscura removes the cost in two ways.

1. The JS engine is V8 (`deno_core` 0.412, `crates/obscura-js/Cargo.toml`). The JIT makes each JS operation cheap, so a render of 1,000 fibers is cheap. bare has no JIT. Its bytecode lane runs behind `BARE_JS_EXEC=byte`, but it does not help this journey yet (section 5A). So this way is closed to bare now.
2. Its MCP tools send no keys:
   - `browser_fill` calls the native `value` setter of the element (`__obscura_setFieldValue`, `bootstrap.js:11977`). Then it dispatches one `input` and one `change`. The native setter goes around the value tracker of React, so React sees the change.
   - `browser_type` does the same with the old value plus the new text, and then one `input`.
   - `crates/obscura-mcp/src/lib.rs:1099` and `:1126`.
   - The events are synthetic. `__obscura_markTrusted` makes them look trusted.

The CDP path is different:
- `Input.insertText` is one `beforeinput` and one `input` with `inputType` `insertText` (`crates/obscura-cdp/src/domains/input.rs:407`). Playwright `fill()` uses this call.
- `Input.dispatchKeyEvent` sends one key at a time. Playwright `pressSequentially()` uses it.

What bare takes from this: option B, as in the CDP `insertText` path. bare does not take the value setter of the MCP tools. It skips `beforeinput`, so a page cannot cancel the edit. It also needs events that only look trusted. That is the direct injection that engine plan topic 1 forbids.

## 5. Options

### A. Make the interpreter faster

- Bytecode (engine lane B) stopped at the R1 gate on 2026-11-08 (2.49x, the gate was 3x). The owner revived it on 2026-11-12 (`docs/decisions.md`). It runs behind `BARE_JS_EXEC=byte`, and the default stays `tree`.
- Measured on this journey (2 runs each, main `00e701b0`): `byte` 79.245 to 79.246 G, `auto` 79.232 to 79.233 G, `tree` 77.456 G. So `byte` costs 2.3% more here. Hypothesis: most React functions do not run in the bytecode loop (lazy functions, refused forms) and go to the tree walker.
- Even 3x gives 35 to 80 ms for each key on this form, and the work stays O(keys x fibers).
- Shape and IC fixes (lane A) are merged. The miss rate is 3.8%.
- Gain on this journey: at most 2.5x. It does not remove a kind of work.

### B. A `fill` step: one edit for the whole text

This is what Playwright `fill` does:
1. Focus the field.
2. Select its text.
3. Insert the whole text as one edit: one `beforeinput` and one `input` with `inputType` `insertText`. A paste or an IME commit gives the same events.

There are no key events. React renders once for the field, not once for each character.

- Cost: O(fibers) for each field, not O(keys x fibers). On this form: about 500 M instructions for each field, not 500 M x 56.
- Gain on the journey (estimate): the 121 keys in the large fields cost about 60 G (121 x 500 M), and the 75 keys in the small fields cost about 9 G. With `fill`, these become 7 edits of at most 500 M each, about 3.5 G. The rest of the journey is about 8 G. Total: about 77.5 G to about 11 G (7x).
- Memory: no change.
- Risk: some fields need keys, for example autocomplete, input masks, and handlers on `keydown` or `keyup`. `type` stays for them.
- Rule from engine plan topic 1: "No direct token injection." `fill` uses the normal edit path of the browser: `beforeinput` can cancel it, and `input` fires. It does not set `.value` from outside. To verify: that the owner reads the rule this way.

- Today the MCP `act` tool takes `fill`, but it maps it to `type` (`crates/mcp/src/main.rs:594`). So an agent that asks for `fill` gets keys. B makes the name do what it says.

### C. Keep the key events, but collapse the renders

Send all the key events, but let React render only once. That is not possible without changes to the page. React renders discrete events synchronously at the end of each event. A browser cannot defer that and stay correct.

Not chosen.

### D. A cheaper walk of React's parent chain

Patch the cost of the walk in `markUpdateLaneFromFiberToRoot`. That function is 8.4% of the cost. This is the hot-spot patch kind that `CLAUDE.md` says to avoid.

Not chosen.

## 6. Recommendation

Build B: `fill LOC "text"` in the step language and in the MCP tools. It uses `Input.insertText` of Chrome as its model.

- `type` stays for fields that need keys.
- The MCP tool doc says to prefer `fill`, and `type` for a field with keys.

Tests to write before the code:
- A React-controlled input.
- A field whose `beforeinput` cancels the edit.
- A `maxlength` field.
- A `textarea`.
- A contenteditable.
- A field that the page replaces on focus (the Wikipedia case of `type_start`).
- `fill ""` clears the field.

Benchmark target: this journey with `fill` in all form fields. Expected 77.5 G to about 11 G (estimate in option B). Run 3 times, rebase on the newest main, and run again before merge.

## 7. Not done

- No Chrome measurement of the same render.
- The lost-characters bug of `type` right after a dialog opens is a separate problem. It is not covered here.
- Only one form was measured. The client form (7 to 8 ms for each key) was not profiled.

## 8. Results (built)

Code:
- `Dom::insert_at_focus` and `Browser::ui_insert_text` (`browser_ui.rs`). A field goes through `edit` with `insertText`, the path that each typed key already used. An editing host goes through `insert_text` of `browser_editing.rs`.
- `Page::fill_text` (`control.rs`). It is `type_start` (the checks, the click, Control+A; "" clears), then one `insert_text`.
- The `fill` step (`crates/drive`). The MCP `act` op `fill` sends a `fill` step now. Before, it sent `type`.

Tests, written before the code. They failed first.
- `browser_drive_tests.rs`, 7 tests:
  - one trusted `beforeinput` and one `input`, and no key events
  - the edit replaces the selected text
  - a canceled `beforeinput` stops the edit
  - `maxlength`
  - line feeds in a `textarea`
  - a contenteditable host
  - no editable focus does nothing
- `crates/drive/tests/fill.rs`, 4 tests: one `input` and no keys, `fill ""` clears, a `textarea`, and a field that the page swaps on focus.
- `steps_parse`.
- `act_fill_inserts_the_text_as_one_edit_with_no_keys`. Before the change, `act fill` gave 33 key events and 11 `input` events for 11 characters.

Benchmark: the journey of section 2, 3 runs each, the same flags. The base is main `00e701b0`.

| Build and steps | Instructions |
|---|---|
| main, `type` | 77.454 to 77.458 G |
| branch, `type` (no change expected) | 77.455 to 77.457 G |
| branch, `fill` in the 6 form fields | 10.020 to 10.021 G |

`fill` uses 7.7 times fewer instructions. Section 5 estimated 11 G. Each field is one tick now:

| Field | CPU before | CPU after |
|---|---|---|
| Stage goal | 3,392 to 3,772 ms | 95 ms |
| Success criteria | 4,344 to 4,795 ms | 302 ms |
| Stage 2 name | 1,716 to 2,363 ms | 209 ms |

### `fill LOC "text" keys=last`

A widget that waits for a key (autocomplete) sees no key from `fill`. `keys=last` inserts all characters but the last as one edit. Then it types the last character as one key (`type_char`; a line feed is Enter). The page renders about twice for each field.

- Code: `Page::fill_text` takes `last_key`. The step word is `keys=last`. The MCP `act` op `fill` takes `last_key: true`.
- Tests: 2 `bare-drive` tests (a text of 5 characters: 3 key events and 2 `input` events; a text of 1 character: 3 key events and 1 `input`), 2 parse cases, and a part of the MCP test.
- Benchmark: the same journey with `keys=last` on the 6 `fill` steps. 3 runs: 12.165 to 12.166 G. A new build of plain `fill` gave 9.655 G (1 run). The old build gave 10.020 G. Hypothesis: the new build has a different code layout. So `keys=last` adds about 2.5 G, about one key for each field. It uses 6.4 times fewer instructions than `type`.
- It does not help an input mask that checks each key. `type` stays for it.

Not done:
- No Chrome log of `Input.insertText` on these fixtures. The event list follows the Input Events spec and the comment in Obscura (`input.rs:405`). To verify: record it with `tools/ui/chrome_drive.mjs`.
- No rebase on a newer main yet.
- Not measured: the change on the other 47 journeys. They do not use `fill`, and the `type` path did not move on this journey.
- The test failures that main also has: `chunked.rs` (2), `survive_limit.rs` (1), `a_snapshot_of_5000_elements_takes_under_30_ms`. `a_pending_meta_refresh_is_queued_work` failed once under load and passes alone.

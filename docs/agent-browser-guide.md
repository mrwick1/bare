# Agent browser guide

This guide shows how an agent drives `bare-mcp`. Each example uses tools and arguments that exist on this base. `crates/mcp/tests/agent_guidance.rs` runs them against `tools/fixtures/agent-guide/index.html` (loopback, synthetic data).

## The loop

1. **Open.** `open {"url": "http://127.0.0.1:8080/"}` starts the live session `main`. Add `"snapshot": true` to get the outline in the same reply.
2. **Inspect.** `snapshot` gives the outline with refs (`[e3] button "Accept notice"`). `find {"role": "button", "name": "Accept notice"}` gives only the matches.
3. **Act.** `act {"op": "click", "ref": "e3"}`, or use a semantic locator: `act {"op": "type", "loc": "label=\"Nickname\"", "text": "Ada"}`.
4. **Verify.** A receipt that says `ok` says the input was delivered. It does not say the business result happened. Check the page: `step {"steps": "expect text=\"Saved Ada\" visible"}`, `read`, `links`, or `screenshot` for visual evidence.
5. **Recover.** On a failure, read the error and its hint, then `snapshot` or `read`. Retry a side effect (a submit, a purchase) only after the page shows it did not happen.

Use `read` for the text of the page or of one element (`ref`), and `links` for URLs (filter with `text`, `root`, `external`). Use `screenshot` only when text does not explain the page. Use `eval` to read a value. Do not use it to click or type: it skips the user input path.

## Sessions and saved state

- A **live session** is a running page. The name is `session` (default `main`). Sessions run side by side: `open {"url": ..., "session": "other"}`. A new `open` of the same name ends the old page. `close {"session": "other"}` ends one. `sessions` lists them.
- A **saved login** is a name for cookies and storage. `save_state {"name": "shop"}` stores it in memory (add `file` to also write a private file that holds live credentials). `open {"url": ..., "state": "shop"}` starts a new live session from it. Memory ends with the server.
- A public page needs only `open`. Use `ensure_session` for an authenticated journey: it restores the saved name, runs `check`, and runs `login` only when the check fails. The page stays open for `step`.
- `open` shares the HTTP cache with earlier opens. Add `"fresh": true` for a clean network client.

## Traps

| Situation | What happens | What to do |
|---|---|---|
| Ref lifetime | Refs stay valid until the page navigates. | `snapshot` or `find` again after a navigation. |
| Duplicate target | `FAILED duplicate: ... matches 2 elements`, with the hint to use a ref. | Take the ref from `find`, or narrow the locator. |
| Overlay | `FAILED covered: input#a is covered by div ...`. | Close the overlay first (its button, or `press` Escape), then repeat the act. |
| Delayed result | The page changes after the receipt. | `step` with `expect ...`. It waits until its time limit. |
| Field text | `act type` clears the field, then types. `\n` is Enter. | Do not clear it yourself. |
| Submit | `press` Enter with `ref` or `loc` in a form field submits the form. | Verify the result, not the key press. |
| select / check / upload | `select` takes a label or value (`options` for a multiple select). `check` clicks only when the state differs. `upload` takes `files`. | Verify with `expect LOC checked` or `expect LOC value~"..."`. |
| Dialogs | A dialog that `act` opens is accepted by default. Use `"dialog": "dismiss"` to cancel. The receipt lists each one. | Check the receipt. A dialog from a timer is always dismissed and is listed on the next reply. |
| New tab | `opened tab: main~2 URL`. It is a new session. | Use `session: "main~2"`, then `close` it. |
| Downloads | A download does not replace the page. The receipt names the file. | `downloads` lists them. |
| Secrets | Put credentials in `secrets_file` (`NAME=value`) and use `${NAME}` in step lines. Known secrets show as `[secret]`, and `screenshot` is refused while one shows. | Never put a credential in a step text, file or report. |
| Long output | `snapshot` keeps 12000 characters by default and says what it cut. `read` returns pages of 8000. | Use `offset`, `root`, `max_chars`, or `find`. |
| Time limit | `act` waits as long as the `timeout_ms` that `open` set. `act` takes no `timeout_ms`. | Set `timeout_ms` on `open` (or `timeout MS` in `step`). |

## Examples that run

### Open, find, act, read

```json
{"tool": "open", "arguments": {"url": "http://127.0.0.1:PORT/", "snapshot": true}}
{"tool": "find", "arguments": {"role": "button", "name": "Accept notice"}}
{"tool": "act", "arguments": {"op": "click", "ref": "e3"}}
{"tool": "read", "arguments": {}}
```

Evidence: the notice text is gone from `read`. Tests: `open_find_act_read_and_an_overlay_is_closed_before_the_page_is_used`.

### Fill a form and verify

```json
{"tool": "act", "arguments": {"op": "type", "loc": "label=\"Nickname\"", "text": "Ada"}}
{"tool": "act", "arguments": {"op": "select", "loc": "label=\"Colour\"", "text": "Blue"}}
{"tool": "act", "arguments": {"op": "check", "loc": "label=\"Terms\""}}
{"tool": "act", "arguments": {"op": "press", "loc": "label=\"Nickname\"", "key": "Enter"}}
{"tool": "step", "arguments": {"steps": "expect label=\"Nickname\" value~\"Ada\"\nexpect label=\"Terms\" checked\nexpect text=\"Saved Ada\" visible"}}
```

Evidence: `step` returns `"ok": true`. Test: `a_form_is_filled_and_the_result_is_verified_with_step_expect`.

### Delayed data

```json
{"tool": "act", "arguments": {"op": "click", "loc": "role=button name=\"Load report\""}}
{"tool": "step", "arguments": {"steps": "expect text=\"Report ready: 3 rows\" visible"}}
```

A failed `expect` leaves the session open. Test: `a_delayed_result_is_awaited_with_expect`.

### Overlay and duplicate recovery

`act {"op": "check", "loc": "label=\"Terms\""}` fails with `covered` while the notice shows. Close the notice by ref, then repeat. Test: `a_covered_target_and_a_duplicate_locator_are_errors_with_a_hint`.

### A second session

```json
{"tool": "open", "arguments": {"url": "http://127.0.0.1:PORT/?second", "session": "other"}}
{"tool": "read", "arguments": {"session": "other"}}
{"tool": "close", "arguments": {"session": "other"}}
```

Test: `a_named_second_session_runs_beside_main`. `the_tools_and_arguments_of_the_guide_exist` checks the tool and argument names of this guide against `tools/list`.

## When bare is wrong

If a step fails and the page looks fine in a real browser, call `diagnose`. It gives a verdict: site problem, bare gap, network or unclear. Report a bare gap as the server instructions say (GitHub issue by default, or offstage `report_issue` when `BARE_MCP_REPORT=offstage`). Give the tool call, the result and the expected result.

## Pending, not on this base

These are not documented as current and are not tested here.

- PR #614 adds structured outcomes (`action_status`, `page_status`, `verification`) and an optional `expect` argument of `act`. On this base use `step` with `expect` as shown above. `the_tools_and_arguments_of_the_guide_exist` fails when `act.expect` appears, as a reminder to update this guide.
- Other planned sibling work (new observations, error codes, cancellation, document revisions) is not described here.

# Agents

`bare` is built to be the browser of an AI agent. Two programs give it an interface for agents.

## bare-drive

`bare-drive` runs a **step file**: one step on each line, such as `open URL`, `click`, `type`, `press`, `expect`, `save-state` and `goto`. It finds elements as a user names them (by role and name, label, text or CSS) and waits for conditions with a time limit. Each click and key goes through the same hit testing and input path as a user in the window. The result is JSON with the console, the network requests and captures. Secrets given with `--secrets` show as `[secret]`.

- Headless by default. `--window` shows the page in a Wayland window.
- `--suite DIR` runs a suite of related tests that share one login in K warm worker processes. A worker keeps its network client and its parse caches between tests, and each test starts with clean storage.
- `--repeat N` runs the steps N times in one process, to find leaks.
- `--profile NAME` keeps the cookies and `localStorage` in a profile on disk, so the next run opens logged in. A value with a path separator is a path. The default is the profile `default` (`~/.local/share/bare/profiles/default` on Linux). When another process holds it, the run is in memory and says so on stderr. An empty `BARE_DATA_DIR` turns profiles off.
- A **journey** is a step file that follows a real task, such as a login or a search. `tools/drive/public/` holds 48 journeys on public sites. `tools/drive/staging-journey.steps` is a login journey with create and delete steps.

## bare-mcp

`bare-mcp` is an MCP server over stdio. It uses `bare-drive`, so an agent gets the same input path. Its tools:

| Tool | Purpose |
| --- | --- |
| `open`, `close`, `sessions` | Start, end and list live sessions. Sessions run side by side. |
| `snapshot`, `find`, `read`, `links`, `screenshot` | Inspect the page: an outline with element refs, matches by role and name, text, URLs, a picture. |
| `act`, `step` | Click, type, select, check, press and upload. `step` runs more step lines and `expect` lines. |
| `eval`, `console`, `network`, `storage`, `downloads` | Read page state, logs, requests, storage and downloads. |
| `ensure_session`, `save_state` | Keep a logged-in state in memory or in a file, and log in again only when a check fails. |
| `run_steps`, `run_suite` | Run a whole step file or a suite. |
| `diagnose` | Say whether a failure is a site problem, a gap in `bare`, or a network problem. |

`act` returns `action_status`, `page_status` and `verification`. A completed action does not prove that its business result happened, so add an `expect` line to check the page. See [the action outcome contract](design-agent-outcomes.md).

The loop is: open, inspect, act, verify, recover. [The agent guide](agent-browser-guide.md) gives the details and examples that its tests run.

## Who uses it

- [`deck`](https://github.com/mrwick1/deck), a self-hosted AI agent in a web app, starts `bare-mcp` for its browser tool calls.
- [`offstage`](https://github.com/mrwick1/offstage), a caged desktop for AI agents, installs `bare-mcp` and `bare-drive` from a release and gives their tools as `browser_*`.

Both use `bare` as their only browser. Each release carries `bare-mcp` and `bare-drive` for Linux amd64, Linux arm64 and Windows amd64.

## Reporting a problem

An agent that uses `bare-mcp` reports each problem it finds. In `offstage`, use `report_issue` with area `bare`. Elsewhere, file a GitHub issue on `mrwick1/bare` with the label `bare-mcp`. Give the tool call, the result and the expected result.

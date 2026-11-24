# Agent interface: one browser, one agent front door

Status: draft for owner review. No code is built yet.

## Goal

- `bare` is a standalone headless browser for people. A browser app for a small group of people is a possible later goal.
- People drive `bare` through a standard protocol: WebDriver BiDi. MCP is not the public interface of `bare`.
- `offstage` is the only way an agent reaches `bare`. `deck` is an agent, so `deck` also goes through `offstage`.
- `offstage` installs the newest `bare` release. It does not pin a version.
- Linux and Windows work first. macOS comes later.

## Owner decisions (2026-11-18)

1. MCP is not acceptable as the public interface for people. Build BiDi.
2. `deck` does not need `--workspace`.
3. `offstage` uses the `latest` `bare` release.
4. `deck` uses `offstage`, because `deck` is an agent.

## Current state (2026-11-18)

Three paths lead from an agent to `bare`:

| Path | How | Version |
| --- | --- | --- |
| Standalone registration | Claude and Codex start `~/.local/bin/bare-mcp --workspace 6`. | The installed binary |
| `offstage` | `offstage-mcp` finds `bare-mcp` and `bare-drive` beside its binary or on `PATH`. It starts one `bare-mcp` process for each browser session. It exposes the tools as `browser_*`. | `latest` release, unless `OFFSTAGE_BARE_VERSION` is set |
| `deck` | Starts `bare-mcp` from `PATH` or `[bare] command`. It exposes the tools as `browser_*`, except `run_steps`. It hides some arguments (`window`, `secrets_file`, `file`, `path`, `downloads_dir`, `inline`). It sets `secrets_file` and `state_dir` itself. Source: `deck` `src/tools.rs`, `docs/architecture.md`. | The binary on `PATH` |

Agent-specific parts live in `bare` today:

- The `initialize` text in `crates/mcp/src/main.rs` (`instructions`). It holds the agent loop and the issue route.
- `BARE_MCP_REPORT=offstage` switches the issue route. `offstage` always sets it.
- `--workspace N` and the `hyprctl` window rule. These are Hyprland-only.
- `docs/agent-browser-guide.md`, the agent guide.

Other facts:

- `bare-mcp` is a thin MCP layer (one 110 KB `main.rs`) on the `bare-drive` library. `bare-drive` holds the session, input, locator, snapshot, state and suite code. Its `run_suite` tool starts `bare-drive --suite-worker` processes.
- `offstage` is Go. `bare` and `deck` are Rust. A process boundary stays between them.
- The `bare` README says that `offstage` pins `BARE_VERSION`. The `offstage` installer uses `latest` by default. The code is correct; the README is wrong.
- `offstage/bin/offstage-bare-mcp` says that `.mcp.json` points to it. `.mcp.json` names only `offstage-mcp`. Hypothesis: the file is dead.
- The local `deck` checkout is behind `origin/main` and has a different history. These facts come from `origin/main`.

## Problems

1. Two products own the agent wording. They can disagree.
2. `bare` knows about `offstage` (`BARE_MCP_REPORT`) and about Hyprland (`--workspace`). A standalone browser must not.
3. Two issue routes and two agent guides exist.
4. `offstage` takes `latest`, but no test checks a new `bare` release against `offstage` before the release is published.
5. `deck` and `offstage` each build a browser tool layer. They wrap the same tools in two ways.
6. The duplicate registration gives an agent two sets of browser tools.
7. People cannot drive `bare` with the tools they know.

## Target

```
people ── Puppeteer / Selenium / own client ──┐
                                              ├── WebDriver BiDi (+ bare: extension module) ── bare
agents ── MCP ── offstage ────────────────────┘
deck ──── MCP ── offstage (browser tools only)
```

- `bare` has one external interface: WebDriver BiDi, plus an extension module for what BiDi does not have.
- `offstage` owns everything agent-facing: tool names, descriptions, the agent loop text, the skill, issue routing, screens, window placement and install.
- `bare-mcp` goes away at the end.

## Options for the agent path

### A. offstage keeps `bare-mcp` as an internal link

`bare-mcp` stays, but only `offstage` uses it. `bare` loses the agent-ops parts. BiDi is a second front end for people.

Cost: small now. Later, two front ends in `bare` to keep in step.

### B. offstage speaks BiDi to bare

`offstage` drives `bare` through BiDi and the `bare:` extension module. It turns these into MCP tools. `bare-mcp` is deleted.

Cost: large. `offstage` needs a BiDi client in Go and must own the tool schemas.
Gain: one interface in `bare`. Each agent feature is also open to people. The BiDi code gets daily use from agents, so it stays correct.

### Recommendation

B as the end state. A as the bridge until the BiDi server can carry all the tools that `offstage` uses. No user change is visible at the switch.

## BiDi in bare

### Scope

WebDriver BiDi is a W3C draft. It defines modules with commands and events. First modules, mapped to what `bare-drive` already does:

| BiDi module | Commands and events | `bare-drive` source |
| --- | --- | --- |
| `session` | `new`, `end`, `status`, `subscribe`, `unsubscribe` | session code |
| `browsingContext` | `create`, `close`, `navigate`, `reload`, `getTree`, `locateNodes`, `captureScreenshot`, `handleUserPrompt`, load and navigation events | `run.rs`, `locate.rs`, screenshot steps |
| `script` | `evaluate`, `callFunction`, `getRealms`, `disown` | `eval` |
| `input` | `performActions`, `releaseActions`, `setFiles` | the driver input path |
| `network` | request and response events | `network` tool |
| `log` | `entryAdded` | `console` tool |
| `storage` | `getCookies`, `setCookie`, `deleteCookies` | `state.rs`, `storage` tool |

The `bare:` extension module carries what BiDi has no command for:

- `bare:snapshot`: the text snapshot with element refs.
- `bare:find`: the locator syntax (`label="Email"`, `role=button name="Save"`).
- `bare:act`: one action with an `expect` check and the outcome contract (`action_status`, `page_status`, `verification`).
- `bare:steps` and `bare:suite`: step files and suites.
- `bare:saveState` and `bare:restoreState`: login state files.
- `bare:diagnose`.

BiDi allows extension modules with a prefixed name. To verify: the exact rules for extension module names and their commands.

### Hard parts

- `script` needs the BiDi remote value model: serialization, handles, realms and `disown`. The `bare` JS heap must keep handled objects alive.
- `input.performActions` needs action chains with ticks, pointer and key state.
- `locateNodes` needs CSS, XPath, inner text and accessibility locators. XPath is new work.
- Events need subscription and ordering across browsing contexts.

### Transport

BiDi uses WebSocket. `bare` must run a local WebSocket server. To verify: whether BiDi allows a pipe transport, which avoids an open port. Bind to `127.0.0.1` only. Use a random port and a token by default.

### Clients

To verify before the build: which clients work with a non-Firefox BiDi browser today (Puppeteer, Selenium, WebdriverIO, Playwright). Pick one as the first acceptance client. Hypothesis: Puppeteer is the most complete.

### Process model

Today each session is one process. BiDi has many browsing contexts in one browser. Two choices:

1. One process for each BiDi session. Contexts share one heap and one network stack. Lower memory per tab.
2. One process for each top-level context, with one front process. Isolation is better. Memory is higher.

Measure peak memory and time to ready for both on the public journeys before choosing.

## Performance and resources

- The BiDi front end must add no work when no client subscribes to an event. Events are built only for subscribers.
- JSON over a local socket costs per message. Hypothesis: the cost is small next to page work. Measure it: instructions and wall time per command on the public journeys, at least 3 runs, idle machine for wall time.
- `offstage` adds one hop for each tool call. Measure it the same way.
- Compare with the current `bare-mcp` path on the same journeys. Record the numbers in `docs/progress.md`.

## Release gate

`offstage` takes `latest`, so a `bare` release reaches all agents at once. The release job must block a bad release:

1. Build the candidate `bare` binaries.
2. Run the `offstage` browser tests against them. Run the `deck` browser tests through `offstage`.
3. Publish the release as `latest` only when both pass.

## Plan

### Phase 1: one agent front door (small)

1. `bare`: remove `BARE_MCP_REPORT`, `--workspace` and the `hyprctl` rule. Make the `initialize` text neutral.
2. `offstage`: own the agent loop text, the skill, the issue route and window placement. Move `docs/agent-browser-guide.md` there. Remove `bin/offstage-bare-mcp` if it is dead.
3. `offstage`: add a way to serve only the browser tools, for `deck`. It must accept the `secrets_file` and `state_dir` settings that `deck` sets today, and hide the same arguments.
4. `deck`: start `offstage` instead of `bare-mcp`. Keep its tool filter.
5. `bare` CI: add the release gate.
6. Machines: remove the standalone `bare` registration from Claude and Codex.
7. Fix the `bare` README: users table, issue routes and the version rule.

### Phase 2: BiDi in bare (large)

1. Confirm the transport and the first client (the "to verify" items above).
2. Write the BiDi front end on the `bare-drive` library. Start with `session`, `browsingContext` and `input`. Pass the client's own test cases for those modules.
3. Add `script`, `network`, `log` and `storage`.
4. Add the `bare:` extension module.
5. Measure per the section above.

### Phase 3: offstage on BiDi

1. `offstage`: a Go BiDi client. Map each `browser_*` tool to BiDi or `bare:` commands.
2. Run the release gate against both paths. Switch when both pass.
3. Delete `bare-mcp`.

## Open questions

1. Can `offstage` run headless on a server with no Hyprland, as `deck` needs? The `offstage` README says that headless browsing needs no screen. To verify: start `offstage-mcp` on a host without Hyprland and run a journey.
2. `deck` has an optional VM path (`[vm]`), with a guest desktop. Does `offstage` run inside that VM, or on the host?
3. Which BiDi client is the first acceptance client?
4. Process model for BiDi contexts: decide after measurement.

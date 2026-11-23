# Browser task evaluation

This tool measures whether an agent finishes a browser task through `bare-mcp`. It does not measure a model. It has two modes. They share one task corpus, one set of fixtures and one scorer.

- **Scripted smoke run.** A fixed list of tool calls runs through real stdio MCP. It tests the protocol, the fixtures and the graders. It is labelled `scripted`. It is not an agent result.
- **External agent run.** A real agent runs against a fixture that this tool starts. The tool scores the agent transcript and the fixture record. The tool never calls a model.

Python stdlib only. No LLM client, no credential lookup, no paid run. The plan is `docs/plan-agent-evaluation.md`.

## Commands

```sh
python3 -m unittest discover -s tools/agent-eval/tests   # the unit tests (no browser needed)
python3 tools/agent-eval/run.py --help
python3 tools/agent-eval/run.py list

# Scripted smoke run: all tasks, or one with --task ID (repeatable). --out must be new or empty.
CARGO_BUILD_JOBS=2 cargo build --release -p bare-mcp
python3 tools/agent-eval/run.py smoke --bare-mcp target/release/bare-mcp --out /tmp/eval-smoke [--task upload] [--seed 1] [--perf]

# External agent run, one task at a time
python3 tools/agent-eval/run.py fixture --task upload --out /tmp/eval-ext --seed 1 --deadline 900 \
    --bare-mcp target/release/bare-mcp --runner-name NAME --model MODEL --prompt-file PROMPT --tool-setup "TEXT"
#   prints one JSON line: base_url, prompt, downloads_dir, sessions. Run the agent against base_url with that prompt.
#   Stop with SIGINT/SIGTERM or at the deadline. It then writes /tmp/eval-ext/upload/state.json.
python3 tools/agent-eval/run.py score --task upload --transcript TRANSCRIPT.jsonl --state /tmp/eval-ext/upload/state.json \
    --out /tmp/eval-ext-score [--downloads-dir DIR] [--answer-file F] [--tokens-json F]
python3 tools/agent-eval/run.py report /tmp/eval-smoke   # aggregate every result.json below a folder
```

One example for each task (`--task` values): `login_restore`, `search_extract`, `duplicate_button`, `overlay_delayed`, `nav_stale_ref`, `two_sessions`, `upload`, `download`. For example: `python3 tools/agent-eval/run.py smoke --bare-mcp target/release/bare-mcp --out /tmp/e --task two_sessions`.

Exit status: 0 when all requested tasks pass, 1 when a task fails, 2 for bad usage or an unreadable input. A tool never overwrites a transcript or a result: every file is created exclusively, and a used `--out` is refused.

## Arms

An arm is the snapshot mode that `bare-mcp` uses when a call gives no `mode`. The question: does a smaller outline change agent success? Two arms exist: `interactive` (the default) and `all` (the outline with the text between the controls). A `mode` in a call always wins over the arm.

`bare-mcp` reads the arm from `BARE_MCP_SNAPSHOT_MODE` (`interactive` or `all`) once at start. Any other value means `interactive`. The agent is not told the arm.

For each task and each arm, run the same agent and model at least 3 times (seeds 1, 2 and 3). Use the same prompt and tool setup in every run. `fixture` prints `mcp_env`. Start `bare-mcp` for the agent with that environment.

```sh
for arm in interactive all; do for seed in 1 2 3; do
  python3 tools/agent-eval/run.py fixture --task upload --arm $arm --seed $seed --out /tmp/eval-arms/$arm-$seed \
      --bare-mcp target/release/bare-mcp --runner-name NAME --model MODEL --prompt-file PROMPT --tool-setup "TEXT"
  #   The agent runs against base_url. Its bare-mcp runs with the mcp_env line, for example BARE_MCP_SNAPSHOT_MODE=all.
  python3 tools/agent-eval/run.py score --task upload --transcript T.jsonl --state /tmp/eval-arms/$arm-$seed/upload/state.json \
      --out /tmp/eval-arms-score/$arm-$seed [--tokens-json F]
done; done
python3 tools/agent-eval/run.py report /tmp/eval-arms-score
```

`score` takes the arm from `state.json`. `smoke --arm NAME` runs the scripted sequences with an arm. Put each arm in its own `--out` folder.

`report` prints a table on stderr and `by_arm` in the JSON on stdout. Each row is one task and one arm: runs, success rate, median turns (tool calls) and median total tokens. Tokens come from `--tokens-json` (input plus output). Without it, the tool uses tool-output characters / 4. The row says `estimate` then. A row that mixes both says `mixed`.

A smaller outline is adopted only when its success rate does not fall below the `interactive` arm on the same tasks. A scripted smoke run knows the page. It shows the plumbing and not the effect of an arm.

## Tasks

Each task has a prompt, a fixture page set, an independent grader and a scripted sequence (`tools/agent-eval/tasks.py`). The task corpus version is `tasks.VERSION` (now 2). A change to a prompt, fixture or grader must raise it.

| Task | What a pass needs (read from the fixture record or a saved file) |
|---|---|
| `login_restore` | Exactly 1 successful login and at least 2 authenticated `/account` loads with the same session. The transcript must show repeated `ensure_session` for one saved name or `save_state` followed by `open state=name`. A second login is a repeat. |
| `search_extract` | The answer holds the SKU and the price of the target item and no SKU of another item. The item page was opened. |
| `duplicate_button` | Of 5 identical Approve buttons, only the target was approved, once. |
| `overlay_delayed` | The page dismissed its overlay. One submit with the exact note. The delayed result was delivered. |
| `nav_stale_ref` | Steps 1, 2, 3 loaded in order. The answer holds the completion code. No step skipped. Link order changes on each load. |
| `two_sessions` | Two named sessions remain open at task completion, with two users and one note each posted by the right user. Session lifecycle requests must succeed in the transcript. |
| `upload` | One upload. The received bytes equal the seeded bytes (SHA-256 and length) and the name is kept. |
| `download` | One file in the downloads folder. Its bytes equal the seeded bytes (SHA-256). |

An outcome grader never reads receipt text. Lifecycle checks also use correlated successful tool calls, so repeated reads cannot prove a session restore. A task needs a nonempty tool transcript. A receipt such as `ok` or a page that says "Report accepted" does not pass a task. The unit tests prove this for each task: a wrong button, a second submit, a changed upload byte and an `ok`-only server all fail. The `ok`-only server is `tests/fake_mcp.py`; the smoke command against it completes 0 of 8 tasks.

`search_extract` and `nav_stale_ref` compare the agent's answer text with data made from the seed. They also need the page to have been opened. An answer that holds every SKU fails.

### Fixtures and seeds

`tools/agent-eval/fixture.py` serves pages from `tools/fixtures/agent-eval/` on `127.0.0.1` on a free port. One fixture serves one task run. `Fixture.reset(seed)` or `POST /__admin/reset?seed=N` drops all state and builds the data again. The admin routes need the random `X-Eval-Admin` token. The token is never written to a file.

The seed makes all data: users (`alice.N@example.test`, `bob.N@example.test`, passwords `pw-<hash>`), items, requests, the flow codes, the upload bytes and the download bytes. All of it is synthetic. The record has session ids only as short hashes.

The submit endpoint of `/report` has its own counter. A task fails when the counter is above 1, even when the last page says success.

## Transcript, manifest and result formats (schema v1)

`transcript.jsonl`: one JSON object per line with `v`, `type` and `t` (monotonic seconds since the first event).

- `request`: `id`, `method`, `bytes`, and for `tools/call`: `tool`, `session`, `arguments` (redacted).
- `response`: `id`, `bytes`, `is_error`, `text` (redacted, at most 4000 characters), `text_chars`, `error` when JSON-RPC gave one.
- `final_answer`: `text`. A runner or an agent harness writes it for extraction tasks.
- `note`: a runner event such as a deadline.

The collector matches a reply to its request by `id`, not by order, so late or reordered replies stay correct. `validate()` reports a request with no response, a second or unknown response, a missing field, a wrong schema version and a time that goes back. A transcript with a problem fails the task through the `transcript_valid` check. Request and response IDs must be strings or integers; container IDs are reported as invalid instead of crashing validation.

Redaction: passwords, session ids, any key that names a credential, and the `file` and `secrets_file` arguments are replaced. Do not pass `save_state` with a `file`: it holds live credentials. The data in a run is synthetic.

`manifest.json`: task id and version, fixture seed and base URL, browser (binary path, SHA-256, size, git SHA and where the SHA came from), runner (kind, name, model, prompt SHA-256, tool setup), viewport, timeouts, machine and run id.

`result.json`: `completed`, `checks` (name, ok, detail), `failed_assertions`, `duplicate_actions`, `duplicate_submissions`, `metrics` (tool calls and their split by tool, tool errors, request and response bytes, elapsed time), `resources`, `tokens`. `summary.json` and `results-*.jsonl` give the same fields for a whole run, so two frozen versions can be compared with the same keys.

### Metric provenance

- **Tokens.** Present only when the caller supplies `--tokens-json` (`input_tokens`, `output_tokens`). Otherwise `available: false` with a reason. The tool never estimates tokens from bytes.
- **CPU and peak RSS.** Of the one `bare-mcp` process that a task owns. CPU is user plus system time from `wait4`. Peak RSS is `VmHWM` read from `/proc` before the process stops. The `wait4` `ru_maxrss` is not used: a forked child starts with the parent RSS in that field (a child that only sleeps reported 216944 KiB when the parent held 200 MiB). A smoke run starts one process per task, so the process-wide figure is also the per-task figure. `summary.json` sums CPU and takes the maximum RSS over tasks and says so.
- **Instructions.** With `--perf`: `perf stat -e instructions:u` around the process. The CPU time then also holds the small cost of the `perf` process. Without it the value is `null` with `instructions_reason`.
- **Missing values** are `null` with a reason. They are never 0.
- An external run: CPU and RSS are `null` ("the runner of the agent owns the process"). Supply them from outside.

## Observed results

### Original corpus version 1

The following measurements describe the original version 1 corpus. They are historical results, not measurements of the revised lifecycle checks.

Frozen input. Same inputs for every run:

- Base commit `00e701b089a9abe9a399bbe69150c5d772b5c4cc` (main when assigned). No change to Rust code in this branch.
- Binary: `cargo build --release -p bare-mcp` with `CARGO_BUILD_JOBS=2` in a checkout-local `target`, copied to a frozen path. SHA-256 `7c075e2052246ace93bffb568b019eed8e2206610bbe1ad3c0baa8f630fef221`.
- Seed 1, viewport 1280x800, call timeout 20 s, `BARE_SEED=1`, `--perf`. Command: `python3 tools/agent-eval/run.py smoke --bare-mcp FROZEN --out DIR --perf --browser-sha 00e701b0…`.
- Machine: AMD Ryzen 9 5900HX, 16 logical CPUs, 31 GiB, Linux 7.2.15-zen1. The machine was busy (load average 42 at the start). Trust instruction counts. Do not trust wall time or CPU ms from these runs.
- 3 runs, one after the other. This is a small sample.

Outcome: **all 8 tasks completed in all 3 runs** (`completed 8/8`, 0 failed assertions, 0 duplicate actions). Tool calls and response bytes were the same in every run (they differ by at most 1 byte between runs because the port number changes length).

| Task | Tool calls | Response bytes | Instructions min (M) | max (M) | spread | Peak RSS (KiB) |
|---|---|---|---|---|---|---|
| login_restore | 3 | 2313 | 33.74 | 33.76 | 0.07% | 18892–18988 |
| search_extract | 4 | 1127 | 22.45 | 22.47 | 0.07% | 18700–18908 |
| duplicate_button | 3 | 872 | 27.93 | 27.94 | 0.05% | 18536–18924 |
| overlay_delayed | 6 | 1123 | 19.96 | 20.00 | 0.19% | 18456–18744 |
| nav_stale_ref | 8 | 2093 | 54.15 | 54.16 | 0.02% | 25960–26140 |
| two_sessions | 14 | 2473 | 71.48 | 71.50 | 0.02% | 19832–20164 |
| upload | 4 | 955 | 19.92 | 19.93 | 0.02% | 18496–18752 |
| download | 4 | 1204 | 11.95 | 11.96 | 0.07% | 18392–18604 |

`nav_stale_ref` made 1 tool error on purpose: the script uses a ref from the old page after a navigation. The browser refused it with `stale_ref`, and the script went on with a new snapshot.

What this shows: the fixtures, the graders and the stdio protocol path work on this binary, and the scripted calls reach a correct end state on all 8 tasks. What it does not show:

- It is **not** a model-agent result. A scripted sequence knows the page. It does not show that an agent finds the way.
- It is **not** a performance claim. There is no before and after. The instruction counts are a baseline for later runs of the same scripts only.
- No browser gap was found, so no task is marked unsupported here. A task the browser cannot do stays in the corpus as a scored failure.

### Reviewed corpus version 2

The parent review added five regression tests. Before the correction, three lifecycle checks failed their regressions and a container request ID crashed validation. Version 2 requires a nonempty transcript, restore-specific requests, and both named sessions to remain open. The two-session smoke sequence leaves both pages open until process cleanup.

Validation after correction: 62 Python tests passed. All eight scripted tasks passed in each of three runs, with zero duplicate actions. The runs used the same checkout-local release binary from `00e701b089a9abe9a399bbe69150c5d772b5c4cc`, seed 1 and viewport 1280x800. No instruction counter was requested for these runs. They establish task correctness on this binary, not a performance improvement or real-agent success. The two-session script now makes 12 tool calls rather than the version 1 count of 14.

## Limits and unsupported features

- No real-agent run was made. Real-agent results stay unverified until a run on the current harness records the prompt, the model and the tool setup (`--runner-name`, `--model`, `--prompt-file`, `--tool-setup`). `result.json` has `agent_recorded: false` and a note until all four are given.
- Scripted sequences use locators that a script author knows (a CSS row id for `duplicate_button`). A real agent must find them from the snapshot.
- The fixture covers: forms by POST, cookies, redirects, a timer-driven overlay and fetch polling, multipart upload and `Content-Disposition` download. It does not cover: HTTPS, range or split downloads, iframes, dialogs, long-running pages, or a saved-state file. `login_restore` uses `ensure_session` in memory only.
- `fixture` waits for a signal or its deadline. It is a foreground command and not a daemon.
- Other findings, not changed here: `tools/bench/journeys.py` reads peak RSS from `wait4`. That value may include the parent RSS (see above). The journey benchmark was not changed.

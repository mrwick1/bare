## 2026-11-21: journey triage runner (fast plan, phase 2)

`tools/journeys/triage.sh [-e ENGINE] [-j N] [-o OUT.tsv] [-g REGEX] [-b BIN_DIR] [-T SECS]` runs the 50 census workloads on one engine (`BARE_JS_EXEC=ENGINE`). The workloads are the 48 public journeys (`tools/drive/public/*.steps`), the typing page (`type_load DIR '#a1' 50`) and an optional staging row. Each row has status, class, wall time, peak RSS (`wait4`), exit code and the first error line. A second file, `OUT.queue.tsv`, has one line per distinct cause with the number of workloads it blocks, largest first. Classes: `unsupported:<name>`, `syntax`, `type_error`, `timeout`, `crash`, `not_ready`, `other`. The header of the script has the details. Extra files: `triage.py` (the code).

The staging row runs only when `STAGING_BASE`, `STAGING_QUERY` and `STAGING_SECRETS` are set. Otherwise it is `skip` with that message. The script does not read the vault.

### `auto` run (archMachine, 2026-11-21, main 8875c60a, release build, one run, `-j 1`, load average about 16 from other agents)

45 pass, 4 fail, 1 skip (staging). The 48 public journeys: 45 pass, 4 fail (3 public, see below; the 4th failure is `nextjs-docs`; the typing page passes, 2,199 ms).

| Journey | Error (first line) | Note |
| --- | --- | --- |
| `curl-se-home` | `expected css="a[href='/docs/']" visible: a.sitenav-overview is hidden` | hypothesis: the live site changed |
| `duckduckgo-html-search` | `document.querySelectorAll('.result a.result__a').length > 3` is false | hypothesis: the live site changed or blocks the client |
| `lobsters-comments-back` | `expected 1 of css="a.u-url", found 4` | hypothesis: the live site changed |
| `nextjs-docs` | `Unsupported("checkVisibility with content-visibility or display: contents")` | message of the browser host, not of js2; the click on the link does not run |

Each failed twice (the runner retries once). None of the four steps files has a `known-bug` mark. Not checked against an older binary, so "live site changed" is a hypothesis.

### `new` run (same machine, `-j 4`)

All 49 rows (48 journeys and the typing page) are `not_ready` at 3 to 11 ms, no hang; the queue has one cause, `not_ready`, 49 workloads. Staging is `skip`.

Not tested: the staging row (no credentials in this setup), `-e tree` and `-e byte`, a js2 run (it cannot run scripts yet), more than one run, wall time at an idle machine. Classes `syntax`, `type_error` and `crash` are tested on no real failure; the patterns come from the message forms in `crates/js2` (`Abort::Unsupported(name)`, `not supported yet: ...`).

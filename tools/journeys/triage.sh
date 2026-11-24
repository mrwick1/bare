#!/usr/bin/env bash
# Run every daily journey on one JavaScript engine and write a failure queue.
#   tools/journeys/triage.sh [-e ENGINE] [-j N] [-o OUT.tsv] [-g REGEX] [-b BIN_DIR] [-T SECS]
#
# Workloads (the 50 of the feature census, docs/census/2026-11-18.tsv, plus an optional staging row):
#   public:<name>  the 48 steps files of tools/drive/public, run with bare-drive on the live sites (needs the network).
#   typing         `type_load DIR '#a1' 50` (crates/bare/examples/type_load.rs) on the React typing page of tools/js/typing/build.sh.
#                  DIR is $TRIAGE_TYPING_DIR (default /data/bare-tmp/triage/typing-page); it is built there when it is missing.
#   staging        tools/drive/staging-journey.steps. It needs a live login, so it runs only when all three are set:
#                  STAGING_BASE=https://HOST  STAGING_QUERY=TEXT  STAGING_SECRETS=FILE (lines EMAIL= and PASSWORD=).
#                  Otherwise the row is `skip` with the reason. The script never reads the vault.
# Options:
#   -e ENGINE  value of BARE_JS_EXEC: auto (default), tree, byte, or vex (alias new). `new` stops with "cannot run scripts yet" until Vex is wired in.
#   -j N       workloads in parallel (default 1). Instruction counts hold under load; wall time and peak RSS hold only at -j 1 on an idle machine.
#   -o FILE    result table (default triage.tsv). The queue goes to the same name with `.queue.tsv` instead of `.tsv`.
#   -g REGEX   run only the workloads whose name matches (python `re.search`; names: public:<journey>, typing, staging).
#   -b DIR     a release directory with bare-drive and examples/type_load (default ${CARGO_TARGET_DIR:-target}/release). Build it with
#              cargo build --release -p bare-drive --bin bare-drive --example type_load -p bare
#   -T SECS    kill a workload after SECS seconds (default 300); the row is class `timeout`.
# Result columns: workload, status (pass|fail|skip), class, wall_ms, peak_rss_kib, exit, error (first error line).
#   class: unsupported:<name> (Vex Abort::Unsupported(name), "not supported yet: ..."), syntax, type_error, timeout, crash (signal or panic),
#          not_ready (BARE_JS_EXEC=new before Vex is wired in), other. A failed public journey runs once more (network flake);
#          the second result is kept. Not-ready and unsupported rows are deterministic and do not rerun.
# Queue columns: cause, blocks (workloads that fail with it), workloads. Sorted by blocks, largest first: this is the work queue.
#   cause is the class plus the normalized first error line for syntax, type_error and other (digits and quoted text are removed).
# Exit code: 0 when no workload failed, 1 otherwise (skips do not count).
exec python3 -I "$(dirname "$(readlink -f "$0")")/triage.py" "$@"

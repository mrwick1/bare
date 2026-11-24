# 2026-11-24 — Item 10: OLD against js2 on react-docs and substack-home

This is a measurement. It changes no engine or browser code. It answers `docs/next-work.md` item 10: is js2 a real loser on `react-docs` (1.74x OLD) and `substack-home` (6,992 M)?

## Result in short

- No. In every controlled mode js2 uses fewer instructions than OLD on both journeys. Ratio js2/OLD: 0.76 to 0.89 on react-docs, 0.80 to 0.81 on substack-home.
- The item 10 numbers (1.74x, 6,992 M) do not repeat on current main. I cannot trace them to one cause (see "Where the item 10 numbers came from").
- The js2 subsystem profiles say "Network + TLS + HTTP" is 21% (react-docs) and 61% (substack-home). That is a classifier artefact, not network cost: `tools/bench/classify.py` has no rule for `bare_js2::`.
- Item 10 is not a js2 regression. Next step: fix the classifier, then re-profile.

## Inputs

- Source: main at `1c6ec577` (includes #990, the dispatch-event panic fix). Release `bare-drive` of one build for both engines, with symbols and frame pointers: `CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1 RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release --locked -p bare-drive`. Frame pointers change codegen. The earlier plain build gave counts within 0.1% to 0.3% of this one on substack-home tape runs (6,038 against 6,033 OLD; 4,818 against 4,817 js2).
- Host: archMachine (16 threads), shared with other agents. Instruction counts hold under load. Shares and sample counts do not.
- Env for every run: `BARE_SEED=1 LC_ALL=C GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. OLD is `BARE_JS_EXEC=auto`, js2 is `BARE_JS_EXEC=new`.
- Modes: `live` (real network); `tape` (`BARE_NET_REPLAY` of the tape in `/data/bare-tmp/i10/tapes/<journey>` + `BARE_CLOCK=virtual`); `tapenc` (replay, real clock).
- Counts: `perf stat -x, -e instructions:u`, 3 runs per cell. Script: `/data/bare-tmp/i10b/stat.sh`. Log: `/data/bare-tmp/i10b/stat3.log`. JSON and stderr: `/data/bare-tmp/i10b/out/`.
- Profiles: `tools/bench/subsystem_profile.sh run --runs 3 --replay <tape>`, with `BARE_CLOCK=virtual`. Reports: `/data/bare-tmp/i10b/prof-{react-docs-new,substack-home-auto,substack-home-new}/report.txt`. The react-docs OLD profile is the earlier agent's (`/data/bare-tmp/i10/prof-react-docs-auto/report.txt`): OLD did not change since, and its replay did not panic.
- Earlier data (build of main at about `7d02af28`, plain release): `/data/bare-tmp/i10/stat.log`, `stat2.log`.

## Instruction counts (M, instructions:u)

Cell: min to max of 3 runs. Row "earlier" is the earlier agent's build (`stat.log`, `stat2.log`).

| Journey | Mode | OLD | js2 | js2/OLD |
|---|---|---:|---:|---:|
| react-docs | tape | 6,852.2 to 6,852.5 | 5,184.8 to 5,186.2 | 0.76 |
| react-docs | tape, earlier | 5,204.2 to 5,204.5 (partial tape, see below); 6,860.1 to 6,881.7 | 3,676.6 to 3,677.3, rc=1 (panic, fixed by #990) | |
| react-docs | tapenc | 2,199.8 to 2,200.0 | 1,945.7 to 1,946.6 | 0.88 |
| react-docs | tapenc, earlier | 2,201.8 to 2,201.9 | 1,942.7 to 1,946.9 | |
| react-docs | live | 1,713 to 1,963 | 1,553 to 1,754 | 0.92 (means) |
| react-docs | live, earlier | 1,588 to 1,778 | 1,492 to 1,516 | |
| substack-home | tape | 6,033.2 to 6,033.5 | 4,817.2 to 4,819.3 | 0.80 |
| substack-home | tape, earlier | 6,037.3 to 6,038.3 | 4,815.1 to 4,818.2 | |
| substack-home | tapenc | 5,818.7 to 5,821.7 | 4,688.7 to 4,692.3 | 0.81 |
| substack-home | tapenc, earlier | 5,816.2 to 5,826.4 | 4,687.9 to 4,690.4 | |
| substack-home | live | 7,037 to 7,112 | 5,707 to 5,776 | 0.81 |
| substack-home | live, earlier | 5,727 to 5,986 | 4,527 to 5,076 | |

All 36 new runs passed (rc=0), also react-docs tape on js2. Tape spread is at most 0.04% (react-docs) and 0.05% (substack-home) over 3 runs. Live spread is 13% to 25% on one engine and one journey.

Reply counts (length of the `network` list in the JSON report, 3 runs):

| Journey | Mode | OLD | js2 |
|---|---|---|---|
| react-docs | tape | 12, 12, 12 | 12, 12, 12 |
| react-docs | tapenc | 25, 25, 25 | 25, 25, 25 |
| react-docs | live | 17, 11, 12 | 10, 9, 12 |
| substack-home | tape | 141 x3 | 141, 142, 141 |
| substack-home | tapenc | 145 x3 | 145 x3 |
| substack-home | live | 143 x3 | 143 x3 |

## Where the item 10 numbers came from

I could not reproduce 1.74x or 6,992 M in any mode, so this part is partly hypothesis.

- Racy stop point (verified in the data). The replies of react-docs depend on the mode: 12 with the virtual clock, 25 with the real clock on replay, 9 to 17 live. The instruction count follows: 6,852 M, 2,200 M and 1,713 to 1,963 M for OLD. A ratio of one run per engine at different stop points can have any value. `next-work.md` item 10 itself says OLD had 22 replies against 142 for js2 on substack-home. Here OLD gets 143 live, 141 on tape.
- Live noise (verified). Substack-home live OLD is 5,727 M to 5,986 M in the earlier runs and 7,037 M to 7,112 M now: 6,992 M lies inside the live range of OLD. It is not a js2-only value.
- Old build (hypothesis). The item 10 numbers came before #990 and the clone fix. I did not rebuild that commit.
- The react-docs "tape" cell of the earlier OLD run (5,204 M in `stat.log`, 6,860 to 6,882 M in `stat2.log`) is explained by the tape, not the engine. `stat.log` was written at 22:37:01. Two tape files of `/data/bare-tmp/i10/tapes/react-docs` have mtime 22:37:12, later than that. Hypothesis: the first runs used a tape that the recorder had not finished, so the replay ended earlier. The js2 rc=1 of the same file also ran on that tape, but #990 fixes a real panic: on the complete tape js2 now passes.
- The ratio 1.74x is not found in any pair of cells above (largest js2/OLD is 1.0).

## Subsystem shares (percent of samples, mean of 3 runs, range in brackets)

OLD, react-docs (1,005 to 1,970 samples per run), OLD substack-home (2,860 to 2,925), js2 react-docs (2,224 to 2,435), js2 substack-home (2,063 to 2,084). All are under 5,000 samples, so shares are rough. The OLD react-docs run 1 has twice the samples of runs 2 and 3: the host was shared. Compare shares, not sample counts.

| Subsystem | OLD react-docs | js2 react-docs | OLD substack-home | js2 substack-home |
|---|---:|---:|---:|---:|
| JS engine (without parse, collector) | 46.4 [43.1 to 48.3] | 0 (no rule) | 53.9 [53.4 to 54.9] | 0 (no rule) |
| Parsing (HTML, JS parse and compile) | 18.0 [16.4 to 20.3] | 2.5 [2.4 to 2.6] | 17.8 [16.9 to 18.5] | 1.2 [1.1 to 1.5] |
| Style | 14.4 [12.7 to 16.1] | 5.9 [5.6 to 6.3] | 4.9 [4.8 to 5.1] | 7.1 [6.9 to 7.4] |
| Network + TLS + HTTP | 6.7 [5.9 to 7.4] | 21.4 [20.9 to 21.9] | 4.4 [3.9 to 4.8] | 60.6 [60.5 to 60.7] |
| Images | 6.4 [5.8 to 6.9] | 3.0 [2.8 to 3.1] | 9.0 [8.7 to 9.2] | 12.7 [12.5 to 12.9] |
| Layout + text | 2.9 [2.2 to 3.6] | 1.3 [1.2 to 1.4] | 0.6 [0.5 to 0.7] | 1.0 [0.8 to 1.1] |
| Driver | 1.9 [1.6 to 2.1] | 0.8 [0.7 to 0.9] | 2.8 [2.7 to 3.0] | 2.1 [1.9 to 2.3] |
| DOM + Web APIs | 1.8 [1.6 to 1.9] | 64.7 [63.8 to 65.3] | 0 | 14.6 [14.5 to 14.8] |
| Collector (any frame) | 0 | 0 | 5.4 [5.3 to 5.5] | 0 |
| Other | 1.6 [1.0 to 2.2] | 0.5 [0.4 to 0.5] | 0.6 [0.6 to 0.7] | 0.8 [0.7 to 0.8] |

The js2 columns are not comparable with the OLD columns. See the next section.

## Why "Network + TLS + HTTP" is 21% to 61% on a js2 replay

- Verified in the code: `tools/bench/classify.py` rule `('JS engine', r'^<?bare_js::')` matches OLD symbols only. `bare_js2::` does not match (`bare_js::` needs `::` right after `js`). Neither `classify.py` nor `jscat.py` contains the string `js2`.
- Consequence (derived from the rule "owner by first own frame from the leaf"): a js2 sample has no JS owner. It goes to the nearest classified frame up the stack, which is the browser binding or the loader that called into js2. The JS time of js2 is spread over "DOM + Web APIs" and "Network + TLS + HTTP".
- On OLD the same journeys show Network at 4% to 7%. Hypothesis: the js2 Network share is mostly JS run from the script loader and the fetch and XHR callbacks (`bare::resources`, `browser_fetch`, `browser_xhr`), not network work. I did not verify this with call stacks: the perf data was not kept (`--keep` was not used).
- The "JS engine by kind of work" tables of the js2 reports show 70% to 79% "JS engine, all kinds". I did not check how `jscat.py` classifies js2 frames, so I do not use them.
- So the js2 profiles cannot say where js2 spends its time, and the shares of the two engines cannot be compared. The instruction counts above can.

## Conclusion

- Item 10 is not a real js2 regression on current main. Under tape and tapenc modes (stable, 0.05% spread) js2 uses 12% to 24% fewer instructions than OLD on both journeys.
- Live comparisons need many runs and a fixed stop point. One run per engine is not enough: the live spread (13% to 25%) is larger than most claimed differences.
- Do next:
  1. Add `bare_js2::` rules to `classify.py` and `jscat.py`, then re-run these four profiles with `--keep` (a separate PR, not part of this measurement). Then both engines have a comparable "JS engine" row, and the Network share on js2 can be checked.
  2. Close item 10 in `docs/next-work.md` (the owner's session edits it; I did not).
  3. Keep `docs/progress/2026-11-23-js2-reply-diffs-and-tapes.md` as the record for the substack-home difference of two `substackcdn.com` face images.

## Not tested

- The commit of the item 10 numbers (no rebuild). The cause of 1.74x and 6,992 M is open.
- Plain release build (no frame pointers) on current main: all new counts are from the profiling build.
- Call stacks of the js2 "Network" samples (perf data not kept).
- Other journeys, other machines, Windows, test262, cost rows.
- OLD react-docs profile on the current build (reused from the earlier agent).

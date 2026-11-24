# 2026-11-24 — OLD against js2 on crates-io-home and mdn-reference

This is a measurement. It changes no engine or browser code. It follows `docs/next-work.md` item 8 ("profile crates-io-home and mdn-reference on tapes") and copies the method of `2026-11-24-js2-react-substack-profile.md`.

## Result in short

- `mdn-reference`: js2 is not a loser. js2/OLD is 0.81 to 0.85 in all three modes. The subsystem profile shows js2 uses fewer samples in JS and about the same in Style and Layout.
- `crates-io-home`: js2 is a loser in two of three modes. js2/OLD is 2.10 (tape), 1.81 (live, mean) and 1.03 (tapenc). Reply counts are equal for both engines in each mode.
- The `crates-io-home` journey stops at the first visible `body` (steps: `open`, `expect css="body" visible`). Its stop point is racy: OLD does 282 M on tape and live, but 514 M on tapenc. js2 does 593 M on tape and 529 M on tapenc. So the 2.1x is a difference in how much work each engine does before the stop, and the full-work cost is about equal (tapenc 1.03). This is a hypothesis; I did not verify what runs before the stop.
- Next step: give `crates-io-home` a deterministic stop point, then compare again (see "Next").

## Inputs

- Source: main at `9e541650` (includes #992, the js2 rules of the classifiers). Release `bare-drive`, one build for both engines: `CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1 RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release --locked -p bare-drive`.
- Host: archMachine, shared with other agents. Instruction counts hold under load. Shares and sample counts do not.
- Env for every run: `BARE_SEED=1 LC_ALL=C GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. OLD is `BARE_JS_EXEC=auto`, js2 is `BARE_JS_EXEC=new`.
- Tapes: one live OLD run per journey with `BARE_NET_RECORD`, in `/data/bare-tmp/i13/tapes/{crates-io-home,mdn-reference}` (55 and 118 files). Both recording runs passed. No replay missed a request, so I did not need `BARE_NET_FILL`.
- Modes: `live`; `tape` (`BARE_NET_REPLAY` + `BARE_CLOCK=virtual`); `tapenc` (replay, real clock).
- Counts: `perf stat -x, -e instructions:u`, 3 runs per cell. Script `/data/bare-tmp/i13/stat.sh`, log `stat3.log`, JSON and stderr in `out/`.
- Profiles: `tools/bench/subsystem_profile.sh run --runs 3 --replay <tape>` with `BARE_CLOCK=virtual`. Reports: `/data/bare-tmp/i13/prof-<journey>-{auto,new}/report.txt`.

## Instruction counts (M, instructions:u)

Cell: min to max of 3 runs. All 36 runs passed (rc=0, `ok: true`, no panic in any stderr).

| Journey | Mode | OLD | js2 | js2/OLD |
|---|---|---:|---:|---:|
| crates-io-home | tape | 281.9 to 282.3 | 593.2 to 593.4 | 2.10 |
| crates-io-home | tapenc | 513.5 to 514.0 | 528.7 to 529.5 | 1.03 |
| crates-io-home | live | 277.4 to 287.3 | 436.6 to 561.8 | 1.81 (means); 1.52 to 2.03 (min to max) |
| mdn-reference | tape | 7,845.7 to 7,846.2 | 6,682.0 to 6,683.2 | 0.85 |
| mdn-reference | tapenc | 7,501.8 to 7,502.7 | 6,204.3 to 6,214.4 | 0.83 |
| mdn-reference | live | 6,079.4 to 6,102.2 | 4,949.2 to 4,952.1 | 0.81 |

Tape spread is at most 0.1%. Live js2 on crates-io-home spreads 29% over 3 runs.

Reply counts (length of `network`, 3 runs):

| Journey | Mode | OLD | js2 |
|---|---|---|---|
| crates-io-home | tape | 41 x3 | 41 x3 |
| crates-io-home | tapenc | 41, 41, 40 | 41 x3 |
| crates-io-home | live | 39 x3 | 39 x3 |
| mdn-reference | tape | 91 x3 | 91 x3 |
| mdn-reference | tapenc | 92 x3 | 92 x3 |
| mdn-reference | live | 75, 77, 75 | 75 x3 |

## Subsystem shares (percent of samples, mean of 3 runs, tape, virtual clock)

Samples per run: crates-io-home OLD 170 to 179, js2 345 to 349; mdn-reference OLD 3,506 to 3,582, js2 2,546 to 2,605. All are under 5,000, so shares are rough, and crates-io-home is very rough (170 to 349 samples).

| Subsystem | crates OLD | crates js2 | mdn OLD | mdn js2 |
|---|---:|---:|---:|---:|
| JS engine (run) | 43.8 | 41.9 | 51.0 | 37.5 |
| Parsing (HTML, JS parse and compile) | 18.9 | 25.9 | 15.6 | 20.4 |
| Network + TLS + HTTP | 17.5 | 12.7 | 3.7 | 5.1 |
| Style | 5.9 | 8.9 | 11.6 | 15.9 |
| DOM + Web APIs | 4.4 | 4.2 | 2.8 | 3.7 |
| Layout + text | 0.6 | 1.3 | 8.6 | 11.9 |
| Collector (any frame) | 0 | 0.8 | 4.9 | 4.5 |
| Driver | 2.9 | 0.9 | 1.3 | 0.4 |
| Other | 5.6 | 3.0 | 0.3 | 0.4 |
| Images | 0.4 | 0.5 | 0.2 | 0.3 |

With #992 the js2 "Network" share is now 5% to 13%, near OLD. The artefact of the earlier study is gone.

Shares to samples (share x mean samples per run), to see where the extra js2 cost on crates-io-home is. Hypothesis: the sample ratio (about 2.0) matches the instruction ratio (2.10) on this tape.

| Subsystem | OLD samples | js2 samples | js2 minus OLD |
|---|---:|---:|---:|
| JS execute | 76 | 145 | +69 |
| JS parse + compile (JS split row) | 31 | 80 | +49 |
| Style | 10 | 31 | +21 |
| Network + TLS + HTTP | 30 | 44 | +14 |
| DOM + Web APIs | 8 | 15 | +7 |

On mdn-reference, Style is 412 samples (OLD) against 408 (js2). Layout is 305 against 305. JS is the whole gain: js2 uses fewer samples in JS execute.

## Top js2 kinds on crates-io-home (jscat, percent of all samples)

"Parse and compile" 15.4, "Built-ins (natives)" 14.0, "Other JS" 13.0, "Allocation" 11.0, "Property get and set" 8.5, "Strings and atoms" 1.8, "Calls" 1.4. OLD: "Parse and compile" 18.4, "Allocation" 13.2, "Property get and set" 8.8, "Bytecode loop" 6.3, "Calls" 4.2, "Built-ins" 1.4, "Other JS" 3.3.

In samples: js2 "Built-ins" is about 49 against about 2 for OLD, and "Other JS" about 45 against about 6. Hypothesis: the js2 interpreter loop is not a jscat kind ("Bytecode loop" is 0.1% for js2), so its dispatch lands in "Other JS". The jump of "Built-ins" is not explained; I did not look at call stacks (`--keep` not used).

## Cause on crates-io-home (hypothesis)

- Verified: replies are equal, the journey passes, and the full-work count (tapenc) is within 3%.
- Verified: OLD tape (282 M) and OLD tapenc (514 M) differ by 1.8x, so the stop point of the journey moves the count more than the engine does.
- Hypothesis: with the virtual clock js2 runs more JS (execute, parse and compile) and more style recalculation before the `body` check passes than OLD does. I did not find which scripts run.
- The step file says the app does not mount (known bug). I did not check whether js2 mounts it. If js2 runs more of the app than OLD, the extra cost is real work, not a slower engine.

## Conclusion

- `mdn-reference`: not a loser. js2 is 15% to 19% cheaper than OLD in every mode.
- `crates-io-home`: js2 is above OLD by more than 5% on tape and live, but the cause is open and the journey has a racy stop point. Not proof of a slow engine, and not proof of none. In the one mode where both engines do the full work (tapenc) the ratio is 1.03.
- This differs from the react-docs and substack-home result, where js2 was cheaper in every mode.

## Next

1. Give `crates-io-home` a stop point that does not depend on ticks, for example wait for the `/api/v1/summary` request or for a rendered element. Then repeat tape and tapenc. A change to the steps file belongs in a separate PR.
2. If the 2x remains, record with `--keep` and read the call stacks of the js2 "Built-ins" and "Other JS" samples. Also compare the script list that each engine runs before the stop.
3. Do not block item 8 on `mdn-reference`. Decide on `crates-io-home` after step 1.

## Not tested

- What scripts and phases run before the stop on each engine (no call stacks, `--keep` not used).
- Whether js2 mounts the crates.io app and OLD does not.
- A plain release build (no frame pointers); all counts are from the profiling build.
- Other journeys, other machines, Windows, test262, cost rows.
- Live runs: 3 runs per cell is too few for crates-io-home js2 (29% spread).

# 2026-11-24 — crates-io-home: the app mounts; the journey stopped too early

This changes only the journey `tools/drive/public/crates-io-home.steps`. It changes no engine or browser code.

## Result in short

- The "app does not mount" bug did not reproduce. On both engines the app requests `/api/v1/summary` and renders the crate lists, when the journey waits for them.
- The old journey ended at `expect css="body" visible`. `body` is visible before the app starts. The journey stopped before the app requested `/api/v1/summary`. So the report showed no request and the `<noscript>` text was still in the DOM.
- The site is not Ember now. It is SvelteKit: an inline script does `import(".../entry/start.*.js").then(kit => kit.init(...); import(".../entry/app.*.js").then(app => kit.start(app, element)))`.
- Steps 2 to 4 (find a cause, failing test, fix) did not apply: nothing was broken. I wrote no test and changed no engine code.
- The journey now waits for `a[href^='/crates/']` (nth=0) to be visible: a crate link from the "Just Updated" or "Most Downloaded" lists.

## Inputs

- Source: main at `7d3fd679`. Release `bare-drive` built with `CARGO_TARGET_DIR=/data/bare-tmp/cio/target`. Host archMachine.
- Env: `BARE_SEED=1 LC_ALL=C`. OLD is `BARE_JS_EXEC=auto`, js2 is `BARE_JS_EXEC=new`. Live runs, `perf stat -x, -e instructions:u`.

## Live runs of the new journey (3 per engine, all `ok: true`)

| Engine | Replies | `/api/v1/summary` | Instructions (M) | Wall ms |
|---|---:|---:|---|---|
| OLD | 53 | 1 | 544.0, 544.9, 545.2 | 505 to 1067 |
| js2 | 52 | 1 | 557.5, 582.3, 590.2 | 498 to 1032 |

js2/OLD is about 1.06 to 1.08. The earlier 2.1x ratio came from the early stop (see `2026-11-24-js2-crates-mdn-profile.md`).

## Seen, not fixed

- OLD reports a console exception in some page runs: `listener of microtask: ReferenceError: Cannot access 'r' before initialization` (stack in `script#70`, `script#69`, `script#67`, `script#66`). It appears when the run waits longer than the first `body`. The app still mounts and the journey passes. js2 reports no console error. This may be an OLD temporal-dead-zone bug. I did not investigate it.
- Wall time varies 2x between runs with the same instructions. I did not look into it.

## Not tested

- Replay and tape modes: a replay never requests `/api/v1/summary`, so the new step cannot pass on the old tape. A new tape is needed.
- The cost matrix and the OLD exception. No unit test, no `t` run (no code changed).

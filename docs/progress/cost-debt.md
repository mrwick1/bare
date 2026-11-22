# Cost debt

Rows that merged over the cost rule (CLAUDE.md, "Cost gate"). One line per row. A fix PR or a full matrix recheck can mark a row fixed.

| Row | PR | Change (glibc) | Change at cgu=1 | Hypothesis | State |
| --- | --- | --- | --- | --- | --- |
| `browser_load event 1` | #849 | +1.197% | +0.891% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `browser_load event 10` | #849 | +1.361% | +1.011% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `browser_load lookup 1` | #849 | +1.408% | +1.361% | Selector subject loop and wrapper call. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `browser_load lookup 10` | #849 | +1.475% | +1.426% | Selector subject loop and wrapper call. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `focus_load dispatch 1` | #849 | +0.856% | +0.740% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `focus_load dispatch 10` | #849 | +0.977% | +0.854% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `focus_load focus 1` | #849 | +2.134% | +1.495% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `focus_load focus 10` | #849 | +2.092% | +1.377% | Listener probe and dispatch. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `observer_load mo 1` | #849 | +1.527% | +0.680% | Out-of-line DOM helpers. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `observer_load mo 10` | #849 | +1.759% | +0.786% | Out-of-line DOM helpers. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `preload_load script 10` | #849 | +0.943% | +0.569% | Duplicate old DOM installation code and relocations. | [Fixed](2026-11-21-cost-debt-849.md#849-rows). |
| `interp_load loopfn 3000000` | #876 | +2.780% | +1.483% | The iterator branches can change register allocation in the shared dispatch loop. | [Fixed](2026-11-22-full-cost-matrix.md#debt-from-earlier-prs). |
| `interp_load calls 1000000` | #876 | +1.175% | +1.098% | The iterator branches can change register allocation in the shared dispatch loop. | Accepted by the owner on 2026-11-22. #931 measured +0.55% after 2 fix attempts (stack and branch layout). |
| `raw_collect 10000 1000 10` | #864 | +1.361% | +1.231% glibc, +1.121% jemalloc | The count visits unmarked cells and skips free slots after marking. | Accepted by the owner on 2026-11-22. #931 measured +1.39%. The adaptive GC (#911) collects during setup by design. |
| `script_run startup (var x = 1;)` | #901 | +1.085% | +1.085% | Additional prototype shapes and metadata increase realm setup. | [Fixed](2026-11-22-full-cost-matrix.md#debt-from-earlier-prs). |
| `interp_load fib 27` | #921 | +0.895% | +0.895% | Hot dispatch layout changes the cost of existing operations. | [Fixed](2026-11-22-full-cost-matrix.md#debt-from-earlier-prs). |
| `interp_load calls 1000000` | #922 | +0.532% | +0.532% | Hot dispatch layout adds instructions to function-local calls. | [Fixed](2026-11-22-full-cost-matrix.md#debt-from-earlier-prs). |
| `session_load chain 1` | #851 / #854 | +1.424% | +1.410% glibc, +0.838% jemalloc | The page loop adds an engine branch and stores the browser in AnyBrowser. | [Open](2026-11-22-full-cost-matrix.md#bisect-of-session_load-chain-1). |

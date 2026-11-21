# Cost debt

Rows that merged over the cost rule (CLAUDE.md, "Cost gate"). One line per row. A fix PR marks the row fixed.

| Row | PR | Change (glibc) | Change at cgu=1 | Hypothesis | State |
| --- | --- | --- | --- | --- | --- |
| `dataset_load keys 1000 50` | #841 | +2.480% | +1.955% | Each named fill opens a scope. | Open after recheck against `8c9e784b`. |
| `dataset_load keys 10000 50` | #841 | +2.565% | +2.024% | Each named fill opens a scope. | Open after recheck against `8c9e784b`. |
| `dataset_load read 10000 50` | #841 | +0.744% | +0.789% | Each named fill opens a scope. | Open after recheck against `8c9e784b`. |
| `dataset_load write 10000 1` | #841 | +1.356% | +1.276% | Each named fill opens a scope. | Open after recheck against `8c9e784b`. |
| `dataset_load write 10000 50` | #841 | +1.006% | +0.958% | Each named fill opens a scope. | Open after recheck against `8c9e784b`. |
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
| `form_load gates 1` | To verify | +2.802% | +2.667% | Cause to verify. | Open against `1303414b`. |
| `form_load gates 10` | To verify | +2.813% | +2.672% | Cause to verify. | Open against `1303414b`. |
| `form_load gc 1` | To verify | +2.549% | +2.767% | Cause to verify. | Open against `1303414b`. |
| `form_load gc 10` | To verify | +2.653% | +2.890% | Cause to verify. | Open against `1303414b`. |
| `nodelist_load scan 10` | To verify | +0.738% | +0.588% | Cause to verify. | Open against `1303414b`. |

# Cost debt

Rows that merged over the cost rule (CLAUDE.md, "Cost gate"). One line per row. A fix PR marks the row fixed.

| Row | PR | Change (glibc) | Change at cgu=1 | Hypothesis | State |
| --- | --- | --- | --- | --- | --- |
| `dataset_load keys 10000 50` and 4 more `dataset_load` rows | #841 | +0.6% to +2.2% | +0.56% to +1.99% | Each fill opens a scope (about 135 instructions). | open (re-check after #851) |
| `browser_load event 1/10`, `lookup 1/10`, `focus_load dispatch 1/10`, `focus 1/10`, `observer_load mo 1/10`, `preload_load script 10` | #849 | +0.57% to +1.5% | over +0.5% | Less inlining in generic code (`Dom::query_s`, listener find in `Dom::fire`). | fix in progress |

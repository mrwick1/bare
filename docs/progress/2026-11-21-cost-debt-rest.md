# 2026-11-21: Remaining browser cost debt

Compare the dataset rows with `8c9e784b` (before #841). Compare the form and NodeList rows with `1303414b` (before #849). Use #869 at `1534525e` for the before column. Use `f2c07640` for the fix. That tree includes main at `d4777ff0`.

## Cause and change

- Dataset keys and reads. `Object.keys` checks each named property again to decide if it is enumerable. After #841, each long-list check enters the scope bridge. At 10,000 operations and 50 attributes, 520,000 fills add 39.52 M self instructions in `api_fill` and 22.88 M in the scoped host callback. The old callback used 17.68 M. Keep the charges. Convert a short ASCII property name once per lookup. Find its qualified attribute by name, without converting each candidate.
- Dataset writes. The scoped named write replaced the raw-key callback. The adapter and scoped host callback use 2.80 M self instructions per 10,000 writes. The old callback used 0.13 M. Convert short ASCII names on the stack. This removes the temporary property string and the heap allocation for the attribute name. Convert the value before the name checks, as before.
- Form gates. At scale 10, the code for `owned_controls` and its 1.50 M calls to `next_in` takes 27.24 M more self instructions combined. The form attribute helper moves out of its callers. It uses 10.17 M self instructions. Resolve the form atom once per owner lookup and compare atoms during the walk. Resolve the disabled attribute atom once and keep the same charges for walking ancestors.
- Form GC. The queries for each button use 28.17 M more instructions at scale 10. `query_s` makes 1.65 M calls to the out-of-line tree walk. For a selector with only an ID, compare the attribute directly. Keep tree order, the root exclusion, the result limit and both step charges.
- NodeList scan. `prepare` uses 1.33 M more self instructions at scale 10. A fresh list by name still went through `prepare` to check the revision a second time. Use the same fresh-list check for every source. A change still triggers the charged rebuild.

## Measurement

Use default release builds on archMachine (Ryzen 9 5900HX, Rust 1.96.0). Use the minimum of three `instructions:u` counts from `tools/js/cost_matrix.sh -J` for each binary and allocator. Use symbolized release builds and `callgrind_annotate --tree=caller` for the profile comparisons.

Each cell gives the change from the historical base under glibc / jemalloc.

| Row | Before | After |
| --- | --- | --- |
| `dataset_load keys 1000 50` | +2.480% / +2.387% | -38.775% / -37.936% |
| `dataset_load keys 10000 50` | +2.565% / +2.550% | -40.899% / -40.717% |
| `dataset_load read 10000 50` | +0.744% / +0.706% | -28.637% / -27.591% |
| `dataset_load write 10000 1` | +1.356% / +1.240% | -6.251% / -9.157% |
| `dataset_load write 10000 50` | +1.006% / +0.924% | -5.462% / -7.855% |
| `form_load gates 1` | +2.803% / +2.563% | -3.396% / -3.082% |
| `form_load gates 10` | +2.813% / +2.734% | -3.694% / -3.384% |
| `form_load gc 1` | +2.550% / +2.400% | -8.855% / -8.302% |
| `form_load gc 10` | +2.654% / +2.588% | -9.324% / -9.168% |
| `nodelist_load scan 10` | +0.738% / +0.722% | -0.802% / -0.816% |

The default release build passes all 28 rows in the dataset, form, NodeList and selector lookup recheck. These include the ten debt rows. The mean change is -11.786% / -12.010% for the 12 dataset rows and -1.938% / -1.886% for the other 16 rows. The largest spread within three samples is 0.272%. The median spread is 0.002% in both comparisons.

## Tests

- `cargo check --release --locked -p bare` passed.
- The dataset, NodeList, form and selector tests passed. The js2 browser query smoke test passed.
- Windows, the full workspace suite and the full cost matrix were outside this run.

# Intl.PluralRules in Vex (2026-11-25)

## What changed
- `Intl.PluralRules` is a lazy constructor in `crates/vex/src/bi_intl.rs`, like the other Intl constructors.
- It has `select`, `resolvedOptions` and `supportedLocalesOf`. A call without `new` throws TypeError.
- It reads `localeMatcher`, `type` and the digit options through `number_options::digits_only`. That function shares its defaults with `initialize`.
- It uses the ICU4X plural data and `icu::Cache::plural` that bare-intl already had.
- `resolvedOptions` matches Node 22 (V8) for key order. V8 reports significant digits for every rounding type except plain fraction digits. Vex does the same.

## Tests
- New file `crates/vex/src/bi_intl_plural_tests.rs`: 3 tests. Expected strings come from Node v22.23.2. `t` passes.

## test262
- Not run. `tools/js/test262_js2.sh` builds `bare-js-diff --example test262_run`. Commit 3693dabb deleted that crate. No runner exists for Vex.
- Before and after counts for `intl402/PluralRules` (15 files in the checkout) are not available.

## Cost
- No row of `cost_rows.txt` exercises `Intl` on Vex. The only one, `web_apis_load segmenter`, fails on origin/main too (`Intl.Segmenter.segment` is missing).
- Substitute: `script_run` on 2000 rounds of `new Intl.NumberFormat(...).format()` plus `new Intl.Collator().compare()`. Instructions (`instr.sh`, min of 3), origin/main 5bbe6ccc against this branch, one machine (archMachine):
  - glibc: 69459070 -> 69479745 (+0.03%)
  - jemalloc: 69906367 -> 69938460 (+0.05%)

## Not done or not tested
- `selectRange`: Node returns "other" for every English range, including (2, 1). The category of the end value does not give that. Skipped.
- `roundingIncrement` other than 1 throws RangeError (gate), as for NumberFormat.
- Locales other than the baked English ones.

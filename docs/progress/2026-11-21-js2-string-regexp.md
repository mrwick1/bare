# 2026-11-21: String methods with a RegExp, and the URI functions, on js2 (journeys on `BARE_JS_EXEC=new`)

Branch `t3/js2-regexp-string-support`, stacked on `t3/generic-engine-js2-impl` (PR #851, which is stacked on #849). Neither is on main yet.
Previous entries: [PR 6c-2](2026-11-21-s04-pr6c2-js2-browser.md), design [S4 RegExp](../design-js2-regexp.md) (step R3, the String side).

## Causes

The journey run `tools/journeys/triage.sh -e new` on #851 gave 43 pass and 6 fail. Three of the fails were `public:rust-book-search`, `public:python-org-docs-link` and `public:python-docs-search-results`.

1. **The String methods did not use the RegExp runtime.** The runtime of step R1 (`regexp_rt.rs`: literal, constructor, `exec`, `test`) was complete, and the patterns of the pages compile. But `String.prototype.match`, `matchAll` and `search` were stubs that threw `match and matchAll need a RegExp` and `search needs a RegExp`. `replace` and `replaceAll` threw `a RegExp pattern is not supported` for any object pattern, and `split` called `ToString` on a RegExp and split on its source text. It was not a pattern feature and not the host realm: every pattern on the three pages compiles. This is step S4.R3 of the design (the String side).
2. **`encodeURIComponent` did not exist on js2.** After 1 was fixed, `rust-book-search` still failed: `searcher.js` of mdBook calls `encodeURIComponent(searchterms.join(' '))` for each result. The call threw `ReferenceError`, the handler stopped before it added a `li`, and the page had a header and no results. The old engine has `encodeURI`, `encodeURIComponent`, `decodeURI` and `decodeURIComponent` (`crates/js/src/eval_native.rs`); js2 had none of the four. This is outside the RegExp scope of the task. It is needed to pass the journey, so it is a separate commit.

The exact calls (counted with a temporary log of each compile and each method call, removed before the commit; `bare-drive` on the live sites, one run each):

| Journey | `replace` with a RegExp (function replacer) | `replace` with a string | `match` | `split` with a RegExp | `search` | Patterns compiled |
|---|---|---|---|---|---|---|
| `rust-book-search` | 1,642 (33) | 20 | 13 | 4 | 0 | 39 |
| `python-org-docs-link` | 140 (55) | 6 | 0 | 24 | 1 | 62 |
| `python-docs-search-results` | 1 (0) | 115 | 1 | 3 | 0 | 4 |

`python-docs-search-results` (searchtools.js): `/\s+/`, `/[^\p{Letter}\p{Number}_\p{Emoji_Presentation}]+/gu`, `/^\d+$/`, `/[.*+\-?^${}()|[\]\\]/g`.

`rust-book-search` (mdBook `searcher.js`, elasticlunr and its Porter stemmer, `highlight.js`): the 39 patterns are `/[\s\-]+/`, `/\s+/`, `/[&<>'"]/g` with a function replacer, `/'/g`, `/&/g`, `/"/g`, `/</g`, `/>/g`, `/\/([^/?#]+)$/i`, `/^\?/`, `/^([^/])/`, `/\.html$/`, `/\blang(?:uage)?-([\w-]+)\b/i`, `/^(no-?highlight)$/i`, `/^(?:[a-z+]+:)?\/\//`, `/(^(<[^>]+>|\t|)+|\n)/gm`, `/\t/g`, `/\W+$/`, `/^\W+/`, and the stemmer rules `/^(.+?)(ss|i)es$/`, `/^(.+?)([^s])s$/`, `/^(.+?)(ed|ing)$/`, `/^(.+?)e$/`, `/^(.+?)eed$/`, `/(at|bl|iz)$/`, `/([^aeiouylsz])\1$/`, `/^(.+?[^aeiou])y$/`, `/^(.+?)(s|t)(ion)$/`, `/ll$/`, `/.$/` and eight longer rules with `^([^aeiou][^aeiouy]*)?[aeiouy]...`.

`python-org-docs-link` (jQuery 1.x on python.org): 62 patterns, for example `/^[\x20\t\r\n\f]+|((?:^|[^\\])(?:\\.)*)[\x20\t\r\n\f]+$/g` (Sizzle), `/\\(?!\\)/g`, `/-([\da-z])/gi` with a function replacer, `/([A-Z])/g`, `/<(?!area|br|col|embed|hr|img|input|link|meta|param)(([\w:]+)[^>]*)\/>/gi`, `/(?:\{[\s\S]*\}|\[[\s\S]*\])$/`, `/(=)\?(?=&|$)|\?\?/` and `/^<(\w+)\s*\/?>(?:<\/\1>|)$/`.

## What changed

- New `bi_string_regexp.rs`: `match`, `matchAll`, `search`, `replace`, `replaceAll` and the RegExp branch of `split`. They run the R1 runtime (`regexp_run`, `run_at`) in a loop with no property read. `replace` and `replaceAll` now do the string pattern and the function replacer in Rust (a nested call through `call_function`) and the replacement text with `$$`, `$&`, `` $` ``, `$'`, `$n` and `$nn`. The prelude function `replaceWith` and its slots (`replaceWith`, `nativeReplace`, `nativeReplaceAll`) are gone; `prelude_facts.rs` is made again.
- `split` with a RegExp does not run a sticky copy at each index (the spec text). It searches forward from `q` and applies the same rule for an empty match at `p`. A sticky RegExp is tried at `q` only.
- `regexp_rt.rs`: `regexp_run` is split in two: `run_at` (the matcher call and the budget and limit handling, no `lastIndex`) and the `lastIndex` rules. New small helpers for the String side (`spans`, `regexp_flags_of`, `regexp_for`, `regexp_clone`, `last_index_value`, `put_last_index`). `program`, `last_index`, `substring` and `match_result` are `pub(crate)` and `#[inline(always)]`: a second caller made the compiler stop inlining them, and that cost 1.5% of the `test` row (see Cost). `compile_program` is the cold part of `program`.
- New `bi_uri.rs`: `encodeURI`, `encodeURIComponent`, `decodeURI`, `decodeURIComponent` as lazy entries of the global table. The code is that of `crates/js/src/value.rs`. A lone surrogate and a bad escape throw `URIError`. `escape` and `unescape` (Annex B) are not done.
- Tests: `bi_string_regexp_tests.rs` (6 tests, 40 cases, with the patterns above) and `bi_uri_tests.rs` (1 test, 6 cases), each normally and under constant collection. `builtins_more_tests.rs`: the case that asserted that `'a'.match('a')` throws now expects only the three `call(undefined)` errors.

## Known gaps (not done)

- Only a regexp object (`F_REGEXP`) takes the fast path. A script that gives a RegExp its own `exec` or `Symbol.replace`, or changes `RegExp.prototype`, is not seen. The generic path of the design (section 7.5) is not built. js2 has no `Symbol.match`, `replace`, `search`, `split` or `matchAll`.
- No named groups: `$<name>` stays literal, a replacer gets no `groups` argument, and `exec` gives `groups` as `undefined`.
- `matchAll` runs all the matches at the call and returns an array iterator. The tag is `Array Iterator`, not `RegExp String Iterator`. A script that stops early still pays for all the matches.
- `escape` and `unescape`.

## Results

Journeys (`tools/journeys/triage.sh -e new -j 3`, live sites, archMachine, one run):

| Run | pass | fail |
|---|---|---|
| Base #851 (from the task) | 43 | 6 |
| The 3 journeys, this branch | 3 | 0 |
| All journeys, this branch | 46 | 3 (`public:curl-se-home`, `public:duckduckgo-html-search`, `public:lobsters-comments-back`) |

The 3 that still fail also fail on the old engine ([journey triage](2026-11-21-journey-triage.md): the live site changed). `staging` is skipped (no login set).

Tests: `cargo test --locked -p bare-js2 --lib` 749 passed; `cargo test --locked -p bare-js-api` 32 + 1 + 5 passed. The 7 new tests fail on the base tree (6 of the 7 fail; the one that passes, `replace_with_a_string_pattern`, covers behavior that did not change).

test262 (`TEST262=/data/claude-tmp/test262 tools/js/test262_js2.sh -g 'RegExp|built-ins/String'`, js2 column against `tools/fixtures/js/test262-js2-baseline.txt`): no directory lost a pass. 1,941 to 2,078 passes in these directories (+137):

| Directory | js2 pass before | js2 pass after | js2 fail after |
|---|---|---|---|
| `annexB/built-ins/RegExp` | 6 | 8 | 2 |
| `built-ins/RegExp` | 389 | 394 | 6 |
| `built-ins/RegExp/lookBehind` | 2 | 17 | 0 |
| `built-ins/String/prototype/match` | 8 | 39 | 1 |
| `built-ins/String/prototype/replace` | 12 | 47 | 0 |
| `built-ins/String/prototype/replaceAll` | 0 | 1 | 0 |
| `built-ins/String/prototype/search` | 9 | 32 | 0 |
| `built-ins/String/prototype/split` | 84 | 109 | 3 |

The baseline file is that of the commit `e4fbf577`; I did not re-run main js2 for these directories. The URI directories (`built-ins/decodeURI*`, `encodeURI*`) are not in `test262-js2-subset.txt`, so they were not run.

Cost (instructions in user mode, minimum of 3 runs, `perf stat -e instructions:u` with the glibc tunable of `tools/js/instr.sh`, archMachine with other sessions running; base = `140c61a1` (#851), new = this branch; `script_run` rows are scripts I wrote, listed below). The `interp_load` micro rows and the startup row follow the rule: +0.5% under the default build, else within +0.5% at `RUSTFLAGS="-C codegen-units=1"` on both trees.

| Row | base (default) | new (default) | change | base (cgu=1) | new (cgu=1) | change |
|---|---|---|---|---|---|---|
| startup (`script_run` of `var x = 1;`) | 720,229 | 721,641 | +0.20% | 715,381 | 715,487 | +0.01% |
| `interp_load loopfn 3000000` | 875,298,858 | 875,299,691 | +0.00% | 851,284,884 | 851,285,438 | +0.00% |
| `interp_load calls 1000000` | 589,355,296 | 589,357,005 | +0.00% | 566,333,725 | 566,334,863 | +0.00% |
| `interp_load fib 27` | 260,427,486 | 260,429,185 | +0.00% | 250,241,017 | 250,241,704 | +0.00% |
| `interp_load closures 1000000` | 1,965,206,169 | 1,965,209,117 | +0.00% | 1,919,225,081 | 1,919,223,208 | -0.00% |
| `interp_load objects 1000000` | 712,132,263 | 712,130,120 | -0.00% | 713,109,669 | 713,109,266 | -0.00% |
| regexp `test`/`exec` loop (20,000 turns x 5 patterns and 1 `exec`) | 455,914,716 | 454,585,277 | -0.29% | 461,021,672 | 459,750,955 | -0.28% |
| string loop (20,000 turns of `replace`, `replaceAll`, `split` with strings, `replace` with a function) | 394,008,636 | 278,369,398 | -29.35% | 372,185,873 | 256,705,711 | -31.0% |

The first build of the split of `regexp_run` gave +1.58% (default) and +1.53% (cgu=1) on the `test` row. callgrind (symbols kept) showed `program`, `last_index`, `substring` and `match_result` as out-of-line functions: they have a second caller now. `#[inline(always)]` on them closed it. The matcher itself (`regexp.rs`) is not changed, so the `regexp_row` rows (the engine alone) were not re-run. Scripts for the two loop rows:

- regexp: 8 words, 5 stemmer patterns (`/^(.+?)(ss|i)es$/`, `/^(.+?)(ed|ing)$/`, `/^(.+?)e$/`, `/(at|bl|iz)$/`, `/^([^aeiou][^aeiouy]*)?[aeiouy]/`), 20,000 turns: `rs[j].test(w)` for the 5 patterns and `rs[1].exec(w)`.
- string: `s = 'a.b.c,d.e'`; 20,000 turns of `s.replace('.', '-')`, `s.replaceAll('.', '-')`, `s.split(',')`, `s.replace('.', f)`.

Not measured: peak memory and the `replace` row of the design (10,000 matches, function, `g`) against the old engine; the cost of `matchAll` on a large input; binary size; Windows.

## Not tested

- Windows (no Windows VM run).
- A script-defined `exec`, `Symbol.replace` subclass or a modified `RegExp.prototype` (the fast path ignores them).
- The URI directories of test262 and `escape`/`unescape`.
- The 3 journeys on more than one run each, and on a loaded network.

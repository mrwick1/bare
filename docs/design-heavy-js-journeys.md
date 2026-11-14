# Design: where the heavy JS journeys spend their instructions (task R2-A)

Status: proposal. The owner reviews this document before any code is built. No product code changed. Written on 2026-11-14 against main `28a8fbc2` (later main commits changed docs only for this question).

Journeys: `react-docs`, `rust-book-search` and `nodejs-org-home`. Together with the two Wikipedia journeys (task R2-B, [design-wikipedia-journeys.md](design-wikipedia-journeys.md)) they are about 57% of the journey instructions. They moved -0.8% in round 1.

Rules for numbers: "measured" means I ran it. "Estimate" means I computed it from measured numbers, and the arithmetic is shown. "Hypothesis" means I believe it and did not check it. "Prototype" means a temporary patch, kept outside the repository in `/data/bare-tmp/r2/a/`.

## 0. Summary

1. **`react-docs` has no lever outside the replay driver loop.** It costs 16,951 M instructions with the `settle_pictures` loop and 127 to 138 M without it (99.2% to 99.3% loop). There is nothing to design. The loop fix that the owner ordered is the whole gain. Section 3.
2. **`rust-book-search` (16,322 M on main) has two kinds of avoidable work, both measured:**
   - `for-in` walks the whole prototype chain of every array and object: 6.1 M prototype keys are examined for 0.34 M own keys. 18.0% of the journey.
   - The regex engine runs the same inputs again and again: 987,649 `RegExp.test` calls, but only 49,109 different (regex, input) pairs. 95% of the executions repeat an earlier one. The engine and its wrapper are 31% of the journey.
   - Sections 4 and 5.
3. **`nodejs-org-home` (5,921 M) is flat evaluator work.** The tree walker is 75%. The best local levers are worth 2% to 3%. Section 6.
4. **The loop does not change these numbers.** `rust-book-search` costs 16,322 M with and without the loop. `nodejs-org-home` costs 5,921 M both ways. The designs for them need no re-baseline.
5. **Recommendation** (section 8): D1 (`for-in` stops at a prototype tail that has no enumerable key) and D2 (a regex result table, with a lazy DFA for `test()` as the next step if the table does not hold on a live site). Prototype of both together: `rust-book-search` 16,322 M to 9,861 M (-39.6%, 1.68x against the 16,607 M baseline). Criterion 4 needs 12,774 M or less. Criterion 2: 90.1% to about 84.0% from these two alone.

## 1. Method and inputs

- Machine: archMachine, shared. Main `28a8fbc2`. Corpus: `/data/bare-tmp/corpus/virtual`, `BARE_NET_REPLAY`, `BARE_CLOCK=virtual`, `BARE_SEED=1`. Driver: `crates/drive` `bare-drive` with the public steps of `tools/drive/public/<journey>.steps`.
- Instruction counts: `perf stat -e instructions:u` through `tools/bench/journeys.sh compare --clock virtual`. Instruction counts do not depend on the load of the machine.
- Subsystem split: `perf record -e instructions:u` on a build with symbols and `-C force-frame-pointers=yes` (`--call-graph fp`). Each stack goes to one subsystem by the first frame that matches (script `classify.py`). One run per journey for the split (the shares move by 0.5 points or less between runs in the other tasks of this round; not re-checked here).
- JS-level split: a counting profiler (prototype `r2prof.rs`, `prof.patch`). It counts instructions by JS function, by native builtin and by regex program, with counters for `for-in` and regex input repetition. The profiler adds overhead (about 3x on the heavy runs). Use its shares and counts, not its absolute numbers.
- Loop split: a prototype (`nosettle.patch`) turns `settle_pictures` off. `crates/drive` is not changed in the repository.
- Run counts: totals of `rust-book-search` and `nodejs-org-home` come from 3 runs each (spread 0.0%: the instruction count is deterministic on the virtual clock). `react-docs` without the loop: 127 to 138 M over 4 runs. One `journeys.sh compare` run gave 4,321 M for it with the loop off (not explained, section 3).

## 2. Measured split

Shares of the instructions of one profile run (samples of `instructions:u`).

| Subsystem | `rust-book-search` (16.7 G sampled) | `nodejs-org-home` (6.1 G) | `react-docs` (17.6 G, with the loop) |
|---|---:|---:|---:|
| Evaluator (tree walker) | 32.3% | 75.0% | 72.9% |
| Regex engine | 25.8% | 0.3% | 0.1% |
| `for-in` key enumeration | 18.0% | 1.6% | 0.4% |
| JS builtins (`call_ext`, `call_native`, `call_binary`) | 9.3% | 2.3% | 3.0% |
| Regex wrapper (`re_exec_raw`, result, `lastIndex`) | 5.2% | 0.1% | 0.0% |
| GC | 3.3% | 3.6% | 2.5% |
| JSON.parse | 2.6% | 0.9% | 0.1% |
| JS parse (lex, parse, walk) | 1.6% | 6.6% | 3.0% |
| CSS | 0.2% | 2.6% | 4.8% |
| Image decode | 0 | 0 | 7.0% |
| Rest (layout, net, HTML, DOM bindings, regex compile) | 1.0% | 7.1% | 6.7% |

The five largest shares of `rust-book-search`: evaluator 32.3%, regex engine 25.8%, `for-in` 18.0%, builtins 9.3%, regex wrapper 5.2%.

Hypothesis: a live run has the same order, because the page code (`searcher.js`, `elasticlunr.js`) is the same. Not measured.

### 2.1 `rust-book-search` by JS function

The journey types a query into the mdBook search box. The page code runs `formatSearchResult` for each of 180 results. Each result calls `makeTeaser`.

| JS function (counts from the profiler) | Calls | Instructions inside (share of the profiled total) |
|---|---:|---:|
| `makeTeaser` (`searcher.js`) | 180 | 91% including its callees; 33% in its own body |
| the Porter stemmer function of `elasticlunr.js` (`function(n){var i,o,r,s,u,a,l;if(n.length<3)...`) | 109,398 | 52% including the regexes it calls; 12% in its own body |
| `nat:Ext(ReTest)` (`RegExp.prototype.test`) | 987,649 | 38% including the engine |
| `nat:ArrPush` | 224,981 | 4% |
| `gc` (`Interp::collect`) | 2 passes | 3% |
| `JSON.parse` of the 4.7 MB index | 8 | 2.5% |
| `String.prototype.startsWith` / `substr` | 109,323 / 85,209 | about 0.5% each |

`makeTeaser` loops over the words of each result with `for (var i in array)`: 113,523 `for-in` loops, 113,165 of them over arrays. The stemmer runs about 20 regexes per word.

### 2.2 Operations counted

- `for-in`: 113,523 loops. 340,317 own keys examined. 6,115,194 prototype keys examined (54 per loop). 227,152 keys kept. The prototype keys are all non-enumerable built-in names of `Array.prototype` and `Object.prototype`. 100% of them are dropped.
- Regex: 1,026,495 executions (987,649 `test`, 38,846 `exec`). 6,223 different input strings. 49,109 different (regex, input) pairs. The repeat share is 1 - 49,109 / 987,649 = 95.0% for `test`. A typical regex has 10 to 158 instructions and is anchored (`^` at the start).
- GC: 2 collects, 0.55 G (3.3%).
- Not counted: property gets by kind, string allocations, array growth by size. See section 10.

## 3. `react-docs` and the replay loop

- With the loop: 16,951 M. Without: 127 to 138 M (4 runs). The split of the sampled run (17.6 G) is page JS in the evaluator (72.9%), image decode (7.0%), CSS (4.8%): all of it runs because the loop ticks the page for up to 5 virtual seconds while a picture is pending.
- Outside the loop the page costs 127 to 138 M. No subsystem in it is worth a design: 1% of the 105 G total.
- My first profile of `react-docs`, and a prototype that decoded raster images lazily (-3.4%), were confounded by the loop. A lazy decode removes work only inside the loop, because a live run does not wait for pictures. **I drop that design.**
- Unexplained: one `journeys.sh compare` run with the loop off gave 4,321 M for `react-docs`, and four single runs gave 127 to 138 M. Hypothesis: a timing miss in the compare run made the replay take the live path of a timer. Not checked.
- Consequence for criteria: `react-docs` is not a lever for criterion 2 after the loop fix. The fix itself is.

## 4. Design for `for-in` (18.0% of `rust-book-search`)

### 4.1 What the code does now

`Interp::for_in_keys` (`crates/js/src/eval.rs`) builds the key list at loop start:

```
seen = FxSet
loop over object o, then its prototype, until null:
    for k in property_keys(o):         // all own keys, in order
        if seen.insert(k) and o_peek(o, k).enumerable(): keep k
    o = o_get_prototype_of(o)
```

Complexity: O(own + chain keys) set inserts and property peeks per loop. For an array, the chain is `Array.prototype` (about 40 keys) and `Object.prototype` (about 12 keys). The peek is a property lookup by key (`Heap::peek_own_property`), the insert is a hash insert. Memory: one set of at most 54 + own keys per loop.

Measured cost: 3.01 G for 6.46 M keys examined, about 470 instructions per prototype key (set insert, peek, key check). Hypothesis: 94% of the 3.01 G is the prototype part (6.12 M of 6.46 M keys). The prototype in section 4.5 saves 2,876 M, which is 95.6% of 3.01 G and agrees.

The rule that makes this correct: a non-enumerable key on a closer object hides an enumerable key of the same name further up the chain. A walk that skips a prototype is only sound when nothing further up can add a key.

### 4.2 Design D1a: a realm-wide certificate for the two built-in prototypes

Data: one `u32` counter `enum_epoch` in `Interp`, and one `u32` `clean_epoch` (the epoch at the last check).

Algorithm:
1. When the walk reaches `Array.prototype` or `Object.prototype` with `clean_epoch == enum_epoch`, stop. The tail has no enumerable key.
2. Any write that can add an enumerable key to one of them, or change its prototype, bumps `enum_epoch`: `define_own_property` and `[[Set]]` creating a key on these two objects, `Object.defineProperty` with `enumerable: true`, `Object.setPrototypeOf`/`__proto__` on them, `Object.assign` and `Object.defineProperties` through the same path.
3. After a bump, the next `for-in` rescans the two objects (about 52 peeks), stores the result and sets `clean_epoch`.

Complexity: O(own) per loop, O(1) check. A rescan costs about 52 x 470 = 24 K instructions, once per bump. Memory: 8 bytes.

Weakness: the writes must be found in every place that can mutate these two objects. A missed place is a wrong `for-in` result. The two objects are marked by a flag bit on the object, and the flag is checked on the define path only. To verify: that all define paths go through one function.

### 4.3 Design D1b: an enumerable count in the shape (chain certificate)

Data: one `u16` count of enumerable own keys on each shape (and on each dictionary-mode object that has no shape). Shapes are few (`Shapes` table); the field costs 2 bytes per shape.

Algorithm:
1. Start from the receiver. After its own keys, look at the rest of the chain. If every remaining object is an ordinary object (no proxy, no exotic index keys) with count 0, stop. Nothing further can be yielded, and the keys of those objects cannot hide anything that can be yielded.
2. Otherwise walk as now.

The count changes where the shape changes (add key, change attributes, delete key), so no invalidation list exists. The check costs one load and compare per chain object (2 objects for an array: about 20 instructions).

Complexity: O(own + chain length). Memory: 2 bytes per shape. Weakness: to verify whether `Array.prototype` is in shape mode or in dictionary mode (`PropMap`). If it is a dictionary, the count lives on the dictionary and changes in its insert, delete and attribute paths.

### 4.4 Design D1c: an enumeration cache on the receiver shape (V8 enum cache)

V8 keeps the enumerable key list of a map (shape) in an enum cache. SpiderMonkey keeps a `NativeIterator` list guarded by the shapes of the receiver and its prototypes. Both skip the whole walk when the guards hold.

Data: per shape, `Rc<[PropertyKey]>` of the enumerable keys of the receiver and the chain, and the `enum_epoch` of D1a or the identity of the chain's shapes. Algorithm: on `for-in`, if the receiver shape and the chain guards match, copy the list (or share the `Rc` when the loop body cannot change the object; the spec allows keys deleted during the loop to be skipped, so the loop still checks each key).

Complexity: O(keys) copy, O(1) when shared. It removes the own-key work too. Memory: one list per shape that is used in a `for-in`. Own-key work is 5% of the 3.01 G (0.34 M of 6.46 M keys; hypothesis by key count), so D1c gains at most 0.15 G more than D1a or D1b on this journey. Arrays have no shape-specific own keys (the keys are indices), so for arrays the list depends on the length and cannot be shared. I rank D1c below D1b.

### 4.5 Prototype and gain

Prototype (`forin.patch`): skips the chain when the next prototype is `Array.prototype` or `Object.prototype` and a one-time scan found no enumerable key. No invalidation (upper bound of D1a and D1b; their check costs are below 5 M: 113.5 K loops x 40 instructions).

| `rust-book-search` | Instructions | Change |
|---|---:|---:|
| main `28a8fbc2` | 16,322 M | |
| with the prototype | 13,446 M | -2,876 M (-17.6%) |

Gain on the journey: -2,876 M = 1.235x against the baseline of 16,607 M (13,446 / 16,607 = 0.810). Criterion 4 needs 12,774 M or less, so D1 alone is not enough.

Gain on the other journeys: `nodejs-org-home` 0.10 G of `for-in` (1.6%): D1 saves about 0.09 G (hypothesis: the same 94% proto share). `react-docs` 0.07 G with the loop. The other 44 journeys: not measured.

### 4.6 Recommendation for `for-in`

D1b if `Array.prototype` is in shape mode, otherwise D1a. Test cases to write before the code: an enumerable key added to `Array.prototype` before and during the loop; `Object.defineProperty(Object.prototype, 'x', { enumerable: true })`; a non-enumerable own key that hides an enumerable key on a prototype; `Object.setPrototypeOf` on the receiver; a proxy and a typed array in the chain; a key deleted during the loop.

## 5. Design for the regex work (31% of `rust-book-search`)

### 5.1 What the code does now

`RegExp.prototype.test` and `exec` call `re_exec_raw`, then `run_engine` (`crates/js/src/eval_builtins.rs`), then `Regex::exec_in` (`crates/js/src/regexp.rs`). The engine is a backtracking VM: a program of instructions, a `Frame` stack, and `charge` for each step against a step budget (`regexp_steps`, `regexp_input_units`, `regexp_stack_frames`). Complexity: O(n) for simple cases, exponential for nested quantifiers. The budget is the guard against the exponential case: **a limit that protects a slow algorithm**.

Measured: 4.32 G in the engine (25.8%), 0.76 G + 0.12 G in the wrapper (5.2%). 1,026,495 executions: about 4.2 K instructions in the engine per execution and 0.9 K in the wrapper. The hot regexes (anchored, groups 1 to 3, 10 to 158 instructions) are suffix and prefix rules of the Porter stemmer, for example `^(.+?)(ss|i)es$`. Lazy `.+?` before an alternation of suffixes tries each split of the word: O(n x suffixes) steps for a word of n units. 96.2% of the executions are `test()`: the page needs a boolean and no captures.

### 5.2 Design D2a: a result table (memo)

Data: a direct-mapped table of 4,096 entries per realm. An entry holds: the regex program id (`Regex` pointer or counter), the `lastIndex` start (`from`), the input string identity, and the result (`Option<Match>`, captures as offsets) plus the steps the run took. Strings are immutable in JS, so (program, flags, input, from) is a pure key. The input key is a content hash of the units for inputs of 64 units or fewer. Longer inputs bypass the table.

Algorithm:
1. In `run_engine`, compute the key. Probe one entry.
2. Hit: charge the stored steps to the budget (so a step limit stays deterministic), return the stored result.
3. Miss: run the engine, store the result and the step count. Collisions replace the old entry.

Complexity: O(len) hashing per call (about 40 to 150 instructions), O(1) probe. Memory: 4,096 x about 64 bytes = 256 KiB per realm at most (hypothesis for the entry size; the captures vector is the large part: store at most 3 groups inline and skip the table for more).

What must stay outside the table: `lastIndex` writes, the result array object, and the legacy static `RegExp.$1` state if the engine has one (to verify). These run on every call, hit or miss.

Weakness: it needs repeated inputs. Here 95% repeat, because the 6,223 words occur in many of the 180 results. A page that never repeats an input gets no gain and pays the hash (below 5%).

Prototype (`remo.patch`): a `HashMap` keyed by (regex pointer, `from`, `Vec<u16>` copy of the input) for inputs of 64 units or fewer; it skips the step charge on a hit. This is an upper bound on the hit side and a lower bound on the cost side: the key copy and `LocalKey::with` cost 11.1% of the remaining profile (about 1.1 G), more than a direct-mapped table needs (hypothesis).

| `rust-book-search` | Instructions | Change |
|---|---:|---:|
| main `28a8fbc2` | 16,322 M | |
| regex memo prototype | 12,751 M | -3,571 M (-21.9%) |

Estimate for the real table: engine 4.32 G x 0.95 hit rate = 4.10 G removed, minus 1.03 M probes x about 150 instructions = 0.15 G: -3.9 G (hypothesis). I take -3.6 G (the prototype) as the low case and -3.9 G as the middle case.

The prototype alone gives 12,751 M against the 12,774 M target: it passes by 23 M (0.2%). That is inside the noise of the method, so D2a alone is not a safe way to meet criterion 4.

### 5.3 Design D2b: a lazy DFA for `test()`

RE2 and the Rust `regex` crate use a lazy DFA when no captures are needed. The page here needs only a boolean in 96.2% of the calls.

Data: per `Regex`, a table of DFA states built on demand. A state is a sorted set of program counters (NFA states). Transition table: `[u32; 256]` per state for Latin-1 input plus a fallback for other units; up to 64 states per regex (then clear the table and rebuild, as RE2 does). Memory: 1 KiB per state, at most 64 KiB per regex.

Algorithm:
1. `test()` on a regex without back-references and without look-behind runs the DFA from `from`. One table lookup per input unit. Anchoring (`^`, `$`) is a flag on the start state and a check at the end.
2. If the DFA says "no match", return false. If it says "match", return true. The step budget is not needed: the cost is O(n) by construction.
3. A regex with a back-reference or look-behind, and every call that needs captures (`exec`, `match`, `replace`), use the backtracker as now.

Complexity: O(n) per call, O(states x 256) once per regex. Gain (hypothesis, not prototyped): about 8 to 20 units per word, one transition per unit at about 10 instructions, plus 100 for setup: 180 to 300 instructions per call instead of 4.2 K. 1.03 M calls x 4.0 K saved = about -4.0 G. The gain does not need repeated inputs and also works on a live page.

Cost: new code of about 400 to 600 lines (state construction, a byte-class table, `u`/`i`/`m`/`s` flags). Risk: semantic differences from the backtracker (lazy quantifiers need no care in a boolean test, but case folding and `unicode` mode need care). The test262 RegExp tests are the check.

### 5.4 Smaller options

- A `test()` fast path in the wrapper: do not build a result array and do not write capture state when the caller needs a boolean. Hypothesis: part of the 0.88 G of the wrapper (0.3 to 0.5 G on this journey). It is a small local change. It combines with D2a and D2b.
- Cheaper backtracker steps (`charge` with `checked_add`, `Frame` read and write: 5% of the journey in total). This is the patch kind that `CLAUDE.md` says to avoid: it makes the same work cheaper, and it does not remove it.
- A literal-suffix prefilter (reject `^(.+?)(ational|...)$` when the input does not end with one of the literals): it covers only this stemmer. Rejected as too narrow.

### 5.5 Recommendation for regex

D2a first: it is a table in one function, and it is exact when the step charge is kept. D2b second, as a separate topic, only if the owner wants the gain on pages without repeats. They are not both needed for criterion 4 when D1 is built.

## 6. `nodejs-org-home` (5,921 M)

75% of the instructions are in the evaluator (4.58 G). The top frames: `eval_expr_inner` 5.6%, `branch<Value, Error>` 5.1% (the `?` operator on a `Res<Value>`), `ic_own` 3.1%, `link_inner` 2.9%. Parse is 6.6%, GC 3.6%, regex compile 1.9% (0.12 G), `for-in` 1.6%.

- No subsystem except the evaluator is above 7%, and the evaluator has no single hot function: it is flat.
- Register VM lane (`BARE_JS_EXEC=byte|auto`): it was measured at near-full coverage in my earlier runs of this task as a 3% to 8% change on the three journeys (raw numbers not kept for all three, so this is not a citable measurement; PR #518 measured -0.07% on the journey total). Not a design for criterion 2.
- Thin error (`Res<Value>` today carries a 32-byte `Error`; the `?` branch copies it): the `branch<Value, Error>` frame is 5.1% on this journey, 5.3% on `react-docs` and 2.2% on `rust-book-search`. A boxed or index-sized `Error` would make `Res<Value>` 16 bytes. Hypothesis: it halves that frame, a 2% to 3% gain on every evaluator-bound journey. I did not measure `size_of::<Res<Value>>()`. It is an invasive change (every `Err` site).
- D1 on this journey: about -0.09 G (1.5%).
- Regex compile 0.12 G: hypothesis that the same regex literal is compiled again for each evaluation of the enclosing function. A compiled-regex cache by (source, flags) would remove it. Not checked.

Estimate for the journey: D1 plus a compile cache plus thin error: 1.5% + 2% + 2.5% = about 6% (hypothesis), 0.35 G. No design here changes a whole kind of work. I recommend no build for this journey in round 2.

## 7. Journey total and criteria

Numbers: baseline 105,189 M, main (corrected after PR #521) 94,796 M = 90.1%. Criterion 2 needs 84,151 M, so -10,645 M from main. Criterion 4 needs 12,774 M for `rust-book-search`.

| Case | `rust-book-search` | Criterion 4 | Journey total | Criterion 2 |
|---|---:|---|---:|---:|
| main | 16,322 M | 1.02x, no | 94,796 M | 90.1% |
| D1 alone | 13,446 M | 1.235x, no | 91,920 M | 87.4% |
| D2a alone (prototype) | 12,751 M | 1.30x, by 23 M | 91,225 M | 86.7% |
| D1 + D2a (prototype) | 9,861 M | 1.68x, yes | 88,335 M | 84.0% |
| D1 + D2a (middle case, 3.9 G regex) | 9,546 M | 1.74x, yes | 88,020 M | 83.7% |

Arithmetic: total = 94,796 - saving. D1 + D2a: 94,796 - 6,462 = 88,334 M (measured on the combined prototype: 6,462 M saved. The two prototypes alone sum to 2,876 + 3,571 = 6,447 M, so they do not overlap; the 15 M difference is noise). 88,334 / 105,189 = 0.840. The middle case: 94,796 - 2,876 - 3,900 = 88,020 M.

With task R2-B (W1 + H + S-A: -5.4 G, an estimate in its document): 88,334 - 5,400 = 82,934 M = 78.8%. That reaches 80%, but it adds two estimates. It also mixes a with-loop total.

The loop: after the loop fix, the baseline is measured again. Both sides lose the loop cost, so the denominator shrinks as well. R2-B measured criterion 2 as 89.3% without the loop (45,037 M on main; the baseline without loop is 45,037 / 0.893 = 50,433 M, a value I derive and did not measure). Then D1 + D2a give (45,037 - 6,462) / 50,433 = 76.5% (hypothesis: the baseline value is derived). `rust-book-search` is not touched by the loop, so criterion 4 is the same in both cases.

## 8. Recommendation

1. **D1b** (D1a if `Array.prototype` is a dictionary): `for-in` skips a prototype tail with no enumerable key. Smallest change that removes a kind of work (prototype key walk of 6.1 M keys). -2.9 G on `rust-book-search`.
2. **D2a**: regex result table with the step charge kept, plus the `test()` wrapper fast path. -3.6 to -3.9 G.
3. Together: `rust-book-search` about 9.6 to 9.9 G (1.68x to 1.74x): criterion 4 met with a margin of 2.9 to 3.2 G. Criterion 2: about 84% from these two (with loop, before the re-baseline), and about 79% with R2-B's estimate added.
4. D2b (lazy DFA) after D2a only if the owner wants it: a larger change and an estimate only.
5. Nothing for `react-docs` (fix the loop) and nothing for `nodejs-org-home` in round 2.

Each build must run the whole browser benchmark again on a frozen commit (`CLAUDE.md`), on the idle machine, 3 runs. The prototype numbers are upper bounds on the hit side and have no invalidation code: they decide the design, not the final gain.

## 9. Where a limit or budget hides a slow algorithm

- Regex step budget (`regexp_steps`, `charge`): it stops the exponential case. It does not remove the O(n x alternatives) steps of a normal stemmer rule. A lazy DFA (D2b) makes the budget unnecessary for `test()`. A memo (D2a) must charge the stored steps on a hit, or the budget would stop being deterministic.
- `for-in` has no limit. It does O(chain) work per loop because the design builds the full key set first. Nothing protects it, and nothing needs to once D1 lands.
- The replay loop `settle_pictures` stops after 5 virtual seconds. The cap hides that a page can cost 16.9 G in a tick loop that a live run does not have.
- The regex memo's bound (inputs of 64 units or fewer, 4,096 entries) is a size bound on a cache, not on an algorithm.

## 10. Not measured

- Property gets by kind (own, prototype, inline-cache hit and miss), string allocations and their bytes, array growth by capacity. The evaluator is 32% to 75% and I did not break it down.
- A live run of any of the three sites. The instruction numbers are replay on the virtual clock.
- The regex step counts per call (only the totals and the instructions).
- The effect of D1 and D2 on the other 44 journeys.
- Wall time and memory (RSS) of the prototypes.
- `size_of::<Res<Value>>()` and the thin-error gain.
- The cause of the 4,321 M `react-docs` compare run.
- D2b is not prototyped. D1 and D2a have no invalidation code and no tests.

Kept outside the repository: `/data/bare-tmp/r2/a/` (patches, profiles, perf data, scripts).
